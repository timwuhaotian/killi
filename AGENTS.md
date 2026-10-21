# killi

A Rust coding agent for the terminal. One crate, `crates/killi`; the design and the reasons
behind it are in `docs/decisions.md`.

## Commands

- Check: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- Capability tasks: `python3 scripts/eval.py verify` (each task fails as given and passes with
  its solution); `run` and `report` compare tools and need models configured.
- Release: see `RELEASING.md`. `scripts/release.sh` builds `releases/killi-arm64`.

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
- Prefer adding tests to the module's existing `tests` block.
