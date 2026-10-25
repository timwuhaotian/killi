#!/usr/bin/env python3
"""Reads killi's session logs and flags what is worth fixing or optimizing.

  python3 scripts/sessions.py [--home DIR] [--project TEXT] [--since YYYY-MM-DD] [--all] [--json]

Logs are ~/.killi/sessions/<project>/<time>-<pid>.jsonl (--home changes ~/.killi). For each
session it counts turns, input tokens and how much came from the cache, tool calls, repeated
calls, errors and how the session ended. Per model it shows the cache hit rate and whether the
reasoning the provider billed was kept. Logs written since 0.2.16 also carry timestamps and
durations, which add the slowest requests and tools. Findings at the end are the things to look
at first; each is a lead to confirm in the log, not a verdict. The sessions that scripts/bench.py
and scripts/eval.py leave behind are left out unless --all is given.
"""

import argparse
import collections
import json
import re
import statistics
from datetime import datetime
from pathlib import Path

REPEATED = 3  # identical tool calls in one session before it is reported
SLEPT = 120  # seconds in commands that open with `sleep N` before a session is reported
LOW_CACHE = 0.5  # share of input tokens from the cache below which a model is flagged
FIXED_SLEEP = re.compile(r"\s*sleep\s+(\d+(?:\.\d+)?)")  # a command's opening `sleep N`
MEASURING = re.compile(r"-T-(bench|eval-)")  # where bench.py and eval.py run killi: a temporary folder


def read(path):
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            yield json.loads(line)
        except json.JSONDecodeError:
            continue


def looks_failed(output):
    """For logs from before tool results kept how they ended."""
    last = output.rstrip().splitlines()[-1:] or [""]
    return last[0].startswith("[Command ") or output.startswith(("Interrupted by the user", "Could not "))


def parse_args(arguments):
    try:
        args = json.loads(arguments)
    except json.JSONDecodeError:
        return {}
    return args if isinstance(args, dict) else {}


def analyze(path):
    s = {"path": str(path), "project": path.parent.name, "id": path.stem, "models": [], "turns": 0, "prompt": 0,
         "cached": 0, "completion": 0, "max_context": 0, "tools": collections.Counter(), "tool_errors": 0, "version": None,
         "repeats": collections.Counter(), "errors": [], "warns": [], "prompts": 0, "ended": "", "request_ms": [],
         "tool_ms": [], "slept": [0, 0], "compactions": [], "per_model": collections.defaultdict(lambda: collections.Counter())}
    model, awaiting, calls, answered, sleeping = None, None, {}, set(), set()
    for entry in read(path):
        kind = entry.get("type")
        message = entry.get("message") if isinstance(entry.get("message"), dict) else {}
        if kind in ("session", "model"):
            model = entry.get("model") or model
            s["version"] = entry.get("version") or s["version"]
            if model and model not in s["models"]:
                s["models"].append(model)
        elif kind == "message":
            role = message.get("role")
            if role == "user" and isinstance(message.get("content"), str) and not message["content"].startswith("<system-reminder>"):
                s["prompts"] += 1
            if role == "assistant":
                kept = bool(message.get("reasoning_content") or message.get("reasoning") or message.get("reasoning_details")
                            or message.get("reasoning_items"))
                if awaiting:  # a request's `usage` is logged just before its assistant message
                    s["per_model"][awaiting]["reasoning_lost"] += not kept
                    awaiting = None
                for call in message.get("tool_calls") or []:
                    name = call["function"]["name"]
                    calls[call["id"]] = name
                    if name == "bash":
                        command = parse_args(call["function"]["arguments"]).get("command")
                        if isinstance(command, str) and (wait := FIXED_SLEEP.match(command)) and float(wait[1]) >= 30:
                            sleeping.add(call["id"])
                    s["tools"][name] += 1
                    if name != "task_output":  # asking again is how a wait on a task works
                        s["repeats"][(name, call["function"]["arguments"])] += 1
            if role == "tool":
                answered.add(message.get("tool_call_id"))
                meta = entry.get("tool")
                failed = meta["is_error"] if meta else looks_failed(message.get("content") or "")
                s["tool_errors"] += bool(failed)
                if meta and meta.get("ms") is not None:
                    s["tool_ms"].append((meta["ms"], meta.get("name") or calls.get(message.get("tool_call_id"), "?")))
                    if message.get("tool_call_id") in sleeping and not failed:
                        s["slept"][0] += 1
                        s["slept"][1] += meta["ms"]
            s["ended"] = role or ""
        elif kind == "usage":
            s["turns"] += 1
            s["prompt"] += entry.get("prompt", 0)
            s["cached"] += entry.get("cached", 0)
            s["completion"] += entry.get("completion", 0)
            s["max_context"] = max(s["max_context"], entry.get("prompt", 0))
            if entry.get("ms") is not None:
                s["request_ms"].append(entry["ms"])
            current = entry.get("model") or model or "unknown"
            stats = s["per_model"][current]
            stats["turns"] += 1
            stats["prompt"] += entry.get("prompt", 0)
            stats["cached"] += entry.get("cached", 0)
            awaiting = None
            if entry.get("reasoning", 0) > 0:
                stats["billed_reasoning"] += 1
                awaiting = current
        elif kind == "compaction":
            # A subagent's log opens with its starting history as a cause-free compaction;
            # only a real one (from compact_with) carries `cause`.
            if entry.get("cause"):
                s["compactions"].append(entry["cause"])
        elif kind == "error":
            s["errors"].append(str(entry.get("message"))[:160])
            s["ended"] = "error"
        elif kind == "warn":
            s["warns"].append(str(entry.get("message"))[:160])
    if any(i not in answered for i in calls):
        s["ended"] = "unanswered tool call"
    s["repeats"] = {f"{n} {a[:90]}": c for (n, a), c in s["repeats"].items() if c >= REPEATED}
    s["started"] = datetime.fromtimestamp(int(path.stem.split("-")[0])).strftime("%Y-%m-%d %H:%M")
    return s


def pct(part, whole):
    return f"{100 * part / whole:.0f}%" if whole else "–"


def table(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(out)


def findings(sessions, models):
    found = []
    for model, m in models.items():
        if m["turns"] >= 5 and m["cached"] / max(m["prompt"], 1) < LOW_CACHE:
            found.append(f"**{model}: only {pct(m['cached'], m['prompt'])} of input came from the cache over {m['turns']} turns.** "
                         "Probe the provider with `scripts/probe.py cache`.")
        if m["billed_reasoning"] and m["reasoning_lost"] == m["billed_reasoning"]:
            found.append(f"**{model}: reasoning was billed on {m['billed_reasoning']} turns but never kept.** "
                         "Probe with `scripts/probe.py stream` and `responses`; a provider that hides it on chat/completions may return "
                         "it on the Responses API (`type = \"openai_responses\"`).")
    for s in sessions:
        name = f"{s['project'][-24:]}/{s['id']}"
        for call, count in s["repeats"].items():
            found.append(f"`{name}` made `{call}` {count} times.")
        count, ms = s["slept"]
        if ms >= SLEPT * 1000:
            found.append(f"`{name}` spent {ms / 1000:.0f}s in {count} commands that open with a fixed `sleep`: it waits the full time "
                         "however soon the job ends. `task_output` with `wait` or an `until <check>; do sleep 5; done` loop ends with the job.")
        if len(s["compactions"]) >= 3:
            c = s["compactions"][-1]
            found.append(f"`{name}` compacted {len(s['compactions'])} times (last: context {c.get('context', '?')}, "
                         f"suffix {c.get('suffix', '?')}, limit {c.get('limit', '?')}, reserve {c.get('reserve', '?')}, "
                         f"kept {c.get('kept_chars', '?')} chars): which part kept forcing it is in the causes.")
        if s["ended"] in ("tool", "unanswered tool call"):
            found.append(f"`{name}` stopped right after a tool call ({s['ended']}): interrupted, killed or stopped by the repeat breaker.")
        for error in sorted(set(s["errors"])):
            found.append(f"`{name}` hit an error: {error}")
        for warn in sorted(set(s["warns"])):
            found.append(f"`{name}` logged a warning: {warn}")
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--home", default=str(Path.home() / ".killi"))
    parser.add_argument("--project", default="")
    parser.add_argument("--since", default="")
    parser.add_argument("--all", action="store_true", help="include the sessions bench.py and eval.py ran")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    logs = sorted(Path(args.home, "sessions").glob("*/*.jsonl"), key=lambda p: int(p.stem.split("-")[0]))
    logs = [p for p in logs if args.project in p.parent.name and (args.all or not MEASURING.search(p.parent.name))]
    sessions = [analyze(p) for p in logs]
    sessions = [s for s in sessions if s["turns"] and s["started"] >= args.since]
    models = collections.defaultdict(collections.Counter)
    for s in sessions:
        for model, stats in s["per_model"].items():
            models[model].update(stats)
    if args.json:
        print(json.dumps({"sessions": sessions, "models": models, "findings": findings(sessions, models)}, indent=1))
        return

    print(f"# killi sessions: {len(sessions)} with requests, {len(logs) - len(sessions)} empty or filtered out\n")
    print(table(["started", "project", "models", "turns", "input", "cached", "max ctx", "out", "prompts", "ended"],
                [(s["started"], s["project"][-22:], ", ".join(s["models"]), s["turns"], f"{s['prompt']:,}", pct(s["cached"], s["prompt"]),
                  f"{s['max_context']:,}", f"{s['completion']:,}", s["prompts"], s["ended"]) for s in sessions]))
    print("\n## By model\n")
    print(table(["model", "turns", "cached", "reasoning billed", "…and kept"],
                [(m, c["turns"], pct(c["cached"], c["prompt"]), c["billed_reasoning"], c["billed_reasoning"] - c["reasoning_lost"])
                 for m, c in sorted(models.items(), key=lambda kv: -kv[1]["turns"])]))
    tools, errors = collections.Counter(), 0
    for s in sessions:
        tools.update(s["tools"])
        errors += s["tool_errors"]
    print(f"\n## Tools\n\n{sum(tools.values())} calls, {errors} that failed: " + ", ".join(f"{n} {c}" for n, c in tools.most_common()))
    requests = [ms for s in sessions for ms in s["request_ms"]]
    runs = sorted((ms, name) for s in sessions for ms, name in s["tool_ms"])
    if requests:
        print(f"\n## Time\n\nRequests: median {statistics.median(requests) / 1000:.1f} s, slowest {max(requests) / 1000:.1f} s over {len(requests)}.")
    if runs:
        print("Slowest tool calls: " + ", ".join(f"{name} {ms / 1000:.1f} s" for ms, name in runs[-5:][::-1]))
    if not requests and not runs:
        print("\n(No durations: only logs from 0.2.16 on have them.)")
    print("\n## Findings\n")
    for line in findings(sessions, models) or ["None."]:
        print(f"- {line}")


if __name__ == "__main__":
    main()
