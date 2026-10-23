#!/usr/bin/env python3
"""Measures killi, Kimi Code, Claude Code and opencode 2 on one prompt and writes docs/bench.json.

For each tool: installed size, startup time (`--version`, median of 5), peak memory (maximum
resident set size of a one-prompt run, median of 3, from /usr/bin/time -l), and the input
tokens of the first request as the tool itself reports them. Every run happens in an empty
folder. killi, Kimi Code and opencode use BENCH_MODEL; Claude Code uses its default model.

`--tools killi,opencode` measures only those; the other tools keep their rows in
docs/bench.json, each with the date it was measured.

Override binaries with KILLI_BIN, KIMI_BIN, CLAUDE_BIN and OPENCODE_BIN. OPENCODE_BIN is the
compiled binary (`@opencode/cli-darwin-arm64/bin/opencode` in the npm package), not the npm
launcher. opencode gets a fresh config holding only BENCH_MODEL, built from killi's
`~/.killi/config.toml`, in its own XDG folders; HOME stays real, so like the others it sees
this machine's skills.
"""

import argparse
import json
import os
import re
import shutil
import sqlite3
import statistics
import subprocess
import tempfile
import time
import tomllib
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


def machine():
    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
    macos = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
    return f"{chip}, macOS {macos}"


def size_mb(paths):
    return round(sum(p.stat().st_size for p in paths if p.exists()) / 1048576, 1)


def timed(cmd, cwd, env=ENV):
    started = time.time()
    proc = subprocess.run(["/usr/bin/time", "-l", *cmd], cwd=cwd, env=env, capture_output=True, text=True,
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


def opencode_env():
    """A fresh opencode 2 config with only BENCH_MODEL, taken from killi's provider, in its own XDG folders."""
    config = tomllib.loads((Path.home() / ".killi/config.toml").read_text())
    model = config["models"][MODEL]
    provider = config["providers"][model["provider"]]
    if provider["type"] != "openai":
        raise SystemExit(f"opencode bench config handles OpenAI-compatible providers, not {provider['type']}")
    home = Path(tempfile.mkdtemp(prefix="bench-opencode"))
    (home / "opencode.jsonc").write_text(json.dumps({
        "model": MODEL,
        "providers": {model["provider"]: {
            "name": model["provider"],
            "env": ["BENCH_API_KEY"],
            "package": "@opencode/ai/providers/openai-compatible",
            "settings": {"baseURL": provider["base_url"], "apiKey": "{env:BENCH_API_KEY}"},
            "models": {model["model"]: {"limit": {"context": model["max_context_size"], "output": model["max_output_size"]}}},
        }},
    }))
    return {**ENV, "BENCH_API_KEY": provider["api_key"], "OPENCODE_CONFIG": str(home / "opencode.jsonc"),
            **{f"XDG_{d}_HOME": str(home / d.lower()) for d in ("CONFIG", "DATA", "CACHE", "STATE")}}


def opencode_tokens(env):
    """Input of the first request, from the latest assistant message in opencode's database: its `--format
    json` events carry no usage. Fresh, cache-read and cache-write tokens, not output."""
    def read(_out, _workdir):
        db = sqlite3.connect(f"file:{Path(env['XDG_DATA_HOME']) / 'opencode/opencode.db'}?mode=ro", uri=True)
        row = db.execute("select data from session_message where type = 'assistant' order by time_created desc").fetchone()
        tokens = json.loads(row[0])["tokens"]
        return tokens["input"] + tokens["cache"]["read"] + tokens["cache"]["write"]
    return read


def measure(name, version_cmd, run_cmd, tokens_from, size_paths, runtime, model, env=ENV, warmup=False):
    workdir = Path(tempfile.mkdtemp(prefix="bench"))
    if warmup:
        timed(run_cmd, workdir, env)
    startup = statistics.median(timed(version_cmd, workdir, env)[0] for _ in range(5))
    runs = [timed(run_cmd, workdir, env) for _ in range(3)]
    version = subprocess.run(version_cmd, capture_output=True, text=True, env=env).stdout.split()
    tokens = tokens_from(runs[-1][2], workdir)
    if isinstance(tokens, tuple):
        tokens, model = tokens
    result = {
        "name": name,
        "version": next((v.lstrip("v") for v in version if v.lstrip("v")[:1].isdigit()), "?"),
        "runtime": runtime,
        "model": model,
        "size_mb": size_mb(size_paths),
        "startup_ms": round(startup * 1000),
        "memory_mb": round(statistics.median(r[1] for r in runs), 1),
        "tokens": tokens,
        "measured": date.today().isoformat(),
    }
    print(json.dumps(result), flush=True)
    return result


def measure_killi():
    killi = binary("KILLI_BIN", "killi")
    return measure("killi", [killi, "--version"], [killi, "--json", "-m", MODEL, "-p", PROMPT],
                   lambda out, _: json.loads(out)["first_prompt_tokens"], [killi], "Rust", MODEL.split("/")[-1])


def measure_kimi():
    kimi = binary("KIMI_BIN", "kimi")
    return measure("Kimi Code", [kimi, "--version"], [kimi, "-m", MODEL, "-p", PROMPT],
                   lambda _, wd: kimi_tokens(wd), [kimi, kimi.parent / "rg", kimi.parent / "fd"], "Node.js", MODEL.split("/")[-1])


def measure_claude():
    claude = binary("CLAUDE_BIN", "claude")
    return measure("Claude Code", [claude, "--version"], [claude, "-p", PROMPT, "--output-format", "json"],
                   claude_tokens, [claude], "Bun", "default model")


def measure_opencode():
    """`run` talks to a background service by default, which `time` wouldn't see; --standalone keeps it in one process."""
    opencode, env = binary("OPENCODE_BIN", "opencode"), opencode_env()
    return measure("opencode 2", [opencode, "--version"],
                   [opencode, "run", "--standalone", "--format", "json", "-m", MODEL, PROMPT],
                   opencode_tokens(env), [opencode], "Bun", MODEL.split("/")[-1], env=env, warmup=True)


MEASURES = {"killi": measure_killi, "kimi": measure_kimi, "claude": measure_claude, "opencode": measure_opencode}


def main():
    parser = argparse.ArgumentParser(description="Measure the tools on one prompt and write docs/bench.json.")
    parser.add_argument("--tools", default=",".join(MEASURES), help="comma-separated subset; the rest keep their rows")
    chosen = parser.parse_args().tools.split(",")
    unknown = [t for t in chosen if t not in MEASURES]
    if unknown:
        raise SystemExit(f"unknown tool {unknown[0]}; choose from {', '.join(MEASURES)}")
    path = ROOT / "docs/bench.json"
    old = json.loads(path.read_text()) if path.exists() else {"tools": []}
    kept = {t["name"]: {"measured": old.get("measured"), **t} for t in old["tools"]}
    fresh = {r["name"]: r for r in (MEASURES[t]() for t in chosen)}
    merged = {**kept, **fresh}
    tools = [merged[n] for n in ("killi", "Kimi Code", "Claude Code", "opencode 2") if n in merged]
    out = {"measured": date.today().isoformat(), "machine": machine(), "prompt": PROMPT, "tools": tools}
    path.write_text(json.dumps(out, indent=2) + "\n")
    print(f"wrote {path}")


if __name__ == "__main__":
    main()
