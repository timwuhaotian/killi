# Changelog

Notable changes to killi. `install.sh` and `killi update` always install the latest release
from [`releases/`](releases); the versions below match `Cargo.toml`.

## Unreleased

## 0.2.13 — 2026-10-22

- **Sessions share MCP servers.** Stdio servers now run in one shared `killi mcp-serve`
  process, so every killi on the machine uses the same server instead of starting its own. It
  starts on first use, as servers always have, and exits 15 minutes after the last session
  lets go. A server that exits is started again on the next call. User servers from
  `~/.killi/mcp.json` start in the home folder, project servers in the project root.
- **`/mcp`** lists the MCP servers and turns one on or off with Enter, for this session and
  new ones; `/mcp <server>` toggles one directly. The model is told, and calls to a server
  that is off are refused.
- **Choosing a skill in the slash menu no longer runs it.** Tab or Enter puts `/<skill> ` in
  the prompt so you can add a request, then Enter sends it. Built-in commands still run at once.

## 0.2.12 — 2026-10-22

- **`/btw <question>`** asks a side question about the conversation, also while the agent
  works: type it and press Enter mid-turn. The answer shows in the transcript, and neither the
  question nor the answer is added to the conversation.
- **New sessions start on your last pick.** The model and effort last chosen with `/model` or
  `/effort` are remembered in `~/.killi/last-model.json`, and a new session starts on them
  unless `-m` names a model. `/new` keeps the model and effort in use, and a resumed session
  continues on the model and effort it last ran on, as the log now records each switch.
- **The status line shows the model the agent runs on**, also after `/new` and `/resume`,
  which used to go back to the startup model while it showed the switched one. `/model` now
  says that the conversation carries over.
- **Tab selects like Enter** in the slash menu, the model, effort and session lists, and the
  agent's questions.

## 0.2.11 — 2026-10-22

- **The logo as the spinner.** While the agent thinks, runs a tool or works, a `killi`
  wordmark replaces the braille dots: the letters fade in one by one, then the banner's
  gradient drifts through them as a bright spot sweeps across.

## 0.2.10 — 2026-10-22

- **Responsive status line.** In a narrow terminal the status line drops cache and token
  detail, then the context bar and the branch, instead of cutting off at the edge; the model
  and mode always stay.
- **Timers move up a unit.** Elapsed times read `59s`, then `1m05s`, then `1h02m`, instead of
  a growing count of seconds.

## 0.2.9 — 2026-10-22

- **`web_search`.** Searches Brave, through its API or Tavily's with a key under `[search]`,
  or, with none, Brave's public results page.
- **`fetch_url` renders pages.** `render` runs a page's JavaScript in headless Chrome (also
  tried automatically when the static HTML has no readable text), `screenshot` returns a
  picture of the page for checking a local dev server's UI, `raw` returns the body as is, and
  GitHub `blob` URLs fetch the raw file.
- **Better `grep`.** `context` lines around matches, `output` modes `files` and `count`, and
  `multiline` patterns, so fewer follow-up `read` calls.

## 0.2.8 — 2026-10-22

- **Your message shows at once.** Pressing Enter puts the message in the transcript before the
  undo snapshot is taken, so it no longer vanishes for a moment in large repositories.
- **Slash menu anywhere.** A `/` that starts a word, not only the first one, offers commands
  and skills, and known `/commands` in the prompt are drawn in the accent color.

## 0.2.7 — 2026-10-22

- **Richer Markdown.** Tables line up and shrink to the window, links show their address,
  code blocks get a gutter and a language label with colors for Rust, JS/TS, Python, shell,
  Go, JSON, TOML/YAML and diffs, and there is *italic*, ~~strikethrough~~, nested bullets,
  task lists and numbered-list styling.
- **Themes and `NO_COLOR`.** `[ui] theme = "dark" | "light"` picks the palette; without it
  killi reads `COLORFGBG`. `NO_COLOR` turns colors off and keeps bold and dim.
- **Easier to scan.** Your messages sit on a tinted band, the selected row of a menu is
  highlighted, pickers have a titled rule, bash calls show the first lines of their output
  (the last lines when they fail) and how long they took, `Ctrl+O` steps back through the last
  ten outputs, the task list shows a count, the status line groups its parts, quick turns
  skip the cost line, and common errors (key, rate limit, model, network, full context) add a
  hint.

## 0.2.6 — 2026-10-22

- **Quitting says how to come back.** When an interactive session with prompts ends, killi
  prints `killi -r <log>` to continue it.
- **`/clear` clears the screen.** `/clear` (and `/new`) now clear the screen and scrollback and
  show the banner again; the last session stays saved and `/resume` reopens it.

## 0.2.5 — 2026-10-22

- **Pickers filter as you type.** In `/model`, `/effort` and `/resume`, typing narrows the
  list with the slash menu's fuzzy matcher (`q38` finds `qwen/qwen3.8-max`), best match
  first; Backspace widens it again.

## 0.2.4 — 2026-10-22

- **First run sets you up.** Started in a terminal without a config, `killi` offers to import
  Kimi Code's providers, models and MCP servers and then starts; without Kimi Code it offers
  to write the starting config to edit. `killi -p` and piped runs still just say what to run.

## 0.2.3 — 2026-10-22

- **Swarms.** The `agent_swarm` tool runs one subagent per item of a prompt template (up to
  128, 8 at a time; `[agent] swarm_parallel` changes that) and returns every report in item
  order. `/swarm` turns swarm mode on or off, telling the agent to split its work this way;
  `/swarm <task>` runs one task in it.
- **The system prompt in your config.** Its guidance is under `[prompt] system` in
  `~/.killi/config.toml`, to read and edit; `/reload` applies changes. `killi init` adds it to an
  existing config.
- **Safety rules in the prompt:** git commits, pushes and resets only when asked, no secrets
  read or sent, no quiet workarounds or shrinking the task when something is blocked, and a
  check of every explicit requirement before the final reply.
- In `killi -p`, a subagent's first line no longer runs onto the line of the call that
  started it.
- **First run.** Without a config, killi says to run `killi init` or `killi import-kimi`
  instead of showing an OS error, and the installer suggests `killi init` too.

## 0.2.2 — 2026-10-22

- **Slash menu.** Typing `/` opens a menu of commands and skills that narrows as you type,
  ranked with the fuzzy matcher Kimi Code uses, so `/rev` finds `code-review`; skills whose
  description mentions the word follow. ↑↓ choose, Tab completes, Enter runs, Esc closes.
  After `/model ` or `/effort ` it offers the models or efforts, starting on the current one.
- **`/effort`** (or `/thinking`) switches the thinking effort for the session, from a list or
  as `/effort high`; the status line shows it. `/new` is the main name for `/clear`, and
  `/quit` and `/q` exit too. `/skill:<name>` runs a skill, as in Kimi Code.
- **A banner** at start: killi in gradient block letters, with the version, model, effort and
  folder beside it.
- **Tidier output.**
  - The model's text wraps between words, with list items indented under their text.
  - Code blocks show without their fences.
  - Edits show a line diff with line numbers and a little context, instead of the whole old
    and new text.
  - Tool lines stay on one line, with paths relative to the folder and a leading
    `cd <folder> &&` left out.
  - Retry notices show the provider's message instead of the raw JSON.
- **Lists and the prompt.**
  - The model and effort lists and the agent's questions open under the prompt, in the
    same style as the slash menu.
  - The prompt sits between two rules, which turn yellow in plan mode.
  - The status line has a context gauge.

## 0.2.1 — 2026-10-22

- `killi init` writes a starting `~/.killi/config.toml` for machines without Kimi Code.
- The system prompt lists the top of the working directory, so the agent starts without an
  `ls` round trip, as Kimi Code does.
- A message typed during the agent's final reply is answered in the same turn instead of
  waiting for your next prompt.
- `/undo` and other commands typed while the agent works stay in the prompt instead of being
  sent to the model.
- Cleaner transcript: no doubled blank lines, and short error summaries end on a whole word.
  The live area never grows taller than the window, and resizing no longer shifts it.
- Resumed and compacted sessions list your earlier prompts again, in the banner, the history
  and `/resume`.

## 0.2.0 — 2026-10-22

- **Interactive mode.** Run `killi` with no arguments: the conversation prints into your
  terminal's scrollback, with a live area at the bottom for what's happening, the task list,
  the prompt and a status line (model, plan mode, context used, git branch, tokens).
  - Type while the agent works and your message reaches it at its next step; Esc interrupts.
  - Thinking shows as one dim line; Ctrl+O prints the last thought or tool output in full.
  - Edits show a small diff; every tool line says what it found ("412 lines", "7 matches").
  - A desktop notification when a turn takes longer than 10 seconds.
- **Plan mode** (Shift+Tab or `/plan`): the agent researches and plans, writes are blocked and
  bash runs only read-only commands, until you approve its plan.
- **Anthropic and OpenAI Responses providers** besides OpenAI chat: MiniMax, Claude and any
  `type = "anthropic"` or `protocol = "openai_responses"` model work, with prompt caching.
- **Subagents**: the `agent` tool runs up to 4 in parallel on the same model and cached prompt;
  `fork: true` gives one the whole conversation.
- **Background tasks**: `bash` with `run_in_background`, or any command that outlives its
  timeout, keeps running; `task_output` and `task_stop` manage it.
- **Sessions**: `killi -c` continues the last session here, `killi -r` or `/resume` picks one;
  long conversations compact themselves (or `/compact`), keeping your requests word for word.
- **Undo**: `/undo` puts files back as they were before the last turn, including changes made
  by commands, using a separate git store under `~/.killi/snapshots/`.
- **Skills** from `~/.agents/skills` and the project: names in the prompt, loaded on demand, or
  run as `/skill-name`. **MCP servers** through one `mcp` tool, started on first use.
  **Hooks** with Claude Code's protocol, configured in `[[hooks.<Event>]]`.
- `fetch_url` reads web pages as Markdown; `read` shows images to models that can see them;
  `todo_write` and `ask_user_question` tools.
- Nested `AGENTS.md` files are pointed out when the agent works in their folders.

## 0.1.1 — 2026-10-22

- **Tidier output.** Tool calls are one line each, the model's text sits between them with a
  blank line on either side, and the header and summary are dimmed when printing to a
  terminal. `NO_COLOR` turns colors off.
- Commands shown in tool lines print the working directory as `.`.
- The model is told each command starts in the working directory, so it stops prefixing
  `cd /full/path &&`.

## 0.1.0 — 2026-10-22

- First release: `killi -p` runs one task with any OpenAI-compatible model, using `read`,
  `write`, `edit`, `bash`, `grep` and `glob`.
- `killi import-kimi` copies providers and models from Kimi Code's config.
- `killi update` and `install.sh` install the latest release after checking its checksum.
- `--json` prints one summary with tokens, cache hits and time.
