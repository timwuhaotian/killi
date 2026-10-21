<p align="center">
  <img src="docs/icon.png" width="128" height="128" alt="killi icon">
</p>

<h1 align="center">killi</h1>

[![CI](https://github.com/timwuhaotian/killi/actions/workflows/ci.yml/badge.svg)](https://github.com/timwuhaotian/killi/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![macOS · Apple silicon](https://img.shields.io/badge/macOS-Apple%20silicon-lightgrey)

A coding agent for the terminal, written in Rust (4.3 MB). It reads your code, runs commands
and edits files with any OpenAI-compatible model, and it is built around one idea: send the
model as little as possible, and send it the same way every time, so the provider's prompt
cache keeps hitting. It is the sibling of the [Kil](https://github.com/timwuhaotian/kil)
terminal and the [Kiled](https://github.com/timwuhaotian/kiled) editor.

<p align="center">
  <img src="docs/screenshot.png" width="960" alt="killi fixing a failing Rust test: it globs, reads two files, explains the bug, edits src/lib.rs, runs cargo test, and reports 5 requests, 84% of input tokens cached">
</p>

> **Status: early.** One prompt per run (`killi -p`) for now; the interactive TUI is next.
> Apple silicon Macs only.

## Features

- **Small and quick.** One 4.3 MB binary with no runtime to install. It starts in 4 ms and
  peaks at about 14 MB of memory while it works.
- **Few tokens.** The system prompt and all six tools come to about 1,300 tokens. Your
  `AGENTS.md` files are added, and nothing else.
- **Cache-friendly by design.** The system prompt and the tool list are fixed for the whole
  session, and history is only ever appended to, never rewritten. On MiMo, 70–90% of the
  input tokens in a typical task come from the cache.
- **Six tools that forgive small mistakes.** `read`, `write`, `edit`, `bash`, `grep` and
  `glob`. `edit` takes several changes in one call and still matches when only trailing
  spaces, smart quotes or dashes differ. Long output keeps its useful end and points to the
  full copy on disk.
- **Steady on flaky networks.** Failed requests retry with backoff and honor `Retry-After`;
  half-finished replies are dropped rather than kept.
- **Your config, carried over.** The config layout is Kimi Code's, and `killi import-kimi`
  copies your providers and models in one go.
- **Every session on disk.** Each run is logged to `~/.killi/sessions/` exactly as it was
  sent, reasoning included.
- **Updates itself.** `killi update` installs the latest release after checking its
  checksum.

## Size and memory

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/bench-dark.svg">
  <img src="docs/bench-light.svg" width="960" alt="killi 4.3 MB installed, 13.7 MB of memory, 1,323 tokens before you type; Kimi Code 187.2 MB, 419.8 MB, 39,508 tokens; Claude Code 260.3 MB, 319.3 MB, 46,468 tokens">
</picture>

Versions measured: killi 0.1.1, Kimi Code 2.2.0, Claude Code 2.1.306, on 2026-10-22 with
[`scripts/bench.py`](scripts/bench.py). killi and Kimi Code ran the same model, MiMo-V2.6-Pro;
Claude Code ran its default, Claude Opus 5.5. Kimi Code and Claude Code also loaded this
machine's skills and MCP servers, which killi doesn't support yet: when it does, listing skills
by name and reaching MCP through one tool should add about 1,000 tokens, not tens of thousands.
Startup (`--version`): killi 4 ms, Claude Code 8 ms, Kimi Code 227 ms.

## Install

```sh
curl -fsSL https://github.com/timwuhaotian/killi/raw/main/install.sh | sh
```

This puts `killi` in `~/.local/bin` after checking its SHA-256 against
[`releases/latest.toml`](releases/latest.toml). Later, `killi update` does the same in place.

## Build from source

```sh
cargo install --path crates/killi
```

Rust 1.94 or later.

## Using killi

killi reads `~/.killi/config.toml`. If you use Kimi Code, copy its providers and models:

```sh
killi import-kimi
```

Otherwise write it by hand:

```toml
default_model = "mimo/mimo-v2.6-pro"

[providers.mimo]
type = "openai"
base_url = "https://api.example.com/v1"
api_key_env = "MIMO_API_KEY"   # or api_key = "..."

[models."mimo/mimo-v2.6-pro"]
provider = "mimo"
model = "mimo-v2.6-pro"
max_output_size = 128000
```

Then give it a task:

```sh
killi -p "the slugify test fails; fix it and run the tests"
killi -m mimo/mimo-v2.6-pro -p "..."   # pick a model for this run
killi --json -p "..."                  # print one JSON summary: tokens, cache hits, time
```

killi runs commands and edits files without asking, so use it where `git` can take you back.
It follows `~/.agents/AGENTS.md` and every `AGENTS.md` from the git root down to the working
directory, or `CLAUDE.md` where there is no `AGENTS.md`. Only OpenAI-compatible chat providers
(`type = "openai"`) work so far.

## How it works

Providers cache the start of a prompt, so a request that begins exactly like the last one is
cheaper and faster. killi is designed to keep that start identical:

- The system prompt, your `AGENTS.md` files and the tool definitions are built once when the
  session starts and never change, not even when a mode changes.
- Messages are stored exactly as they were sent, tool-call arguments and reasoning included,
  and nothing earlier is ever edited or merged. Notes from killi to the model go at the end.
- On MiMo, changing the tool list mid-session dropped the cached tokens of a request to zero,
  history included. That is why killi never adds or removes tools while it runs.

The design, with the measurements behind each choice, is in
[docs/decisions.md](docs/decisions.md).

## Development

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Releases are described in [RELEASING.md](RELEASING.md).

## License

[MIT](LICENSE)
