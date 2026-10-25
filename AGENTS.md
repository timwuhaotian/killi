# killi

A Rust coding agent for the terminal. One crate, `crates/killi`; the design and the reasons
behind it are in `docs/decisions.md`.

## Commands

- Check: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- Capability tasks: `python3 scripts/eval.py verify` (each task fails as given and passes with
  its solution); `run` and `report` compare tools and need models configured.
- Release: see `RELEASING.md`. `scripts/release.sh` builds `releases/killi-arm64`.
- See the TUI without a model: `scripts/frames.py out/` draws every screen at 118, 80 and 60 columns
  on Kil's themes, from a scripted provider and a pty (`--png`, `--inline`, `--colors 256`; needs `pyte`).
  Use it after any change to `tui/`; a layout is only checked once it has been drawn at 80x24 and 60 columns.
- Re-measure size, memory and tokens and redraw the charts and README numbers: the
  `refresh-metrics` skill; it also says how `scripts/screenshot.py` redraws the README's
  screenshots from a real run. Find bugs and optimizations from real use: the `review-sessions`
  skill, with `scripts/sessions.py` (reads `~/.killi/sessions`) and `scripts/probe.py` (asks a
  provider how it behaves). Run the whole loop — measure, fix, verify, record, ship: the
  `self-eval-improve` skill. All live in `.agents/skills/`.

## Rules that keep the prompt cache hitting

- The system prompt and the tool list are fixed for a session. Never add, remove or reorder
  tools, or change a tool description, while a session runs; block behavior when a tool runs
  instead (see plan mode in `plan.rs` and `agent.rs`).
- History is append-only and stored exactly as sent. Notes to the model go at the end as user
  messages in `<system-reminder>` tags. Only compaction and `/reload` rewrite history.
- Provider adapters in `llm/` convert the OpenAI-format history the same way every time.

## Code

- Match the surrounding code: doc comments on public items and anything non-obvious, no
  comments that narrate changes.
- Keep tool descriptions short; `main.rs` has a test that keeps the system prompt and tools
  under about 3k tokens.
- The prompt's guidance is `prompt/system.md`, and users can replace it with `[prompt] system`
  in their config, so rules that must always hold belong in code, not only in that text.
- Prefer adding tests to the module's existing `tests` block.
