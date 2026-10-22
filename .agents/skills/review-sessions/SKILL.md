---
name: review-sessions
description: Read killi's own session logs in ~/.killi/sessions to find bugs and optimizations from real use: cache hit rate per model, reasoning the provider billed but killi never kept, repeated tool calls, retries on quota errors, runs cut off mid-tool-call, slow requests and tools. Use when asked how killi has been behaving, what to fix or optimize from real usage, or to investigate one odd session.
---

# Review killi's session logs

The logs are the evidence; the scripts find the leads; a probe confirms what a lead says about
a provider; the report ranks what to do. Past findings are in `docs/decisions.md`, decision 31.

## 1. Find the leads

```sh
python3 scripts/sessions.py                       # every session, by model, tools, time, findings
python3 scripts/sessions.py --project my-app --since 2026-10-22
python3 scripts/sessions.py --json                # the same, for a script
```

The sessions `bench.py` and `eval.py` ran are left out unless `--all`. A session still running
shows as stopped after a tool call. Logs before 0.2.16 have no timestamps or durations, and the
session header's `model` is the one it started on: switches are `type: model` records, which the
script follows.

## 2. Read the log behind a lead

A log is JSON lines: `session` (cwd, model, tools), `message` (exactly what was sent;
a tool result also has `tool: {is_error, brief, ms}` beside it), `usage` (input, cached,
completion, reasoning tokens, `ms`), `model`, `compaction`, `error`, `warn`, each with `ts`.
Long tool outputs are in a folder named like the log.

How the earlier cases were told apart:

- **A call repeated many times.** Compare what the model *said* (`content`) with the call it
  made. Look at `usage.completion` on each repeat: the same count every time, matching the call's
  length, means the provider generated it, not that killi's parser duplicated it. Tool call ids
  were unique and arguments valid. Then ask whether its reasoning was lost (next).
- **Reasoning billed but not kept.** `usage.reasoning > 0` and no `reasoning_content`,
  `reasoning_details` or `reasoning_items` on the assistant message. Run
  `python3 scripts/probe.py stream <model>`: if nothing reasoning-like streams, the provider hides
  it on chat/completions. `python3 scripts/probe.py responses <model>` shows whether the Responses
  API returns reasoning items; if it does, set `type = "openai_responses"` on the provider, or
  `protocol` on just that model, in `~/.killi/config.toml`.
- **Cache at 0%.** The first request of a session is always a miss; a model at 0% on every
  turn is the finding. `python3 scripts/probe.py cache <model>` sends the same prefix twice, and
  `--cache-control` marks it. A gateway that writes a cache on request 1 and bills request 2 in
  full doesn't serve reads: adding `cache_control` would cost more, not less.
- **Errors.** `error` records end with `(after N attempt(s))`. A usage-limit 429 isn't retried
  since 0.2.16.
- **A session ending on a tool call.** Interrupted or killed; `Agent::recover` answers the
  dangling call on resume, so it is only a bug if the *resumed* session breaks.

## 3. Report

Rank the findings P0/P1/P2, each with the evidence (session ids, counts), where in the code
it lives, a size, and what is still a guess. Say what you did not read: the scripts count, they
don't judge a model's answers. Keep the report to what the logs show.

## 4. Probe rules

Probes send small synthetic requests with the provider's own key and never print it. A probe
that would send repository content (replaying a recorded request) needs the user's approval
first, since it re-sends their code to a third party.

## 5. When asked to fix

Follow `AGENTS.md`: the system prompt and tool list stay fixed during a session, history is
append-only, rules that must always hold go in code not in `prompt/system.md`, tests go in the
module's existing `tests` block, and run clippy and the tests before saying it's done. Don't
commit unless asked.
