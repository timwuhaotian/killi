#!/usr/bin/env python3
"""Reads killi's session logs and flags what is worth fixing or optimizing.

  python3 scripts/sessions.py [--home DIR] [--project TEXT] [--since YYYY-MM-DD] [--all] [--json]

Logs are ~/.killi/sessions/<project>/<time>-<pid>.jsonl (--home changes ~/.killi). For each
session it counts turns, input tokens and how much came from the cache, tool calls, repeated
calls, errors and how the session ended. Per model it shows the cache hit rate and whether the
reasoning the provider billed was kept. Logs written since 0.2.16 also carry timestamps and
durations, which add the slowest requests and tools, and how often sessions ran side by side. Findings
at the end are the things to look at first; each is a lead to confirm in the log, not a verdict. The sessions that scripts/bench.py
and scripts/eval.py leave behind are left out unless --all is given.
"""

import argparse
import collections
import json
import os
import re
import statistics
from datetime import datetime
from pathlib import Path

REPEATED = 3  # identical tool calls in one session before it is reported
SLEPT = 120  # seconds in commands that open with `sleep N` before a session is reported
LOW_CACHE = 0.5  # share of input tokens from the cache below which a model is flagged
FIXED_SLEEP = re.compile(r"\s*sleep\s+(\d+(?:\.\d+)?)")  # a command's opening `sleep N`
MEASURING = re.compile(r"-T-(bench|eval-)")  # where bench.py and eval.py run killi: a temporary folder
IDLE_GAP = 300_000  # ms without a log line that end a stretch of activity
# Commands that move or rewrite a work tree another session may be working in.
# `git stash list`/`show` only read, so they don't count.
TREE_MOVING = re.compile(r"\bgit\s+(checkout|switch|stash(?!\s+(list|show)\b)|reset\s+--hard|clean|restore)\b")
# The refusal the peer guard (0.6.8) answers a tree-moving command with.
PEER_REFUSAL = "Not run: another killi session"
# A directory a command points at: `cd <dir>`, `git -C <dir>`, `--work-tree[=]<dir>`; the last one wins.
TARGET_DIR = re.compile(r"""(?:\bcd\s+|\s-C\s*|--work-tree[=\s]\s*)(["']?)([^\s"';&|]+)\1""")
MISS_STREAK = 3  # consecutive uncached requests on a growing prompt before it is reported
# The notes killi appends when the other sessions in a repository change (live.rs, tree_note).
PEERS_ON = ("<system-reminder>Another killi session works", "<system-reminder>Other killi sessions work")
PEERS_OFF = "<system-reminder>No other killi session works"


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


def elsewhere(command, base, cwd):
    """Whether `command` acts on a directory outside the session's folder, as a command run in a
    linked worktree does: the guard (peer_guard.rs) leaves another tree alone, so it isn't a miss."""
    targets = TARGET_DIR.findall(command)
    if not (targets and cwd):
        return False
    target = os.path.normpath(os.path.join(base or cwd, os.path.expanduser(targets[-1][1])))
    return os.path.commonpath([target, cwd]) != cwd


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
         "tool_ms": [], "slept": [0, 0], "compactions": [], "per_model": collections.defaultdict(lambda: collections.Counter()),
         "active": [], "peer_git": [], "peer_refusals": 0, "miss_streak": []}
    model, awaiting, calls, answered, sleeping, peers = None, None, {}, set(), set(), False
    cwd, moving = None, {}
    streak, grew_from = 0, None
    for entry in read(path):
        kind = entry.get("type")
        if isinstance(entry.get("ts"), int):
            if s["active"] and entry["ts"] - s["active"][-1][1] <= IDLE_GAP:
                s["active"][-1][1] = entry["ts"]
            else:
                s["active"].append([entry["ts"], entry["ts"]])
        message = entry.get("message") if isinstance(entry.get("message"), dict) else {}
        if kind in ("session", "model"):
            model = entry.get("model") or model
            cwd = entry.get("cwd") or cwd
            s["version"] = entry.get("version") or s["version"]
            streak, grew_from = 0, None
            if model and model not in s["models"]:
                s["models"].append(model)
        elif kind == "message":
            role = message.get("role")
            if role == "user" and isinstance(message.get("content"), str) and not message["content"].startswith("<system-reminder>"):
                s["prompts"] += 1
            if role == "user" and isinstance(message.get("content"), str):
                peers = message["content"].startswith(PEERS_ON) or (peers and not message["content"].startswith(PEERS_OFF))
            if role == "assistant":
                kept = bool(message.get("reasoning_content") or message.get("reasoning") or message.get("reasoning_details")
                            or message.get("reasoning_items"))
                if isinstance(awaiting, str):  # logs without `reasoning_kept`: pair with this reply
                    s["per_model"][awaiting]["reasoning_lost"] += not kept
                    awaiting = None
                for call in message.get("tool_calls") or []:
                    name = call["function"]["name"]
                    calls[call["id"]] = name
                    if name == "bash":
                        command = parse_args(call["function"]["arguments"]).get("command")
                        if isinstance(command, str) and (wait := FIXED_SLEEP.match(command)) and float(wait[1]) >= 30:
                            sleeping.add(call["id"])
                        if peers and isinstance(command, str) and TREE_MOVING.search(command):
                            base = parse_args(call["function"]["arguments"]).get("cwd")
                            if not elsewhere(command, base if isinstance(base, str) else None, cwd):
                                moving[call["id"]] = command[:120]
                    s["tools"][name] += 1
                    if name != "task_output":  # asking again is how a wait on a task works
                        s["repeats"][(name, call["function"]["arguments"])] += 1
            if role == "tool":
                answered.add(message.get("tool_call_id"))
                meta = entry.get("tool")
                failed = meta["is_error"] if meta else looks_failed(message.get("content") or "")
                s["tool_errors"] += bool(failed)
                if (message.get("content") or "").startswith(PEER_REFUSAL):
                    s["peer_refusals"] += 1
                elif message.get("tool_call_id") in moving:
                    s["peer_git"].append(moving[message["tool_call_id"]])
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
            # A cache-miss streak: uncached requests on a prompt that keeps growing, the shape
            # of a provider that bills every request in full. A model switch or a compaction
            # rewrites history, so both start the count over.
            grew = grew_from is not None and entry.get("prompt", 0) > grew_from
            streak = streak + 1 if entry.get("cached", 0) == 0 and grew else 0
            if streak >= MISS_STREAK:
                s["miss_streak"].append((streak, entry.get("prompt", 0)))
            grew_from = entry.get("prompt", 0)
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
                # `reasoning_kept` (logged with the usage since 0.4.12) settles it at once; older
                # logs wait for the assistant message that follows and pair by dialect fields.
                # `reasoning_seen` (since 0.6.8) says what the stream carried on a lost turn:
                # which field the provider used, and fields killi doesn't read.
                if "reasoning_kept" in entry:
                    lost = not entry["reasoning_kept"]
                    stats["reasoning_lost"] += lost
                    if lost:
                        seen = entry.get("reasoning_seen") or {}
                        if seen.get("field"):
                            stats["lost_empty_field"] += 1
                        elif seen.get("unread"):
                            stats["lost_unread"] += 1
                        else:
                            stats["lost_no_field"] += 1
                else:
                    awaiting = current
        elif kind == "compaction":
            streak, grew_from = 0, None
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


def findings(sessions, models, versions_of):
    found = []

    def tag(versions):
        listed = sorted(v for v in versions if v)
        return f"[v{','.join(listed)}] " if listed else ""

    for model, m in models.items():
        if m["turns"] >= 5 and m["cached"] / max(m["prompt"], 1) < LOW_CACHE:
            found.append(f"{tag(versions_of[model])}**{model}: only {pct(m['cached'], m['prompt'])} of input came from the cache over {m['turns']} turns.** "
                         "Probe the provider with `scripts/probe.py cache`.")
        if m["billed_reasoning"] and m["reasoning_lost"] == m["billed_reasoning"]:
            split = ", ".join(filter(None, [
                f"{m['lost_no_field']} with no reasoning field" if m["lost_no_field"] else "",
                f"{m['lost_empty_field']} with an empty field" if m["lost_empty_field"] else "",
                f"{m['lost_unread']} carrying unread stream fields" if m["lost_unread"] else "",
            ]))
            found.append(f"{tag(versions_of[model])}**{model}: reasoning was billed on {m['billed_reasoning']} turns but never kept.** "
                         + (f"Lost turns: {split}. " if split else "")
                         + "Probe with `scripts/probe.py stream` and `responses`; a provider that hides it on chat/completions may return "
                         "it on the Responses API (`type = \"openai_responses\"`).")
    for s in sessions:
        name = f"{s['project'][-24:]}/{s['id']}"
        who = tag([s["version"]])
        for call, count in s["repeats"].items():
            found.append(f"{who}`{name}` made `{call}` {count} times.")
        count, ms = s["slept"]
        if ms >= SLEPT * 1000:
            found.append(f"{who}`{name}` spent {ms / 1000:.0f}s in {count} commands that open with a fixed `sleep`: it waits the full time "
                         "however soon the job ends. `task_output` with `wait` or an `until <check>; do sleep 5; done` loop ends with the job.")
        if s["miss_streak"]:
            longest, reached = max(s["miss_streak"])
            found.append(f"{who}`{name}` ran {longest} requests in a row at 0% cached while the prompt kept growing "
                         f"(reaching {reached:,} tokens): a provider shape, not noise. `scripts/probe.py cache` says which half is broken.")
        if s["peer_refusals"]:
            found.append(f"{who}`{name}` had {s['peer_refusals']} tree-moving git command(s) refused by the peer guard.")
        if len(s["compactions"]) >= 3:
            c = s["compactions"][-1]
            found.append(f"{who}`{name}` compacted {len(s['compactions'])} times (last: context {c.get('context', '?')}, "
                         f"suffix {c.get('suffix', '?')}, limit {c.get('limit', '?')}, reserve {c.get('reserve', '?')}, "
                         f"kept {c.get('kept_chars', '?')} chars): which part kept forcing it is in the causes.")
        if s["ended"] in ("tool", "unanswered tool call"):
            found.append(f"{who}`{name}` stopped right after a tool call ({s['ended']}): interrupted, killed or stopped by the repeat breaker.")
        for error in sorted(set(s["errors"])):
            found.append(f"{who}`{name}` hit an error: {error}")
        for warn in sorted(set(s["warns"])):
            found.append(f"{who}`{name}` logged a warning: {warn}")
        for command in s["peer_git"]:
            found.append(f"{who}`{name}` ran `{command}` after being told another session works in its repository, in the session's own folder, "
                         "and it was not refused; versions since 0.6.8 refuse these while a peer shares the tree.")
    return found


def side_by_side(sessions):
    """Hours with a session active, hours with two or more at once, the most at once, and the
    pairs that overlapped in one project folder."""
    edges = sorted((t, d) for s in sessions for start, end in s["active"] for t, d in ((start, 1), (end, -1)))
    active = together = 0
    running = most = 0
    last = None
    for t, d in edges:
        if last is not None:
            active += (t - last) * (running >= 1)
            together += (t - last) * (running >= 2)
        running += d
        most = max(most, running)
        last = t
    pairs = 0
    for i, a in enumerate(sessions):
        for b in sessions[i + 1:]:
            pairs += a["project"] == b["project"] and any(min(e1, e2) > max(s1, s2) for s1, e1 in a["active"] for s2, e2 in b["active"])
    return active / 3.6e6, together / 3.6e6, most, pairs


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
    versions_of = collections.defaultdict(set)
    for s in sessions:
        for model, stats in s["per_model"].items():
            models[model].update(stats)
            versions_of[model].add(s["version"] or "?")
    if args.json:
        print(json.dumps({"sessions": sessions, "models": models, "findings": findings(sessions, models, versions_of)}, indent=1))
        return

    print(f"# killi sessions: {len(sessions)} with requests, {len(logs) - len(sessions)} empty or filtered out\n")
    print(table(["started", "project", "models", "turns", "input", "cached", "max ctx", "out", "prompts", "ended"],
                [(s["started"], s["project"][-22:], ", ".join(s["models"]), s["turns"], f"{s['prompt']:,}", pct(s["cached"], s["prompt"]),
                  f"{s['max_context']:,}", f"{s['completion']:,}", s["prompts"], s["ended"]) for s in sessions]))
    print("\n## By model\n")
    print(table(["model", "turns", "cached", "reasoning billed", "…and kept"],
                [(m, c["turns"], pct(c["cached"], c["prompt"]), c["billed_reasoning"], c["billed_reasoning"] - c["reasoning_lost"])
                 for m, c in sorted(models.items(), key=lambda kv: -kv[1]["turns"])]))
    by_version = collections.defaultdict(lambda: [0, 0, 0])
    for s in sessions:
        row = by_version[s["version"] or "?"]
        row[0] += 1
        row[1] += s["turns"]
        row[2] += s["tool_errors"]
    print("\n## By version\n")
    print(table(["version", "sessions", "turns", "failed tool calls"],
                [(v, *row) for v, row in sorted(by_version.items())]))
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
    active, together, most, pairs = side_by_side(sessions)
    if active:
        print(f"\n## Side by side\n\n{active:.1f} h with a session active, {together:.1f} h ({pct(together, active)}) with two or more; "
              f"at most {most} at once; {pairs} pairs overlapped in one project folder.")
    print("\n## Findings\n")
    for line in findings(sessions, models, versions_of) or ["None."]:
        print(f"- {line}")


if __name__ == "__main__":
    main()
