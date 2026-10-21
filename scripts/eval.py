#!/usr/bin/env python3
"""Capability tests: runs coding tasks through killi, Kimi Code and Claude Code and compares them.

  python3 scripts/eval.py verify                  every task fails as given and passes with its solution
  python3 scripts/eval.py run [--tools killi,kimi,claude] [--tasks a,b] [--repeat N]
  python3 scripts/eval.py report                  docs/eval.md and docs/eval-{light,dark}.svg from the results

Each task in evals/tasks/<name>/ has `task.toml` (kind, prompt, check), `repo/` (the starting
files, committed to a fresh git repo in a temporary folder), `check/` (files restored before the
check runs, so a tool can't pass by editing the tests) and `solution/` (a reference answer).
A run passes when the check command exits 0. Results are appended to evals/results.jsonl.

killi and Kimi Code run EVAL_MODEL (default mimo/mimo-v2.6-pro); Claude Code runs its default
model. Override binaries with KILLI_BIN, KIMI_BIN and CLAUDE_BIN.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TASKS = ROOT / "evals/tasks"
RESULTS = ROOT / "evals/results.jsonl"
RUNS = ROOT / "evals/runs"
MODEL = os.environ.get("EVAL_MODEL", "mimo/mimo-v2.6-pro")
TIMEOUT = 20 * 60
ENV = {k: v for k, v in os.environ.items() if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}


def binary(env_var, name):
    path = os.environ.get(env_var) or shutil.which(name)
    if not path:
        sys.exit(f"{name} not found; set {env_var}")
    return str(Path(path).resolve())


def tasks(names=None):
    found = sorted(p for p in TASKS.iterdir() if (p / "task.toml").exists())
    if names:
        found = [p for p in found if p.name in names]
    return [(p, tomllib.loads((p / "task.toml").read_text())) for p in found]


def prepare(task_dir, overlays=()):
    workdir = Path(tempfile.mkdtemp(prefix=f"eval-{task_dir.name}-"))
    shutil.copytree(task_dir / "repo", workdir, dirs_exist_ok=True)
    git = lambda *args: subprocess.run(["git", *args], cwd=workdir, check=True, capture_output=True)
    git("init", "-q", "-b", "main")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.test", "add", "-A")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.test", "commit", "-qm", "start")
    for overlay in overlays:
        if (task_dir / overlay).exists():
            shutil.copytree(task_dir / overlay, workdir, dirs_exist_ok=True)
    return workdir


def check(task_dir, spec, workdir):
    if (task_dir / "check").exists():
        shutil.copytree(task_dir / "check", workdir, dirs_exist_ok=True)
    proc = subprocess.run(["bash", "-c", spec["check"]], cwd=workdir, capture_output=True, text=True, timeout=900)
    return proc.returncode == 0, (proc.stdout + proc.stderr)[-3000:]


def verify(names):
    ok = True
    for task_dir, spec in tasks(names):
        start_passes, _ = check(task_dir, spec, prepare(task_dir))
        solved, log = check(task_dir, spec, prepare(task_dir, ["solution"]))
        status = "ok" if not start_passes and solved else "BROKEN"
        ok &= status == "ok"
        print(f"{status:7} {task_dir.name}: starts {'passing' if start_passes else 'failing'}, solution {'passes' if solved else 'fails'}")
        if not solved:
            print(log)
    sys.exit(0 if ok else 1)


def killi_metrics(stdout, _workdir, _started):
    report = json.loads(stdout)
    return {
        "requests": report["requests"],
        "input_fresh": report["prompt_tokens"] - report["cached_tokens"],
        "input_cached": report["cached_tokens"],
        "output": report["completion_tokens"],
        "model": MODEL.split("/")[-1],
    }


def kimi_metrics(_stdout, workdir, started):
    sessions = Path.home() / ".kimi-code/sessions"
    dirs = [d for d in sessions.glob(f"wd_{workdir.name}_*") if d.stat().st_mtime >= started - 5]
    totals = {"requests": 0, "input_fresh": 0, "input_cached": 0, "output": 0}
    for wire in (p for d in dirs for p in d.glob("*/agents/*/wire.jsonl")):
        for line in wire.read_text().splitlines():
            if '"type":"llm.request"' in line:
                totals["requests"] += 1
            elif '"type":"usage.record"' in line:
                usage = json.loads(line)["usage"]
                totals["input_fresh"] += usage.get("inputOther", 0) + usage.get("inputCacheCreation", 0)
                totals["input_cached"] += usage.get("inputCacheRead", 0)
                totals["output"] += usage.get("output", 0)
    totals["model"] = MODEL.split("/")[-1]
    return totals


def claude_metrics(stdout, _workdir, _started):
    report = json.loads(stdout)
    totals = {"requests": report.get("num_turns", 0), "input_fresh": 0, "input_cached": 0, "output": 0}
    for usage in report.get("modelUsage", {}).values():
        totals["input_fresh"] += usage.get("inputTokens", 0) + usage.get("cacheCreationInputTokens", 0)
        totals["input_cached"] += usage.get("cacheReadInputTokens", 0)
        totals["output"] += usage.get("outputTokens", 0)
    totals["model"] = ", ".join(report.get("modelUsage", {})) or "default"
    return totals


def tool_commands():
    return {
        "killi": (lambda p: [binary("KILLI_BIN", "killi"), "--json", "-m", MODEL, "-p", p], killi_metrics),
        # Prompt mode uses the configured default permission mode; ours is "auto" (never asks).
        "kimi": (lambda p: [binary("KIMI_BIN", "kimi"), "-m", MODEL, "-p", p], kimi_metrics),
        "claude": (
            lambda p: [binary("CLAUDE_BIN", "claude"), "-p", p, "--output-format", "json", "--dangerously-skip-permissions"],
            claude_metrics,
        ),
    }


def run_one(tool, task_dir, spec, log_dir):
    command, metrics = tool_commands()[tool]
    workdir = prepare(task_dir)
    started = time.time()
    try:
        proc = subprocess.run(["/usr/bin/time", "-l", *command(spec["prompt"])], cwd=workdir, env=ENV,
                              capture_output=True, text=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL)
        stdout, stderr, timed_out = proc.stdout, proc.stderr, False
    except subprocess.TimeoutExpired as e:
        stdout, stderr, timed_out = (e.stdout or b"").decode(errors="replace"), (e.stderr or b"").decode(errors="replace"), True
    wall = time.time() - started
    footprint = re.search(r"(\d+)\s+peak memory footprint", stderr)
    record = {
        "tool": tool,
        "task": task_dir.name,
        "kind": spec["kind"],
        "when": datetime.now().isoformat(timespec="seconds"),
        "wall_s": round(wall, 1),
        "memory_mb": round(int(footprint.group(1)) / 1048576, 1) if footprint else None,
        "timed_out": timed_out,
    }
    try:
        record.update(metrics(stdout, workdir, started))
    except Exception as e:
        record["metrics_error"] = repr(e)
    passed, check_log = check(task_dir, spec, workdir)
    record["passed"] = passed
    name = f"{tool}-{task_dir.name}"
    (log_dir / f"{name}.out").write_text(stdout[-200_000:])
    (log_dir / f"{name}.err").write_text(stderr[-200_000:])
    (log_dir / f"{name}.check").write_text(check_log)
    shutil.rmtree(workdir, ignore_errors=True)
    return record


def run(tools, names, repeat):
    log_dir = RUNS / datetime.now().strftime("%Y%m%d-%H%M%S")
    log_dir.mkdir(parents=True)
    for _ in range(repeat):
        for task_dir, spec in tasks(names):
            for tool in tools:
                record = run_one(tool, task_dir, spec, log_dir)
                with RESULTS.open("a") as f:
                    f.write(json.dumps(record) + "\n")
                tokens = record.get("input_fresh", 0) + record.get("input_cached", 0)
                print(f"{tool:7} {task_dir.name:22} {'PASS' if record['passed'] else 'fail'}  {record['wall_s']:6.1f}s  "
                      f"{record.get('requests', '?'):>3} req  {tokens:>8} in  {record.get('memory_mb')} MB", flush=True)
    print(f"logs in {log_dir}")


def latest_results():
    rows = [json.loads(line) for line in RESULTS.read_text().splitlines() if line.strip()]
    latest = {}
    for row in rows:
        latest.setdefault((row["tool"], row["task"]), []).append(row)
    return latest


def report():
    import chart

    latest = latest_results()
    names = ["killi", "kimi", "claude"]
    labels = {"killi": "killi", "kimi": "Kimi Code", "claude": "Claude Code"}
    task_names = sorted({task for _, task in latest})
    summary = {}
    for tool in names:
        runs = [r for (t, _), rs in latest.items() if t == tool for r in rs]
        if not runs:
            continue
        count = len(runs)
        summary[tool] = {
            "name": labels[tool],
            "model": runs[-1].get("model", "?"),
            "runs": count,
            "passed": sum(r["passed"] for r in runs),
            "pass_rate": sum(r["passed"] for r in runs) / count,
            "avg_wall_s": sum(r["wall_s"] for r in runs) / count,
            "avg_input": sum(r.get("input_fresh", 0) + r.get("input_cached", 0) for r in runs) / count,
            "avg_fresh": sum(r.get("input_fresh", 0) for r in runs) / count,
            "avg_output": sum(r.get("output", 0) for r in runs) / count,
            "avg_requests": sum(r.get("requests", 0) for r in runs) / count,
            "avg_memory_mb": sum(r.get("memory_mb") or 0 for r in runs) / count,
        }
    (ROOT / "docs/eval.json").write_text(json.dumps({"summary": summary, "runs": latest_rows(latest)}, indent=1) + "\n")
    chart.render_eval(summary)

    lines = ["# Capability tests", "",
             "Ten coding tasks, each run through killi, Kimi Code and Claude Code with `scripts/eval.py`.",
             "A task passes when its check (the project's tests, clippy, or an exact answer) succeeds after the",
             "tool finishes; the tests are restored first, so editing them doesn't help.", ""]
    lines += ["| | " + " | ".join(summary[t]["name"] for t in summary) + " |", "|---|" + "---|" * len(summary)]
    rows = [
        ("Model", lambda s: s["model"]),
        ("Passed", lambda s: f'{s["passed"]} / {s["runs"]}'),
        ("Input tokens per task", lambda s: f'{s["avg_input"]:,.0f}'),
        ("… not from cache", lambda s: f'{s["avg_fresh"]:,.0f}'),
        ("Output tokens per task", lambda s: f'{s["avg_output"]:,.0f}'),
        ("Requests per task", lambda s: f'{s["avg_requests"]:.1f}'),
        ("Time per task", lambda s: f'{s["avg_wall_s"]:.0f} s'),
        ("Peak memory", lambda s: f'{s["avg_memory_mb"]:.0f} MB'),
    ]
    for label, fmt in rows:
        lines.append(f"| {label} | " + " | ".join(fmt(summary[t]) for t in summary) + " |")
    lines += ["", "## Per task", "", "Pass (✓) or fail (✗), input tokens, and time.", ""]
    lines += ["| Task | " + " | ".join(summary[t]["name"] for t in summary) + " |", "|---|" + "---|" * len(summary)]
    for task in task_names:
        cells = []
        for tool in summary:
            rs = latest.get((tool, task), [])
            if not rs:
                cells.append("–")
                continue
            r = rs[-1]
            tokens = r.get("input_fresh", 0) + r.get("input_cached", 0)
            cells.append(f'{"✓" if r["passed"] else "✗"} {tokens / 1000:.0f}k · {r["wall_s"]:.0f} s')
        lines.append(f"| `{task}` | " + " | ".join(cells) + " |")
    lines += ["", "Peak memory is the tool's own process (macOS peak memory footprint), not the compilers or",
              "test runners it starts. Claude Code's tokens include this machine's skills, plugins and MCP",
              "servers, as configured; so do Kimi Code's.", ""]
    (ROOT / "docs/eval.md").write_text("\n".join(lines))
    print("wrote docs/eval.md, docs/eval.json and docs/eval-{light,dark}.svg")


def latest_rows(latest):
    return [rs[-1] for rs in latest.values()]


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser("verify")
    v.add_argument("--tasks")
    r = sub.add_parser("run")
    r.add_argument("--tools", default="killi,kimi,claude")
    r.add_argument("--tasks")
    r.add_argument("--repeat", type=int, default=1)
    sub.add_parser("report")
    args = parser.parse_args()
    names = set(args.tasks.split(",")) if getattr(args, "tasks", None) else None
    if args.cmd == "verify":
        verify(names)
    elif args.cmd == "run":
        run(args.tools.split(","), names, args.repeat)
    else:
        sys.path.insert(0, str(ROOT / "scripts"))
        report()


if __name__ == "__main__":
    main()
