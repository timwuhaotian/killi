---
name: update-config
description: Read, explain or change killi's own configuration: ~/.killi/config.toml (default_model, providers, models, thinking, agent, skills, hooks, mcp, prompt, ui, search), the MCP server files, the project's .killi/config.toml and the other files under ~/.killi. Use when asked where killi's config is, what a setting does, or to add or change a model, provider, API key, hook, MCP server or the system prompt.
---

# killi's configuration

Built into this killi, so it describes the version running. First read the files below that
exist and say which you found; then explain or edit. Never show an API key: shorten it to its
first few characters. `~` is `$HOME`; nothing else moves these paths.

## Files

| Path | Holds |
|---|---|
| `~/.killi/config.toml` | the config, TOML; everything under "Keys". `killi init` writes a starting one (mode 0600), `killi import-kimi` copies Kimi Code's, and the first-run guide can import Kimi Code's, pi's or opencode's |
| `~/.killi/mcp.json` | user MCP servers, `{"mcpServers": {...}}` as Claude Code and Kimi Code write it |
| `<project>/.mcp.json` | project MCP servers, same format; the project is the nearest folder with `.git` |
| `<project>/.killi/config.toml` | project config; only `[mcp] eager = ["name"]` is read: servers to start with the session, with native tools, after a one-time trust check per repo (kept in `~/.killi/trusted.json`) |
| `~/.killi/mcp-off.json` | servers turned off with `/mcp`, kept for later sessions |
| `~/.killi/command-guide.json` | that the first-run guide already asked which command name to install killi under |
| `~/.killi/last-model.json` | the last `/model` pick; new sessions start on it unless `-m` names a model (the effort is per model: `default_effort` in its `[models]` entry, which `/effort` writes) |
| `~/.killi/sessions/<folder>/` | session logs (`-c`, `-r`, `/resume`, `/prune`) and `later.md`; `<folder>` is the working directory with `/` as `-` |
| `~/.killi/snapshots/` | the bare git repositories `/undo` restores from |
| `~/.agents/skills/`, `<project>/.agents/skills/`, `<project>/.claude/skills/` | skills: folders with a `SKILL.md`; the first with a name wins, and `update-config` is built in |
| `~/.agents/AGENTS.md`, then `AGENTS.md` (or `CLAUDE.md`) from the project root down to the working directory | instructions in the system prompt |

## Keys in `~/.killi/config.toml`

A model is named `<provider>/<model>`, picked with `-m`, `/model` or `default_model`.

- `default_model = "alias"` names an entry under `[models]`. Without it, `-m` is required.
- `[providers.<name>]`: `type` ("openai" for Chat Completions and compatible APIs,
  "anthropic" for Messages, "openai_responses" for Responses, "chatgpt" for the user's
  ChatGPT plan: no key, `killi login chatgpt` signs in and adds the plan's models), `base_url`
  (default `https://api.openai.com/v1`, or `https://api.anthropic.com` for anthropic), one of
  `api_key` or `api_key_env` (the name of an environment variable, which wins when both are
  set), `custom_headers = { "X-Name" = "value" }`, and `session_header` (a header that carries
  the conversation's ID on every request, such as OpenCode Go's `x-opencode-session`).
  `killi login opencode-go` writes an OpenCode Go provider and its models from the key.
- `[models."<provider>/<model>"]`: `provider` (a `[providers]` name), `model` (the API's
  model id), `protocol` (overrides the provider's `type` for this model), `base_url` (overrides
  the provider's, for a gateway with a URL per API), `cache_control = true`
  (Chat Completions only: mark the system message and the newest message for a gateway that
  passes it to Claude on Bedrock; without it that model re-bills the whole prompt each turn),
  `max_context_size`
  and `max_output_size` (tokens), `capabilities` (`"image_in"` lets the model take images;
  other entries are kept but unused), `default_effort` (the model's own thinking level; `/effort`
  saves the last pick here) and `support_efforts = [...]` (what `/effort` offers; without them:
  off, on, low, medium, high).
- `[thinking]`: `enabled = false` turns reasoning off everywhere; otherwise each model reasons
  at its `default_effort`, else the middle of its `support_efforts`, else the provider's default.
- `[agent]`: `compact_at` (compact once a request passes this many input tokens; unset: 60%
  of the model's `max_context_size`, or 256000 when the window is unknown), `plan_allow = ["make -n"]` (extra command prefixes bash may run in plan mode),
  `notify_after` (seconds a turn takes before a desktop notification, default 10),
  `swarm_parallel` (subagents one `agent_swarm` call runs at once, default 8),
  `swarm = false` (start sessions with swarm mode off; it is on by default, and `/swarm on|off` changes it),
  `peers = false` (keep this machine's other killi sessions from seeing this one, asking it about its work and messaging it, and don't tell the model about the sessions in its repository; on by default).
- `[skills] dirs = ["~/other/skills"]`: skill folders used instead of `~/.agents/skills`; the
  project's own folders are always read.
- `[[hooks.<Event>]]` with `matcher` (a regex over tool names; none matches every tool),
  `command` (sh, with the event as JSON on stdin) and `timeout` (seconds, default 60). Events:
  `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`,
  `SessionEnd` (runs when the session exits, only if a turn ran). Exit code 2 blocks and
  sends stderr to the model; a `PostToolUse` hook's output goes to the model too.
- `[mcp] eager = ["name"]`: servers that start with every session and get native tool
  definitions; other servers start when first used through the `mcp` tool.
- `[prompt] system = '''...'''`: the system prompt's guidance in place of the built-in text;
  `killi init` adds the built-in text to a config without it. Blank means built in.
- `[ui]`: `theme` ("dark" or "light"; without it killi asks the terminal, then reads
  `COLORFGBG`), `fullscreen = true` (take the whole screen, with the prompt on the bottom rows;
  without it only Kil does that), `recap = false` (no automatic 100-character recap after a
  turn that used tools; `/recap` still asks for one).
- `[search]`: `provider` ("brave", the default, "tavily" or "searxng"), `url` (the SearXNG
  instance) and `api_key` or `api_key_env` for `web_search`. `web_search` tries the
  configured provider first (a transient failure — 429, 5xx, network — is retried once),
  then falls back to any other source the table makes usable and to Brave's public page
  last, which needs no key and can refuse; one `api_key` serves one provider, so a keyed
  fallback to another API is not picked up from this table. "searxng" asks
  `{url}/search?q=…&format=json`, so the instance must enable the json format.
  `/web-search-config` asks for all of this and writes the table itself; picking SearXNG
  without an instance offers to start one with Docker (container `killi-searxng`, localhost
  only, settings killi writes under `~/.killi/searxng`), reusing anything already answering
  on the port.

An MCP server in `mcp.json` has `command`, `args`, `env` (stdio) or `url`, `headers`,
`bearerTokenEnvVar` (HTTP), plus `enabled` (false leaves it out) and `toolTimeoutMs`.

## Entire checkpoints

`entire agent add` has no killi entry, but killi speaks Claude Code's hook protocol, so the
`claude-code` handlers work unchanged. Paste this into `~/.killi/config.toml` — a project's
`.killi/config.toml` only reads `[mcp]`, so hooks must live in the user config. Without the
`entire` CLI on PATH each hook exits 0 at once, so the same config is safe everywhere:

```toml
[[hooks.SessionStart]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code session-start"
timeout = 30

[[hooks.UserPromptSubmit]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code user-prompt-submit"
timeout = 30

[[hooks.PreToolUse]]
matcher = "agent|agent_swarm"
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code pre-task"
timeout = 30

[[hooks.PostToolUse]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code post-task"
timeout = 120

[[hooks.Stop]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code stop"
timeout = 120

[[hooks.SubagentStop]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code subagent-stop"
timeout = 120

[[hooks.SessionEnd]]
command = "command -v entire >/dev/null 2>&1 || exit 0; exec entire hooks claude-code session-end"
```

Like other `[hooks]` changes, it takes the next killi start.

## Making a change

Edit the file with the edit tool, keep other keys as they are, and say how the change takes
effect:

- `[prompt]`, `[skills]`, `AGENTS.md` files, skill folders: `/reload` in the running session.
- `[providers]`, `[models]`, `[thinking]`: `/reload`, then `/model` or `/effort` picks them
  up; sessions started afterwards too.
- `[search]`: `/web-search-config` sets and applies it at once, in this session and new ones;
  a hand edit takes the next killi start (`/exit`, then `killi -c` continues this session).
- `[agent]`, `[hooks]`, `[mcp]`, `[ui]` and the MCP files: the next killi start
  (`/exit`, then `killi -c` continues this session).
