---
name: self-eval-improve
description: Run one full self-improvement round on killi itself, from its own session logs: measure recent real use, attribute each loss to a layer, confirm provider behavior with a probe, land the narrow fix, verify it live both ways, record the decision, and ship a patch release when asked. Use when asked to analyze recent killi conversations and optimize killi, to evolve or self-improve killi, or to run the improvement loop again. For a read-only question about how killi has been behaving, use review-sessions instead.
---

# Self-eval and improve killi

One round turns killi's own logs into one shipped improvement. The evidence-finding half is
`review-sessions` — load it first and follow it for how to read a log, tell cases apart, and
probe a provider. This skill is the loop around it: what to measure, how to decide, where a
fix may live, and when to stop. Past rounds are decisions 31, 34, 36, 43, 45 and 46 in
`docs/decisions.md`.

## 0. Scope the round

- The newest 20 real sessions by default (or what the user named): sort
  `~/.killi/sessions/*/*.jsonl` by the timestamp in the file name. The session you are running
  in is one of them and shows as an unanswered tool call; say so instead of reporting it as cut
  off. `bench.py`/`eval.py` sessions stay out.
- Pin the versions before measuring: the installed `killi --version`, and each log's
  `session.version` (logged since 0.4.11). A log written by an old binary is evidence about
  that binary. Check the git log and `docs/decisions.md` for the window so a finding that was
  already fixed is reported as fixed, with the decision number — re-fixing it is the waste this
  step exists to prevent.

## 1. Measure

`python3 scripts/sessions.py --json` first, then write a short throwaway script over the
scoped logs for what it doesn't count. The numbers every round reports:

- turns, input tokens, cached share, output, per model and in total; request time median, p90,
  p99, and the count over 60 s with the model on each.
- tool calls and failures per tool, with a sample of each failure's error text; repeated
  identical calls; slowest tool calls (parallel `agent` calls share one wall-clock `ms` — a
  log artifact, not lost time).
- per model: turns billed for reasoning vs turns that kept any. Since 0.4.12 the `usage`
  entry carries `reasoning_kept`, the dialect-agnostic verdict; older logs are paired by
  dialect field names, which decision 36 showed going wrong once, so say which you used.
- compactions per session and turns between them; since 0.4.11 each real one carries
  `cause: {context, suffix, limit, reserve, kept_chars}`, which names the forcing part.
- `error` and `warn` entries, verbatim; sessions ending on a tool call, with the last entries
  read before calling it anything but an interruption.

## 2. Attribute before fixing

Every candidate lands in exactly one bucket; the bucket decides whether this round touches it:

- **Already fixed** in the running binary: report with the decision/version, don't re-fix.
- **Provider behavior** (slow at tiny outputs, reasoning billed but not sent, cache at 0%):
  confirm with `scripts/probe.py` (`stream`, `responses`, `cache`), never from the log alone.
  The fix is config (`protocol = "openai_responses"` on a model, `type` on a provider), not
  code, and editing the user's `~/.killi/config.toml` needs their go-ahead. Provider and model
  names change fast: never branch code on them. A code fix must key on a signal any provider
  can produce — billed-but-not-kept is the shape to copy (decision 46).
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

Rank what remains P0/P1/P2 with session ids, counts, the code it lives in, and what is still a
guess. Say what you did not read: the logs count, they don't judge answer quality — that is
`scripts/eval.py`'s job, not this round's.

## 3. Land one narrow fix

- State the acceptance as something observable in the next round's logs or a test (a falling
  failure rate, `reasoning_kept` true, a compaction count), and the non-goals. Stop when it
  passes; further ideas go to the decision record as "not done", with the trigger that would
  reopen them.
- Reuse the seam that owns the invariant. Rules that must always hold go in code; the system
  prompt and tool list stay fixed mid-session (no adding, removing or rewording tools);
  history stays append-only; provider dialects stay opaque — reasoning items are stored and
  replayed verbatim, never re-sequenced (decision 46).
- Tests in the module's existing `tests` block. Then `cargo clippy --workspace --all-targets
  -- -D warnings` and `cargo test --workspace`, both green.

## 4. Verify live, both ways

Build the release binary and exercise the change against a real provider in a scratch folder
with `killi -p`: once in the configuration that triggers the old behavior (expect the old
symptom and the new detection), once in the fixed one (expect it gone). Check the new session
log it wrote, and delete the throwaway logs and folder after. A fix that only has a unit test
is unverified — say so plainly if a live run wasn't possible.

## 5. Record, then ship when asked

- Append a numbered decision to `docs/decisions.md`: what the logs showed (the counts), what
  changed and why there, what was measured and left alone, and what is deliberately not done.
  Add the `CHANGELOG.md` entry under `## Unreleased`.
- Only when the user asks to release: follow `RELEASING.md` — bump `[workspace.package]
  version`, `cargo update -p killi --offline`, move the entry under a `## x.y.z — date`
  heading, commit, `scripts/release.sh`, commit `releases/`, push, then `killi update` from
  the installed copy and report its version (GitHub's raw cache lags about five minutes).
  Stage files by name; this folder accumulates strays (`out/`).
