# Audit brief shared by every Phase 1 audit agent

## Project
- Root: `/Users/wuhaotian@sphnet.com.sg/code/killi` — killi, a Rust coding agent for the terminal
  (macOS arm64 only), one crate `crates/killi` (~23k LoC). Tokio single-threaded runtime,
  reqwest (rustls), crossterm TUI with an inline renderer (default) and a fullscreen renderer
  (`TERM_PROGRAM=kil`). Version 0.4.20, git clean on `main`.
- Read before auditing: `AGENTS.md` (rules — especially the prompt-cache rules), `docs/decisions.md`
  (numbered design decisions WITH their measured reasons and "Not changed" notes), and the two
  intelligence briefs in this folder: `intel-0a.md` (architecture, data flow, concurrency, critical
  paths) and `intel-0b.md` (dependencies, integrations, distribution chain).
- The user asked for: **UI, UX, LLM harness, bugs, performance improvements, quality improvements.**
  Weight findings toward those. A finding that changes what the user sees or what the model
  receives is worth more than a style nit.

## Baseline (must stay green)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → passes (0 warnings).
- `cargo test --workspace --locked` → 338 passed, 0 failed (~4 s).
- TUI frames: `scripts/frames.py <outdir>` draws real screens from a scripted provider in a pty.
  Use the scratch venv's Python, which has pyte:
  `/private/tmp/claude-503/-Users-wuhaotian-sphnet-com-sg-code-killi/a381b734-e6e5-4c1d-b105-209d2882b618/scratchpad/venv/bin/python scripts/frames.py <outdir> --killi target/debug/killi --sizes 80x24 60x24 --themes night paper --scenes conv busy`
  (`cargo build` first; run `scripts/frames.py --help` for scenes/options; `--inline` for the
  default renderer). Output dirs go under the scratchpad.

## Rules for you
- **READ-ONLY on the repository.** Do not edit, create or delete files in the repo. Temporary
  files, scratch crates, test programs and outputs go in the scratchpad:
  `/private/tmp/claude-503/-Users-wuhaotian-sphnet-com-sg-code-killi/a381b734-e6e5-4c1d-b105-209d2882b618/scratchpad`
  (make your own subfolder). To prove a bug with a test, copy the crate into your subfolder
  (`cp -R crates Cargo.toml Cargo.lock <sub>/`, then `cargo test` there with
  `CARGO_TARGET_DIR=<sub>/target`) — never add tests to the real repo.
- **Never run the killi binary against the real HOME (`~/.killi`)**: a pty that closes during the
  first-run guide writes into the user's home and can orphan a process. `scripts/frames.py`
  already uses a scratch HOME; if you run killi yourself, set `HOME` to a scratch folder.
- Don't run `cargo update` (except `--dry-run`), don't touch git state, don't push, don't call
  real model providers.
- **Respect the cache rules** (AGENTS.md): never propose adding/removing/reordering tools or
  changing tool descriptions or the system prompt mid-session, or editing earlier history. A fix
  that would break the cached prefix is wrong for this project unless the finding is severe and
  you say why.
- **Don't re-report deliberate trade-offs.** decisions.md records many "Not changed" choices with
  evidence (e.g. no approval prompts — `/undo` is the net; macOS only; inline renderer default;
  Esc discards queued messages; no LSP; `| tail` on waits is the model's). Re-report one only with
  new evidence that it is wrong.
- **Evidence over volume.** Every finding cites `file:line`, the concrete failure scenario
  (inputs/state → wrong output/crash/cost), and how you verified it: `traced` (read the whole path),
  `tested` (scratch test or program, give the command), `reproduced` (pty/frames run), or
  `unverified` (say so). Prefer 10 real findings to 40 speculative ones. Don't pad.

## Already verified by the orchestrator (don't spend time re-proving these; extend them if you find
## related cases, and reference them by tag)
- **[V1]** `agent.rs:467-471` `ctx_images` uses `Rc::get_mut(&mut self.ctx)`, which returns `None`
  whenever any `Weak` exists — and `share()` (called on every `push_meta`) stores
  `Rc::downgrade(&self.ctx)` in `Aside`. So `/model` never updates `tools::Context.images`:
  after switching between an image and a text-only model, `read`/`fetch_url screenshot`/MCP images
  follow the OLD model's capability.
- **[V2]** `tui/mod.rs:1860-1865` `Event::Retry` doesn't flush `self.partial`, end the thought, or
  reset markdown state: a stream that fails mid-line and is retried prints the partial line glued
  to the retried reply's first line ("Here is the fiHere is the fix…"), and the thinking buffer
  accumulates both attempts.
- **[V3]** `tui/text.rs:416,470-477` `Markdown.in_code` is never reset (`self.markdown` is created
  once in `App::new`, `tui/mod.rs:947`). An unclosed fence (Esc mid-code-block, max-tokens cut,
  retry, odd fences, a replayed log) inverts code/prose styling for the rest of the session.
- **[V4]** `session.rs:307-321` `session::list` fully parses every log in the folder (`load`) to get
  the first prompt and count; logs reach MBs (base64 images), so `/resume`/`killi -r` cost grows
  with history.
- **[V5]** `agent.rs:1666-1671` `is_overflow` matches "too many tokens", which is also AWS Bedrock's
  throttling text ("Too many tokens, please wait before trying again") — the user runs Claude via a
  LiteLLM/Bedrock gateway; a persistent throttle would trigger an unneeded compaction.
- **[V6]** compaction keeps a skill run as `/name request` (0.4.18, `session::user_prompts` →
  `skills::typed`) but nothing tells the model the skill's text is gone and can be reloaded with
  the `skill` tool.
- **[V7]** `llm/mod.rs:211-228` a 200 response that isn't an SSE stream (HTML/JSON body from a
  misconfigured `base_url` or gateway) has its lines skipped as non-`data:` lines, then fails as
  "empty response without a termination reason", is retried 10 times (minutes), and the user never
  sees the body.

- **[V8]** (intel-0a obs 2) `agent.rs:412` + `session.rs:69-79`: a resumed agent restarts `children`
  at 0 and `Session::child(n)` opens `<id>/agent-N.jsonl` with `create_new`, so after resuming a
  session that ran subagents, the first N `agent`/`agent_swarm` children fail "Could not start the
  subagent: could not create …".
- **[V9]** (intel-0a obs 3) `agent.rs:645-649`: `newly_finished()` marks tasks reported at any depth
  but only depth 0 pushes the note, and subagents share `ctx`, so a subagent's step swallows the
  parent's "task finished" notices.
- **[V10]** (intel-0a obs 4) `tools/files.rs:185` + `tools/edit.rs:34-36`: `edit` decodes with
  `from_utf8_lossy` and writes the whole file back, so every invalid UTF-8 byte anywhere in a
  Latin-1/mixed file becomes U+FFFD — silent corruption of lines the model never touched.

## intel-0a "Observations for auditors" — who owns which lead
intel-0a.md ends with 22 UNVERIFIED leads. Each dimension below must confirm or refute the ones it
owns (say which, with evidence) besides its own checklist:
- A Code Quality: 22 (doc drift, `expect`s that can abort — `panic = "abort"` in release).
- B UI/UX: 6 (escapes reaching the screen — rendering side), 8 (first-run guide on EOF), 22 (`take_running` id collisions in rows, `/new` keeping plan/swarm).
- C Performance: 21 (all), plus the blocking-on-runtime list in "Concurrency & Shared State".
- D Architecture: module cycles, `tui/mod.rs` size, the `make_mut`/`share()` invariant.
- E Security: 6 (injection side), 7, 12, 13, 14, 16, 17 (fail-open), 18 (`notify` content).
- F LLM adapters: 22 (Responses `include` vs replay; OpenAI `resize_with(index)`), [V7].
- G Agent loop & tools: [V8] [V9] [V10], 5, 17 (hooks bypassed for `agent`/`agent_swarm`), 22 (subagent id clones).
- H Persistence & IPC: 9, 10 (resume tool list/cwd), 11, 19, 20, 22 (`project_dir` collisions).
- I Functional: 5, 10 (`/reload` keeping hooks/compact_at/plan_allow), 17, requirement drift vs README/decisions.
- J Dependencies & supply chain: 14, 15.
- K Ops: 18 (signal cleanup), CI/test/release gaps.

## Output
Write your full report to `<scratchpad>/audit-<LETTER>.md` in exactly this structure:

```markdown
## [Dimension Name] - Audit Results
**Score**: X/4  (0 = critical issues everywhere, 4 = excellent)
**Files reviewed**: N
**Issues found**: M

### P0 - Critical (must fix before merge / blocks production)
- **[file:line]** Short description.
  Scenario: inputs/state → wrong result.
  Impact: what breaks / who notices.
  Verified: traced | tested (`command`) | reproduced | unverified.
  Suggested fix: one or two sentences, naming the seam to change.
### P1 - High
### P2 - Medium
### P3 - Low / Nits
### Checked and fine (one line each: what you looked at that holds up — saves the next reader time)
```

If nothing is found: `No issues found in [Dimension]. Score: 4/4.` Then reply to the orchestrator
with ONLY: the path of your report, the counts per priority, and your top 5 findings in one line
each (`[P?] file:line — claim`). Keep that reply under 300 words.
