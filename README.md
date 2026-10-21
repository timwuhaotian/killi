# killi

A minimal coding agent for the terminal, built for one person's workflow: a single ~4 MB native binary for Apple Silicon Macs, a short prompt, and requests shaped so the provider's prompt cache keeps hitting.

## Install

```sh
curl -fsSL https://github.com/timwuhaotian/killi/raw/main/install.sh | sh
```

This puts `killi` in `~/.local/bin` after checking its SHA-256. Update later with:

```sh
killi update
```

To build from source instead: `cargo install --path crates/killi`.

## Configure

killi reads `~/.killi/config.toml`, in the same layout as Kimi Code's config. To copy your Kimi Code providers and models:

```sh
killi import-kimi
```

Or write it by hand:

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

Only OpenAI-compatible chat providers (`type = "openai"`) work so far.

## Use

```sh
killi -p "fix the failing test in src/lib.rs"
killi -m mimo/mimo-v2.6-pro --json -p "..."   # one JSON summary: tokens, cache hits, time
```

killi runs commands and edits files without asking. It loads `~/.agents/AGENTS.md` and each `AGENTS.md` from the git root down to the working directory (or `CLAUDE.md` where there is no `AGENTS.md`). Sessions are logged to `~/.killi/sessions/`.

The interactive TUI, more providers, and the rest of the plan are in [docs/decisions.md](docs/decisions.md).

## Release

Bump `version` in `Cargo.toml`, run `scripts/release.sh`, and commit `releases/`. Installed copies pick it up with `killi update`.
