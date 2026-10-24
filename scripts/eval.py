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
        start, solution = prepare(task_dir), prepare(task_dir, ["solution"])
        start_passes, _ = check(task_dir, spec, start)
        solved, log = check(task_dir, spec, solution)
        shutil.rmtree(start, ignore_errors=True)
        shutil.rmtree(solution, ignore_errors=True)
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


def version_of(tool):
    command, _ = tool_commands()[tool]
    words = subprocess.run([command("")[0], "--version"], capture_output=True, text=True, env=ENV).stdout.split()
    return next((w for w in words if w[:1].isdigit()), "?")


def run_one(tool, task_dir, spec, log_dir, version):
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
        "version": version,
        "config_model": "default" if tool == "claude" else MODEL,
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
    versions = {tool: version_of(tool) for tool in tools}
    for _ in range(repeat):
        for task_dir, spec in tasks(names):
            for tool in tools:
                record = run_one(tool, task_dir, spec, log_dir, versions[tool])
                with RESULTS.open("a") as f:
                    f.write(json.dumps(record) + "\n")
                tokens = record.get("input_fresh", 0) + record.get("input_cached", 0)
                print(f"{tool:7} {task_dir.name:22} {'PASS' if record['passed'] else 'fail'}  {record['wall_s']:6.1f}s  "
                      f"{record.get('requests', '?'):>3} req  {tokens:>8} in  {record.get('memory_mb')} MB", flush=True)
    print(f"logs in {log_dir}")


def config_of(row):
    return (row.get("version", "?"), row.get("config_model") or row.get("model", "?"))


def latest_results():
    rows = [json.loads(line) for line in RESULTS.read_text().splitlines() if line.strip()]
    configs = {}
    for row in rows:
        # A metrics failure must not make `(version, "?")` look like a new configuration.
        # Old records only have the model metrics() reported, so use it as their fallback.
        if "config_model" in row or "model" in row:
            configs[row["tool"]] = config_of(row)
    latest = {}
    for row in rows:
        if configs.get(row["tool"]) == config_of(row):
            latest.setdefault((row["tool"], row["task"]), []).append(row)
    # A row that predates config_model and lacks model after a metrics failure cannot be
    # attributed with certainty; if its tool/version has exactly one selected config, keep
    # its pass/fail there while metrics_valid keeps it out of performance means.
    for row in rows:
        if configs.get(row["tool"]) != config_of(row) and "config_model" not in row and "model" not in row:
            config = configs.get(row["tool"])
            if config and config[0] == row.get("version", "?"):
                latest.setdefault((row["tool"], row["task"]), []).append(row)
    return latest


def metrics_valid(row):
    fields = ("wall_s", "memory_mb", "requests", "input_fresh", "input_cached", "output")
    return "metrics_error" not in row and all(row.get(field) is not None for field in fields)


def average(rows, field):
    return sum(row[field] for row in rows) / len(rows) if rows else None


def format_metric(value, spec, suffix=""):
    return "N/A" if value is None else f"{value:{spec}}{suffix}"


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
        valid = [r for r in runs if metrics_valid(r)]
        summary[tool] = {
            "name": labels[tool],
            "version": runs[-1].get("version", "?"),
            "model": runs[-1].get("model", "?"),
            "runs": count,
            "passed": sum(r["passed"] for r in runs),
            "pass_rate": sum(r["passed"] for r in runs) / count,
            "valid_metrics": len(valid),
            "invalid_metrics": count - len(valid),
            "avg_wall_s": average(valid, "wall_s"),
            "avg_input": sum(r["input_fresh"] + r["input_cached"] for r in valid) / len(valid) if valid else None,
            "avg_fresh": average(valid, "input_fresh"),
            "avg_output": average(valid, "output"),
            "avg_requests": average(valid, "requests"),
            "avg_memory_mb": average(valid, "memory_mb"),
        }
    (ROOT / "docs/eval.json").write_text(json.dumps({"summary": summary, "runs": latest_rows(latest)}, indent=1) + "\n")
    chart.render_eval(summary)

    lines = ["# Capability tests", "",
             f"{len(task_names)} coding tasks, each run through killi, Kimi Code and Claude Code with `scripts/eval.py`.",
             "A task passes when its check (the project's tests, clippy, or an exact answer) succeeds after the",
             "tool finishes; the tests are restored first, so editing them doesn't help.",
             "The main report includes every repeat of each tool's latest version and model configuration.",
             "Pass rates include all runs; performance means use only records with complete, valid metrics.", ""]
    lines += ["| | " + " | ".join(summary[t]["name"] for t in summary) + " |", "|---|" + "---|" * len(summary)]
    rows = [
        ("Version", lambda s: s["version"]),
        ("Model", lambda s: s["model"]),
        ("Passed", lambda s: f'{s["passed"]} / {s["runs"]}'),
        ("Valid metrics", lambda s: f'{s["valid_metrics"]} / {s["runs"]}'),
        ("Input tokens per run", lambda s: format_metric(s["avg_input"], ",.0f")),
        ("… not from cache", lambda s: format_metric(s["avg_fresh"], ",.0f")),
        ("Output tokens per run", lambda s: format_metric(s["avg_output"], ",.0f")),
        ("Requests per run", lambda s: format_metric(s["avg_requests"], ".1f")),
        ("Time per run", lambda s: format_metric(s["avg_wall_s"], ".0f", " s")),
        ("Peak memory", lambda s: format_metric(s["avg_memory_mb"], ".0f", " MB")),
    ]
    for label, fmt in rows:
        lines.append(f"| {label} | " + " | ".join(fmt(summary[t]) for t in summary) + " |")
    lines += ["", "## Per task", "", "Passed runs, mean input tokens, and mean time over records with valid metrics.", ""]
    lines += ["| Task | " + " | ".join(summary[t]["name"] for t in summary) + " |", "|---|" + "---|" * len(summary)]
    for task in task_names:
        cells = []
        for tool in summary:
            rs = latest.get((tool, task), [])
            if not rs:
                cells.append("–")
                continue
            valid = [r for r in rs if metrics_valid(r)]
            tokens = sum(r["input_fresh"] + r["input_cached"] for r in valid) / len(valid) if valid else None
            cell = f'{sum(r["passed"] for r in rs)} / {len(rs)} passed · '
            cell += f'{format_metric(tokens / 1000 if tokens is not None else None, ".0f", "k")} · '
            cell += format_metric(average(valid, "wall_s"), ".0f", " s")
            if len(valid) < len(rs):
                cell += f" · {len(rs) - len(valid)} invalid"
            cells.append(cell)
        lines.append(f"| `{task}` | " + " | ".join(cells) + " |")
    every = [json.loads(line) for line in RESULTS.read_text().splitlines() if line.strip()]
    versions = {}
    for row in every:
        if row["tool"] == "killi":
            versions.setdefault(row.get("version", "?"), []).append(row)
    if len(versions) > 1:
        lines += ["", "## killi across versions", "", "Every run of killi, by version, on the tasks that version ran.", "",
                  "| Version | Passed | Input tokens per task | … not from cache | Requests per task | Time per task |", "|---|---|---|---|---|---|"]
        for version, rows in sorted(versions.items()):
            valid = [r for r in rows if metrics_valid(r)]
            tokens = sum(r["input_fresh"] + r["input_cached"] for r in valid) / len(valid) if valid else None
            lines.append(
                f"| {version} | {sum(r['passed'] for r in rows)} / {len(rows)} | "
                f"{format_metric(tokens, ',.0f')} | {format_metric(average(valid, 'input_fresh'), ',.0f')} | "
                f"{format_metric(average(valid, 'requests'), '.1f')} | "
                f"{format_metric(average(valid, 'wall_s'), '.0f', ' s')} |"
            )
    lines += ["", "Peak memory is the tool's own process (macOS peak memory footprint), not the compilers or",
              "test runners it starts. Claude Code's tokens include this machine's skills, plugins and MCP",
              "servers, as configured; so do Kimi Code's.", ""]
    (ROOT / "docs/eval.md").write_text("\n".join(lines))
    print("wrote docs/eval.md, docs/eval.json and docs/eval-{light,dark}.svg")


def latest_rows(latest):
    return [dict(row, metrics_valid=metrics_valid(row)) for rows in latest.values() for row in rows]


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
