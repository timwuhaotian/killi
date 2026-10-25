---
name: self-eval-improve
description: Evaluate and improve killi from its own session logs in ~/.killi/sessions. Two modes: review (read-only — measure recent real use, read the log behind each lead, confirm provider behavior with a probe, report ranked findings) and improve (the full loop — attribute each loss, land one narrow fix, verify it live both ways, record the decision, ship a patch release when asked). Use when asked how killi has been behaving, to investigate one odd session, to analyze recent killi conversations and find optimizations, or to evolve, self-improve or run the improvement loop on killi.
---

# Self-eval and improve killi

The logs are the evidence; the scripts find the leads; a probe confirms what a lead says about
a provider; the report ranks what to do. **Review mode** stops at the report — use it for "how
has killi been behaving" or one odd session. **Improve mode** continues to one landed,
verified, recorded fix — use it for "optimize / evolve killi" or when the user asks to fix
what a review found. If the request doesn't say, review. Past rounds are decisions 31, 34, 36,
43, 45 and 46 in `docs/decisions.md`.

## 0. Scope and pin versions

- Improve mode: the newest 20 real sessions by default (or what the user named), sorted by the
  timestamp in `~/.killi/sessions/*/*.jsonl` file names. Review mode: whatever the question
  scopes to — one session, one project, or all of them.
- Pin the versions before measuring: the installed `killi --version`, and each log's
  `session.version` (logged since 0.4.11). A log written by an old binary is evidence about
  that binary. Check the git log and `docs/decisions.md` for the window so a finding that was
  already fixed is reported as fixed, with the decision number — re-fixing it is the waste this
  step exists to prevent.
- The session you are running in shows as stopped after a tool call; say so instead of
  reporting it as cut off. The sessions `bench.py` and `eval.py` ran are left out unless
  `--all`.

## 1. Measure

```sh
python3 scripts/sessions.py                       # every session, by model, tools, time, findings
python3 scripts/sessions.py --project my-app --since 2026-10-22
python3 scripts/sessions.py --json                # the same, for a script
```

Then write a short throwaway script over the scoped logs for what it doesn't count. The
numbers every review reports:

- turns, input tokens, cached share, output, per model and in total; request time median, p90,
  p99, and the count over 60 s with the model on each.
- tool calls and failures per tool, with a sample of each failure's error text; repeated
  identical calls; slowest tool calls (parallel `agent` calls share one wall-clock `ms` — a
  log artifact, not lost time).
- per model: turns billed for reasoning vs turns that kept any. Since 0.4.12 the `usage` entry
  carries `reasoning_kept`, the dialect-agnostic verdict; older logs are paired by dialect
  field names, which decision 36 showed going wrong once, so say which you used.
- compactions per session and turns between them; since 0.4.11 each real one carries
  `cause: {context, suffix, limit, reserve, kept_chars}`, which names the forcing part.
- `error` and `warn` entries, verbatim; sessions ending on a tool call, with the last entries
  read before calling it anything but an interruption.

## 2. Read the log behind a lead

A log is JSON lines: `session` (version, cwd, model, context window, tools), `message` (exactly
what was sent; a tool result also has `tool: {name, is_error, brief, ms}` beside it), `usage`
(model, input, cached, completion, reasoning tokens, `reasoning_kept`, `ms`), `model`,
`compaction` (with `cause`), `error`, `warn`, each with `ts`. Logs before 0.2.16 have no
timestamps or durations, and a log's starting `model` is the one it began on: switches are
`type: model` records, which the script follows. Long tool outputs and subagent logs are in a
folder named like the log.

How the earlier cases were told apart:

- **A call repeated many times.** Compare what the model *said* (`content`) with the call it
  made. Look at `usage.completion` on each repeat: the same count every time, matching the
  call's length, means the provider generated it, not that killi's parser duplicated it. Tool
  call ids were unique and arguments valid. Then ask whether its reasoning was lost (next).
- **Reasoning billed but not kept.** `usage.reasoning > 0` with `reasoning_kept` false (older
  logs: no `reasoning_content`, `reasoning_details` or `reasoning_items` on the assistant
  message). Run `python3 scripts/probe.py stream <model>`: if nothing reasoning-like streams,
  the provider hides it on chat/completions. `python3 scripts/probe.py responses <model>` shows
  whether the Responses API returns reasoning items; if it does, set
  `type = "openai_responses"` on the provider, or `protocol` on just that model, in
  `~/.killi/config.toml`.
- **Cache at 0%.** The first request of a session is always a miss; a model at 0% on every turn
  is the finding. `python3 scripts/probe.py cache <model>` sends the same prefix twice, and
  `--cache-control` marks it. A gateway that writes a cache on request 1 and bills request 2 in
  full doesn't serve reads: adding `cache_control` would cost more, not less.
- **Errors.** `error` records end with `(after N attempt(s))`. A usage-limit 429 isn't retried
  since 0.2.16.
- **A session ending on a tool call.** Interrupted or killed; `Agent::recover` answers the
  dangling call on resume, so it is only a bug if the *resumed* session breaks.

Probes send small synthetic requests with the provider's own key and never print it. A probe
that would send repository content (replaying a recorded request) needs the user's approval
first, since it re-sends their code to a third party.

## 3. Report — review mode stops here

Rank the findings P0/P1/P2, each with the evidence (session ids, counts), where in the code it
lives, a size, and what is still a guess. Say what you did not read: the scripts count, they
don't judge a model's answers — answer quality is `scripts/eval.py`'s job. Keep the report to
what the logs show.

## 4. Improve: attribute before fixing

Every candidate lands in exactly one bucket; the bucket decides whether this round touches it:

- **Already fixed** in the running binary: report with the decision/version, don't re-fix.
- **Provider behavior** (slow at tiny outputs, reasoning billed but not sent, cache at 0%):
  confirm with a probe, never from the log alone. The fix is config
  (`protocol = "openai_responses"` on a model, `type` on a provider), not code, and editing the
  user's `~/.killi/config.toml` needs their go-ahead. Provider and model names change fast:
  never branch code on them. A code fix must key on a signal any provider can produce —
  billed-but-not-kept is the shape to copy (decision 46).
- **Model behavior** (repeated calls it chose, `| tail` on a wait, reading too long): report
  with counts. It is not a harness bug. Only a failure the tool's own output could have
  prevented becomes a fix — a better error, a blocked waste — and it goes in code, never only
  in `prompt/system.md`, which users can replace (see `AGENTS.md`).
- **Harness defect**: trace it from the log line to the code that wrote it, and read that code
  and its git history before proposing. The apparent cause has twice been an artifact of how
  the reader paired entries (decision 36) or of an old binary (decision 43).
- **Log gap**: if the log can't answer a question the round needed, that's a finding too.
  Closing it is a write-and-read pair — the new field in the `record` call and its reader in
  `scripts/sessions.py` in the same change, old logs still parseable. Fields ride beside the
  message, never inside it: history is resent byte-for-byte (see `session.rs`).

## 5. Land one narrow fix

- State the acceptance as something observable in the next round's logs or a test (a falling
  failure rate, `reasoning_kept` true, a compaction count), and the non-goals. Stop when it
  passes; further ideas go to the decision record as "not done", with the trigger that would
  reopen them.
- Follow `AGENTS.md`: reuse the seam that owns the invariant; rules that must always hold go in
  code, not only in `prompt/system.md`; the system prompt and tool list stay fixed mid-session
  (no adding, removing or rewording tools); history stays append-only; provider dialects stay
  opaque — reasoning items are stored and replayed verbatim, never re-sequenced (decision 46).
- Tests in the module's existing `tests` block. Then `cargo clippy --workspace --all-targets
  -- -D warnings` and `cargo test --workspace`, both green.

## 6. Verify live, both ways

Build the release binary and exercise the change against a real provider in a scratch folder
with `killi -p`: once in the configuration that triggers the old behavior (expect the old
symptom and the new detection), once in the fixed one (expect it gone). Check the new session
log it wrote, and delete the throwaway logs and folder after. A fix that only has a unit test
is unverified — say so plainly if a live run wasn't possible.

## 7. Record, then ship when asked

- Append a numbered decision to `docs/decisions.md`: what the logs showed (the counts), what
  changed and why there, what was measured and left alone, and what is deliberately not done.
  Add the `CHANGELOG.md` entry under `## Unreleased`. Don't commit unless asked.
- Only when the user asks to release: follow `RELEASING.md` — bump `[workspace.package]
  version`, `cargo update -p killi --offline`, move the entry under a `## x.y.z — date`
  heading, commit, `scripts/release.sh`, commit `releases/`, push, then `killi update` from the
  installed copy and report its version (GitHub's raw cache lags about five minutes). Stage
  files by name; this folder accumulates strays (`out/`).
