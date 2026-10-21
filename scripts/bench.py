#!/usr/bin/env python3
"""Measures killi, Kimi Code and Claude Code on one prompt and writes docs/bench.json.

For each tool: installed size, startup time (`--version`, median of 5), peak memory (maximum
resident set size of a one-prompt run, median of 3, from /usr/bin/time -l), and the input
tokens of the first request as the tool itself reports them. Every run happens in an empty
folder. killi and Kimi Code use BENCH_MODEL; Claude Code uses its default model.

Override binaries with KILLI_BIN, KIMI_BIN and CLAUDE_BIN.
"""

import json
import os
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from datetime import date
from pathlib import Path

PROMPT = "Reply with the single word OK."
MODEL = os.environ.get("BENCH_MODEL", "mimo/mimo-v2.6-pro")
ROOT = Path(__file__).resolve().parent.parent
# Claude Code refuses some flags when it thinks it runs inside itself.
ENV = {k: v for k, v in os.environ.items() if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}


def binary(env_var, name):
    path = os.environ.get(env_var) or shutil.which(name)
    if not path:
        raise SystemExit(f"{name} not found; set {env_var}")
    return Path(path).resolve()


def size_mb(paths):
    return round(sum(p.stat().st_size for p in paths if p.exists()) / 1048576, 1)


def timed(cmd, cwd):
    started = time.time()
    proc = subprocess.run(["/usr/bin/time", "-l", *cmd], cwd=cwd, env=ENV, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL, timeout=600)
    wall = time.time() - started
    if proc.returncode != 0:
        raise SystemExit(f"{cmd[0]} failed:\n{proc.stdout[-2000:]}\n{proc.stderr[-2000:]}")
    rss = int(re.search(r"(\d+)\s+maximum resident set size", proc.stderr).group(1))
    return wall, rss / 1048576, proc.stdout


def kimi_tokens(workdir):
    sessions = Path.home() / ".kimi-code/sessions"
    dirs = sorted(sessions.glob(f"wd_{workdir.name}_*"), key=lambda p: p.stat().st_mtime)
    logs = sorted(dirs[-1].glob("*/agents/main/wire.jsonl"), key=lambda p: p.stat().st_mtime)
    text = logs[-1].read_text()
    usage = {k: int(re.search(rf'"{k}":(\d+)', text).group(1)) for k in ("inputOther", "inputCacheRead", "inputCacheCreation")}
    return sum(usage.values())


def claude_tokens(out, _workdir):
    report = json.loads(out)
    usage = report["usage"]
    tokens = usage["input_tokens"] + usage["cache_creation_input_tokens"] + usage["cache_read_input_tokens"]
    return tokens, ", ".join(report.get("modelUsage", {})) or "default model"


def measure(name, version_cmd, run_cmd, tokens_from, size_paths, runtime, model):
    workdir = Path(tempfile.mkdtemp(prefix="bench"))
    startup = statistics.median(timed(version_cmd, workdir)[0] for _ in range(5))
    runs = [timed(run_cmd, workdir) for _ in range(3)]
    version = subprocess.run(version_cmd, capture_output=True, text=True, env=ENV).stdout.split()
    tokens = tokens_from(runs[-1][2], workdir)
    if isinstance(tokens, tuple):
        tokens, model = tokens
    result = {
        "name": name,
        "version": next((v for v in version if v[0].isdigit()), "?"),
        "runtime": runtime,
        "model": model,
        "size_mb": size_mb(size_paths),
        "startup_ms": round(startup * 1000),
        "memory_mb": round(statistics.median(r[1] for r in runs), 1),
        "tokens": tokens,
    }
    print(json.dumps(result), flush=True)
    return result


def main():
    killi, kimi, claude = binary("KILLI_BIN", "killi"), binary("KIMI_BIN", "kimi"), binary("CLAUDE_BIN", "claude")
    model_name = MODEL.split("/")[-1]
    tools = [
        measure("killi", [killi, "--version"], [killi, "--json", "-m", MODEL, "-p", PROMPT],
                lambda out, _: json.loads(out)["first_prompt_tokens"], [killi], "Rust", model_name),
        measure("Kimi Code", [kimi, "--version"], [kimi, "-m", MODEL, "-p", PROMPT],
                lambda _, wd: kimi_tokens(wd), [kimi, kimi.parent / "rg", kimi.parent / "fd"], "Node.js", model_name),
        measure("Claude Code", [claude, "--version"], [claude, "-p", PROMPT, "--output-format", "json"],
                claude_tokens, [claude], "Bun", "default model"),
    ]
    out = {"measured": date.today().isoformat(), "prompt": PROMPT, "tools": tools}
    (ROOT / "docs/bench.json").write_text(json.dumps(out, indent=2) + "\n")
    print(f"wrote {ROOT / 'docs/bench.json'}")


if __name__ == "__main__":
    main()
