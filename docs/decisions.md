# killi design decisions

A personal CLI coding agent harvested from Kimi Code (`../kimi-code`).
Goals: minimal install size, minimal tokens per request, minimal memory per session, best results.

## Settled

1. **Language: Rust, single binary.** The goals are minimal size, minimal memory, and the best performance and results. This replaces an earlier plan to copy Kimi Code's TypeScript packages.
   - The user's Rust conventions (from `../kil`, `../kiled`): edition 2024, Rust 1.94; release profile `opt-level = 3`, `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`; `anyhow` and `thiserror`, `serde`, `toml`.
2. **Kimi Code is the reference, not the source.** Read its code for behavior and edge cases, reuse its prompt text as a starting draft, port its tests, and use it as the baseline in `killi eval`.
   - Provider layer: the authoritative reference is the live engine's LLM layer, `packages/agent-core-v2/src/human/llm/**`, together with `human/llm-kimi/**`, `llm-adapter/**` and `agent/llmRequester/**`. Kimi Code's engine does not use `packages/kosong`, which is a near-copy used only by the Node SDK; it's a secondary reference, and its tests still become Rust tests. Port: the reasoning-text names, tool-call ID normalization, Anthropic thinking signatures, error classification.
   - Tools: port the logic and edge cases of the tools under `packages/agent-core-v2/src/agent/tools`.
   - `grep` / `glob` run inside killi on ripgrep's own libraries (`ignore` and friends); no `rg` binary needed.
   - Renderer: a small inline renderer on `crossterm` plus `pulldown-cmark`, informed by `packages/pi-tui`. Fullscreen mode (decision 16) is the second renderer, on the same live area.
   - No embedded runtime and no Node prerequisite; `bash` runs through `std::process`; copy uses `pbcopy` / OSC 52.
3. **Test model: `mimo/mimo-v2.6-pro`.** OpenAI Chat Completions wire (kosong `type = "openai"`), token-plan endpoint; image/video/audio input, thinking, tool use; 1M context, 128k output.
   - Reasoning round-trip: keep kosong's handling of `reasoning_content` / `reasoning_details` / `reasoning`.
4. **Tools:** `read`, `write`, `edit` (`old_string` / `new_string` / `replace_all`), `bash`, `grep`, `glob`, `fetch_url`, `web_search`, images, subagents with background tasks, `ask_user_question`.
5. **Workflow:** plan mode (read-only), todo list, desktop notification when a turn finishes, undo via file history.
6. **Permissions:** no approval prompts by default. The only other mode is plan mode.
7. **Extensions:** MCP (servers start lazily, tools load on demand), skills (names only in the prompt), hooks (Claude Code event names: `PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`, `SubagentStop`, `UserPromptSubmit`).
8. **Dropped:** web UI, kap-server, remote control, vis / inspect, VS Code extension, ACP, klient / node-sdk, minidb search, transcript store, OAuth, telemetry, legacy migration, plugins and marketplace, custom agent profiles, tower / cron / goal / btw, smart bash approval (tree-sitter-bash), session index and export.

9. **Caching: the cached prefix must never break.** The token plan discounts cached input, so keeping the start of the prompt stable comes before cutting prompt size. Measured on the MiMo endpoint on 2026-10-21:
   - Automatic prefix caching, reported in `usage.prompt_tokens_details.cached_tokens`, in blocks of about 128 tokens, lasting at least several minutes.
   - Tool definitions come first in the prompt: changing the tool list caches 0 tokens, conversation history included.
   - Rules that follow:
     - The tool list is fixed for the whole session.
     - The system prompt, including the skill list and environment details, is fixed at session start.
     - Plan mode blocks writes when a tool runs instead of removing tools.
     - History is append-only: reminders go at the end, and only compaction rewrites older messages.
   - So Kimi Code's `select_tools` approach (adding tools mid-session) is ruled out.
10. **Compaction:** defaults to 256k tokens and can be changed in config. It stays well below MiMo's 1M window because cached input still costs something and very long contexts are slower and degrade quality.

11. **MCP: one gateway tool, plus per-project eager servers.** The user's servers are godot, blender, unity-mcp (stdio) and composio-youtube (HTTP).
    - The tool list holds a single `mcp` tool. Called with a server name, it starts that server if needed and returns its tool names and schemas as text; called with a server, tool and arguments, it runs that tool.
    - A project config (for example `.killi/config.toml`) can mark servers as eager. Their tools become native tool definitions from the first request and stay fixed for the session.
    - No MCP server process starts until first use.
    - Stdio servers are shared by every killi on the machine through a broker, `killi mcp-serve`, started on first use. Its socket is in the temp folder, because socket paths are limited to about 100 bytes, and its name carries the user and version. The broker initializes each server once, answers each client's `initialize` from that, and gives requests ids of its own so replies go back to the right client. When a server exits, its clients are let go, and their next call reconnects and starts it again. The broker exits 15 minutes after the last client leaves, and its servers stop when their stdin closes. Servers keep the environment of the killi that started the broker, so `env` in `mcp.json` is the place for anything that must be set. User servers start in the home folder, so one copy serves every project. If no broker answers, the server is started directly, as before.
    - `/mcp` turns servers on or off. The tool list stays fixed, so a server that is off stays in the session's `mcp` description, but calls to it are refused and the model is told at its next prompt. The choice is kept in `~/.killi/mcp-off.json` (the project's `.mcp.json` is usually checked in), so new sessions leave the server out of the description and don't start it eagerly.
12. **`fetch_url` runs locally.** Port Kimi Code's local backend (`app/web/providers/local-fetch-url.ts`, readability and linkedom, imported on first use). No Moonshot login service.

13. **Skills: names only in the prompt, plus a `skill` tool.** About 1k tokens for the user's 204 skills (full descriptions would be about 15.7k).
    - The system prompt lists skill names only. It's fixed at session start, so a skill added mid-session shows up in the next session.
    - `skill(name)` loads the full `SKILL.md`. `skill(query)` searches the descriptions locally, costing no model tokens, and returns the top 5 names with their descriptions.
    - The user can type `/skill-name` to load a skill directly.
    - Default folders: `~/.agents/skills`, plus each project's `.agents/skills` and `.claude/skills`. The list can be changed in config. `~/.claude/skills` is left out because its entries are symlinks into `~/.agents/skills`.

14. **Config: killi's own `~/.killi/config.toml`, in the same layout as Kimi Code's.** killi replaces Kimi Code rather than running alongside it.
    - Same `[providers.<name>]` and `[models."<provider>/<model>"]` layout and keys, plus `default_model` and `[thinking]`.
    - `killi import-kimi` copies providers and models once from `~/.kimi-code/config.toml`. It skips providers that sign in through Kimi's account login (`type = "kimi"`) and reports them. Started in a terminal without a config, `killi` offers this import, or `killi init`'s template when there's no Kimi Code config.
    - API keys: `api_key = "..."` or `api_key_env = "NAME"`.
    - Adapters kept: OpenAI chat (`openai`), OpenAI Responses (`openai_responses`), Anthropic (`anthropic`). Models can pick one with `protocol = ...`.
    - The user's default model is `minimax/MiniMax-M3.1-Flash`, which uses the Anthropic API. That API only caches where the request marks it, so cache breakpoints are needed from day one. Run the same cache probe on MiniMax when building that adapter.

15. **Quality checks: `killi eval` over the user's own Rust and TypeScript tasks, with Kimi Code as the baseline.**
    - 10–20 tasks, each with an automatic check: a test passes, a build succeeds, or an expected change appears.
    - Tasks are taken from git history. Check out the commit's parent, use the commit message as the prompt, and grade with the tests that commit added. The repos are private, so the models can't have memorized the answers.
    - Every run happens in a temporary git worktree, never in the real working tree.
    - Reported per task: pass or fail, fresh and cached tokens, number of requests, time taken, peak memory.
    - Baseline: the same tasks through Kimi Code's headless mode (`--output-format stream-json`). killi has to match its pass rate using fewer tokens.
    - A quick subset of about 5 tasks for each change, the full set before accepting a change to a prompt or tool. Each run spends token-plan quota.
    - So killi needs a headless print mode from phase 1.

16. **TUI:**
    - Compact lines: one line per tool call with a short result, edits show a small diff, long output hidden behind a key.
    - Thinking shown as a dim one-line summary ("thinking 4s" plus the first sentence), full text behind a key.
    - Status line: model, plan mode, context used, git branch, session tokens (fresh and cached input, output).
    - Markdown is a small streaming styler in `tui/text.rs`, not `pulldown-cmark`: it styles a line as it arrives and holds table rows until the table ends. Code is colored by a built-in keyword table, not `syntect`.
    - Colors come from `Sgr` values that pick a dark or light shade (`[ui] theme`, else `COLORFGBG`); `NO_COLOR` strips color codes where lines are written.
    - Colors are roles from `docs/killi-design-system.html`: `lagoon` for Killi (caret, running and done, modes, commands), `fin` for the human (your messages, questions), `warn`, `danger`, `fg-faint`, each the nearest 256-color shade. Every state also has a glyph (`✓ ✗ ◐ ○ ? ↳ ∴`) so it reads without color.
    - A message typed while the agent works steers it: it's added after the current tool call finishes. This is cache-safe because it goes at the end.
    - The live area is painted in one write inside a synchronized update (`CSI ?2026`), with the cursor hidden, and a tick rewrites only the rows that changed, so the status line never blinks (2026-10-22). Before, each 100 ms tick erased the area below the cursor and wrote it again, and a terminal could paint the empty frame in between: the status line was rewritten 59 times in one 4 s turn, now about 10, each when its content changed.
    - Vertical spacing is one rule in code, `App::begin(Kind)`: a blank row opens a block unless it continues the one above (prose after prose, tool calls and thoughts together, notes together). Notices, errors, questions, `/help`, `Ctrl+O` output and `/btw` answers are blocks of their own. The live area keeps a blank row above the prompt's top rule and above the task list.
    - A `/btw` answer is one block hung off the `│` gutter, like a tool's output, and waits for the model's paragraph to end so it never cuts one in two. While it is answered it is a running row in the live area.
    - The status line colors each kind of figure (`effort`, `tokens`, `branch`; the meter and mode stay `lagoon`, warn from 80%) because the user asked for it, so figures can be told apart at a glance. The design system allows these hues in the status line and the banner's effort only.

    - **Fullscreen mode** (added 2026-10-22, `[ui] fullscreen`, on by default only when `TERM_PROGRAM` is `kil`): killi takes the alternate screen, the prompt and status line stay on the bottom rows, and the conversation scrolls above them (wheel, PageUp, PageDown). The default is still inline, whose point is that the terminal's own scrollback, search and selection keep working.
      - Why: in inline mode the conversation is written into the terminal and can't be laid out again, so after a pane or tab changes width the tool lines stay cut and the prose stays wrapped for the old one, while the live area follows; and when the user scrolls back, the prompt scrolls away with everything else. A DECSTBM scroll region doesn't fix the second: the terminal's scrollback moves the whole screen, pinned rows included. Only an app that owns the screen and its scrolling can.
      - The conversation is kept as `history::Entry` values (a line, a word-wrapped line, a one-row line, or a block rendered for a width), not rows: each is laid out for the width in use and again on every resize. So prose is wrapped by `commit_wrapped`/`emit` and every width-dependent line (tool lines, the prompt block, the banner, tables, `/btw` blocks, plans) is a closure over its source. Inline mode renders the same entries at once, so one code path serves both.
      - The mouse is `?1000h` and `?1006h` only (wheel and buttons in SGR form), not crossterm's `EnableMouseCapture`, which adds `?1003` and reports every pointer move; and not `?1007`, which turns the wheel into arrow keys that the prompt already uses for its history. Elsewhere than Kil, mouse reports mean selecting text takes Shift (Option in some terminals); Kil leaves selection to itself.
      - On leaving, killi leaves the alternate screen and prints the whole conversation to the normal one, so it stays in the terminal after quitting. A panic or a signal restores the terminal but can't print it (the session log has it).
      - The history keeps the newest 20,000 entries. Widening the window lays everything out again; narrowing cuts rows by wrapping them.

17. **pi (github.com/earendil-works/pi) is a second reference and a second baseline, not the base.** Checked on 2026-10-21:
    - It's TypeScript on Node >= 22.19. A Node process that does nothing measured 43 MB peak memory; with `https`, `child_process` and `readline` loaded, 50 MB. That's the floor before any agent code. The Rust probe with HTTPS and a subprocess measured 13 MB.
    - It's no longer minimal: `coding-agent` is 359 files, `ai` 212, plus `mcp`, `codemode`, `durable`, `chord`, `env` and `telemetry`. Its dependencies include the AWS Bedrock, Google, OpenAI and Anthropic SDKs, a QuickJS WASM sandbox, and `jiti` for TypeScript extensions.
    - It's configured in JSON (`models.json`, `settings.json`), not TOML.
    - Worth reading during the build: its short prompt and tool descriptions, `core/tools/` (`edit-diff`, `truncate`, `file-mutation-queue`), `core/compaction/`, `cache-stats.ts` / `cache-warmer.ts`, the `mcp` client, and `packages/evals`.
    - `killi eval` also runs pi as a baseline. Kimi Code sets the pass rate to match; pi sets the token count to beat.

18. **Async runtime: Tokio, single-threaded, with `reqwest` (rustls, HTTP/2, streaming).** Measured on the user's release profile plus `strip`:

    | Probe (streams HTTPS and runs a subprocess) | Binary | Peak memory |
    |---|---|---|
    | Tokio + `reqwest` | 2.65 MB | 13.1 MB |
    | Plain threads + `ureq` | 1.79 MB | 12.8 MB |

    The 0.86 MB is worth it for:
    - cancellation that drops the HTTP connection immediately, so output nobody wants isn't billed;
    - easy coordination of parallel subagents, background tasks, MCP servers, steering and timeouts;
    - access to the official Rust MCP library.

    CPU-heavy search runs on the blocking pool.
19. **Faster, but no worse.** Speed and size gains never come at the cost of results.
    - Every harness behavior in Kimi Code that affects results is either ported, or dropped only when `killi eval` shows no loss. That covers request retries and error handling, tool-call parsing and repair, context assembly, reminders, truncation, compaction, and each tool's edge cases.
    - The checklist lives in `docs/reference/kimi-harness-inventory.md`. pi's equivalents are in `docs/reference/pi-harness-inventory.md`.

20. **Subagents: same model, same prompt and tool list as the main agent.**
    - Their first request starts largely cached, and quality is the same.
    - Spawning more subagents is blocked when the `agent` tool runs, not by removing the tool, because removing it would change the tool list and lose the cache.
    - Fresh context (task prompt only) by default. A fork option lets the subagent inherit the whole conversation, which is cheap because that history is already cached (see Kimi Code's `agent-fork.md`).
    - At most 4 subagents from `agent` calls run at once.
    - Swarms (added 2026-10-22): `agent_swarm` runs one fresh subagent per item of a prompt template, up to 128, `[agent] swarm_parallel` (default 8) at a time, and returns every report in item order, cut to an equal share with the full text in a file when they don't fit. `/swarm` mode is announced with appended reminders, like plan mode, so the prompt and tool list stay fixed. Unlike Kimi Code's `AgentSwarm` there's no `resume_agent_ids`: the result names failed items, and the model runs them again.
    - A cheaper subagent model can be added later if quota becomes a problem, after `killi eval` shows it doesn't hurt results.

21. **Sessions: an exact, append-only log that resumes byte-identical.**
    - Stored at `~/.killi/sessions/<project>/<id>.jsonl`, kept forever. Retention is configurable.
    - The log holds exactly what was sent, including the frozen system prompt, reasoning text and tool-call IDs, so a resume within the cache lifetime starts mostly cached.
    - Resume keeps the original system prompt. `/reload` rebuilds it on purpose, accepting one uncached request.
    - `killi -c` continues the most recent session in the current directory, `killi -r` picks from a list, and `/resume` works inside the TUI.
22. **Hooks: Claude Code's protocol, configured in TOML.** The same event names, JSON on stdin, exit code 2 blocks, and JSON decisions on stdout, so Claude Code hook scripts work unchanged. Neither `~/.kimi-code/config.toml` nor `~/.claude/settings.json` has hooks today, so this is phase 5 work.

23. **Plan mode: blocked when tools run, with known read-only bash commands allowed.**
    - `write` and `edit` are refused.
    - `bash` runs only commands starting with a built-in read-only prefix (`git log/diff/status/show`, `rg`, `ls`, `cat`, `head`, `tail`, `wc`, `cargo check/tree/metadata`, `npm ls`, ...); the list can be extended in config. Redirects, pipes into writers, and chains to unlisted commands are refused, with a message saying plan mode is on.
    - Plan mode ends when the model calls `exit_plan_mode` with its plan. The user approves it (switch to normal mode and continue) or replies with feedback (stay in plan mode). That tool is always in the tool list, and calling it outside plan mode returns an error.
    - Entered with Shift+Tab or `/plan`.

24. **Undo: hidden git snapshots of the whole working tree, taken before each turn.**
    - Stored in a separate git directory, `~/.killi/snapshots/<project>.git`, through the user's `git` binary. The user's own repo, index and history are never touched, and no git library is bundled.
    - `/undo` restores the tree to the start of any earlier turn, including changes made by bash.
    - Files excluded by `.gitignore` are skipped.
    - Folders that aren't git repos work too. The snapshot is skipped, with a notice, above about 50k files.
    - This is the approach Cline uses.
25. **Instruction files:**
    - Always load the global `~/.agents/AGENTS.md`; the user's is 459 bytes.
    - Per project, load `AGENTS.md`, falling back to `CLAUDE.md` only when there's no `AGENTS.md`. The fallback understands Claude Code's `@file` lines; several repos have a `CLAUDE.md` that is just `@AGENTS.md`.
    - Show the instruction files' token count at session start. One of the user's repos has a 47 KB `AGENTS.md`, about 12k tokens per request.
    - At session start, also load the `AGENTS.md` in each folder from the git root down to the current directory, like Kimi Code (`agent/profile/context.ts:161-206`).
    - Nested `AGENTS.md` files are found after tool calls: from paths read, edited, written, globbed or grepped, and from bash's `cwd`. killi then appends a reminder pointing to the file ("Read them before making changes in those directories"), not the file's content. Without the bash parser, folders named inside bash commands aren't detected, a small loss compared with Kimi Code.
    - An instruction file that changes on disk gets an appended reminder. The system prompt is never re-rendered.

26. **Porting rules from the two inventories** (`docs/reference/kimi-harness-inventory.md`: 549 items, 29 cache-breaking behaviors; `docs/reference/pi-harness-inventory.md`).
    - **Port the behaviors that matter for results:**
      - Retries: 10 attempts per step, 500 ms doubling to a 32 s cap plus up to 25% jitter, Retry-After honored.
      - An empty or thinking-only response is retried.
      - Tool-call IDs limited to `[A-Za-z0-9_-]` and 64 characters; duplicate incoming IDs renamed.
      - When the provider rejects a request: reduce, then strip, older media; compact and retry once on overflow.
      - On interruption, every unanswered tool call gets a placeholder result.
      - Repeated identical tool calls: reminders at 3, 5 and 8 repeats, stop at 12.
      - Read and edit share one text view (`N\t` line prefixes, exact continuation hints, CRLF handling).
      - Bash: 60 s default timeout (300 s max); on timeout the command moves to the background instead of being killed. So does what a finished command leaves running in its process group (see 33).
      - Reminders go at the end of the conversation, never into the system prompt.
    - **Avoid Kimi Code's cache breaks:**
      - The tool list is built once per session in a fixed order (Kimi sorts it with a locale-dependent comparison on every request).
      - Tool descriptions are rendered once and frozen with the system prompt; nothing is computed live from config.
      - Tool-call arguments and reasoning are stored exactly as the model wrote them and never re-serialized. The reasoning field name is learned per endpoint and saved, so it never flips mid-session.
      - Messages already sent are never merged into or edited.
    - **Fix Kimi Code's gaps instead of copying them:**
      - Malformed tool-call arguments: repair common mistakes and send back an error showing the raw arguments, never a silent `{}`.
      - Output cut off at `max_tokens`: tell the model.
      - A failed, retried stream: drop its partial output from history, as pi does. Kimi Code keeps it, possibly by accident.
    - **Take from pi:**
      - The multi-edit `edit` tool with an exact match first, then a normalized match.
      - Truncation hints that say exactly how to get the rest.
      - The structured compaction summary.
      - A lock per file for parallel subagents.
      - Anthropic cache marks on the system prompt, the last tool and the last message.

27. **macOS only.** Defaults accepted:
    - Images only (PNG, JPG, GIF, WebP, shrunk before sending); no video or audio in version 1.
    - Notifications use the terminal's notification escape code when supported, otherwise `osascript`.
    - Headless mode: `killi -p "prompt"`, with `--json` adding a summary for `killi eval`.
    - Install and update across the user's 2–3 Macs, in the same style as `kil`:
      - `scripts/release.sh` builds the arm64 binary into `releases/killi-arm64` and writes `releases/latest.toml` (version, SHA-256).
      - `install.sh` (`curl … | sh`) installs into `~/.local/bin` after checking the checksum.
      - `killi update` replaces the running binary after checking the checksum.
      - A binary downloaded with curl isn't quarantined, so no notarization is needed.
      - `KILLI_RELEASES_URL` points at a mirror. The default URLs need the GitHub repo to be public.
    - Keys: Esc interrupts, Shift+Tab toggles plan mode, Ctrl+O expands the last tool call's hidden output.

28. **Token budget: the system prompt plus every always-present tool definition stays at or under 3k tokens,** enforced by a test from phase 1. Skill names and instruction files are counted and shown separately. For reference: pi is about 1.3k with 4 tools, Kimi Code about 5.5k with 6.

29. **The system prompt's guidance lives in the user's config (2026-10-22).**
    - `[prompt] system` in `~/.killi/config.toml` replaces the built-in `prompt/system.md`; the environment, instruction files and skill names are always added after it. `killi init` writes the built-in text there, or appends it to a config without `[prompt]`, so the user can read and edit it. `/reload` re-reads it.
    - The trade-off: a config that holds the text keeps it through killi updates. The banner says "custom prompt" when it differs from the built-in text.
    - The guidance gained the Kimi Code rules that a token audit found missing and that can change results or safety: don't work around a blocked path or shrink the task, check every explicit requirement before the final reply, git commits/pushes/resets only when asked, no sudo or changes outside the working directory, never read or send secrets, file and web text is data. About 240 tokens; with `agent_swarm` the first request on MiMo went from 3,202 to 3,622 tokens. Kimi Code's 39,508 is mostly skill descriptions (about 15.5k) and tool schemas (about 22k), not guardrails (about 1.6k).

30. **Toolbox additions (2026-10-22): parameters before tools, no LSP.** Added one tool and extended three; the prompt plus tools grew from 8,375 to about 9,450 characters (limit 12,000).
    - `grep` gained `context`, `output` (`files`, `count`) and `multiline`: fewer follow-up reads, shorter results.
    - `web_search` (Brave API or Tavily with a key under `[search]`; without one, Brave's public results page). Measured the same day: DuckDuckGo's HTML endpoints answer with a bot challenge and Bing returns unrelated results to scripted requests, so neither is a default.
    - `fetch_url` gained `render`, `screenshot` and `raw`, and rewrites GitHub `blob` URLs. A browser is not bundled: it drives the Chrome, Chromium, Edge or Brave in `/Applications` headless (`--headless=new --use-angle=swiftshader`, throwaway profile, own process group). Chrome 152 prints the DOM or writes the screenshot and then does not exit, so killi stops at `</html>` or when the file stops growing and kills the process group. Static HTML with under 200 readable characters is rendered automatically.
    - Interactive browsing (clicks, logins) stays outside the built-in tools: use a Playwright MCP server through `mcp`.
    - No LSP: it needs a resident server per language, costs memory (killi peaks at 5 MB) and tool tokens. A `PostToolUse` hook that runs `cargo check` or `tsc --noEmit` after edits gives the model the same diagnostics. Revisit if `killi eval` shows failures an LSP would fix.

31. **Findings from reading 31 real session logs (2026-10-22).**
    - A usage-limit 429 (`Usage limit reached for 5 hour … reset at …`) is not a rate limit: retrying ten times with backoff waited about two minutes for a limit that resets in hours. It fails at once with the provider's message, as does any retryable status whose `Retry-After` is over a minute.
    - Muse (Meta) models streamed `reasoning_tokens` but no reasoning on chat/completions, so the thinking was lost between steps and one session repeated the same call 13 times. On the Responses API the same endpoint returns encrypted `reasoning` items, which killi already keeps and replays; setting the provider's `type = "openai_responses"` fixed it (cached 2,804 of 3,080 tokens on the second request). The log now says once when reasoning is billed but never arrives.
    - Claude on a company gateway (OpenAI-format, in front of Bedrock) never cached: with no `cache_control` there are no cache tokens, and with `cache_control` the first request writes the cache (about 25% dearer) but the next ones, with the same prefix 3 s later, bill in full. No `cache_control` option was added; use the `anthropic` provider for Claude until the gateway's cache reads work.
    - The repeat breaker stops at 8 identical calls, not 12: the warnings at 3, 5 and 7 were ignored by the model that looped. It counts by call and now notes when the result was also the same.
    - Logs gained `ts` and durations because nothing in them could tell a slow model from a slow tool. They sit beside each record's data, so a message is still stored exactly as sent.
    - A resumed session prints its conversation from the log: the model always had it, the screen didn't, which read as if nothing was loaded. Tool results kept how they ended, so a replay shows ✗ for what failed.

32. **Images in the prompt, and a banner that keeps up (2026-10-22).**
    - Ctrl+V reads the clipboard with `osascript`: a file copied in Finder first (or its icon would be taken), then the image as PNG, which macOS offers for TIFF too. Large images are shrunk the same way the `read` tool shrinks them. About 0.5 s.
    - Cmd+V belongs to the terminal, and a terminal pastes nothing for an image. Kil 0.0.13 sends an empty bracketed paste instead, and killi takes an empty paste to mean "read the clipboard's image". In other terminals Ctrl+V is the way.
    - An image stands in the prompt as `[Image #N]`, numbered across the session so a recalled prompt can't point at a newer image. Deleting the placeholder drops the image. The prompt goes to the model as the usual string message, then a user message with `[Image #N]:` and the image, the same shape tool images already used, so history, `/resume`, compaction and the Anthropic adapter's merging needed no change.
    - The banner is drawn again after `/model`, `/effort` or `/reload` while its top row is still on screen. killi keeps the lines printed since the banner until it scrolls off (counting wrapped rows), moves up to it and prints it all again. A resize or Ctrl+L stops this, since the terminal reflows what it shows.

33. **Sessions clean up after themselves; commands while the agent works (2026-10-22).**
    - killi remembers the process group of every `bash` command, not only of background tasks. A finished command whose group still has processes, such as `nohup server > log &`, becomes a background task at once and its result says so; before, killi forgot it and it outlived the session. The check is `killpg(pgid, 0)`; once the shell is reaped, a live process holding the group's id means the id was reused, and the group counts as gone.
    - A completed turn in the TUI (not an interrupted one, not a subagent's) that leaves tasks running gets one more step: a reminder lists them and asks the model to stop what nothing needs or say why it stays. Each task comes up once. Code finds them, so a turn that leaves nothing costs no tokens; asking the model at the end of every turn would cost a request each time and have it guess with `ps` and `pkill`.
    - Ending a session (`/exit`, `/new`, `/resume`, the end of a `-p` run) kills what still runs and names it; `-p` skips the reminder, since asking the model to keep a server would be a promise its exit breaks. `--json` lists them as `stopped`. SIGHUP (the terminal window closed), SIGTERM and SIGINT (Ctrl+C under `-p`) used to end killi without running destructors, leaving the groups behind; now they kill them and restore the terminal first.
    - What leaves the process group (`docker compose up -d`, `docker run -d`, `brew services`, daemons that call `setsid`) can't be tracked this way, so the prompt asks the model to stop those. Tagging processes with an environment variable didn't work: `KERN_PROCARGS2` gave no environment for another process on macOS 27.
    - The slash menu shows while the agent works, offering only what runs then: `/btw`, `/plan`, `/swarm` on or off, `/later` and `/help`. The other commands need the agent, which the running turn holds. Choosing `/btw` from the menu leaves it in the prompt for the question instead of running it empty.

## Build phases

0. **Scaffold:** a Cargo workspace following `kil`'s conventions (edition 2024, Rust 1.94, its release profile plus `strip`); TOML config; `killi import-kimi`.
1. **Core, headless:**
   - The OpenAI chat adapter (MiMo): streaming, reasoning round-trip, retries, argument repair.
   - The agent loop with the frozen prompt and tool list, and the core tools (`read`, `write`, `edit`, `bash`, `grep`, `glob`) with truncation.
   - The session log, `killi -p`, and `killi eval` with the Kimi Code and pi baselines.
   - First measurements: tokens, cache hit rate, peak memory.
2. **TUI:** compact layout, thinking summary, status line, steering, Esc, plan mode with `exit_plan_mode`, todo, notifications, undo snapshots.
3. **Resume and compaction; the Anthropic adapter** (MiniMax, cache marks, cache probe) **and the Responses adapter.**
4. **More tools:** `fetch_url`, images, `ask_user_question`, subagents (fork, limit of 4, file locks), background tasks.
5. **Extensions:** the MCP gateway plus eager servers, the `skill` tool, hooks, nested `AGENTS.md` reminders.

## Status

All build phases are done as of 0.2.0 (2026-10-22): the headless core, the interactive TUI,
the Anthropic and OpenAI Responses adapters, compaction, resume, plan mode, subagents,
background tasks, undo, `fetch_url`, images, skills, MCP, hooks and nested `AGENTS.md`
reminders. `scripts/eval.py` is the `killi eval` of decision 15, run with Python so the same
harness can drive Kimi Code and Claude Code.

Measured:
- Release binary: 6.0 MB (4.3 MB before the TUI, `fetch_url` and MCP).
- First request on MiMo, on the user's machine: 3,224 tokens, of which the 204 skill names are
  about 860 and the global `AGENTS.md` 130; Kimi Code's was 39,508.
- Capability tests (`docs/eval.md`), 14 tasks: killi 0.2.1, Kimi Code 2.2.0 and Claude Code
  2.1.306 all passed 14 of 14. Per task, killi used 27.6k input tokens (4.9k not cached),
  Kimi Code 206.8k on the same model and Claude Code 278.9k; killi's own process peaked at
  5 MB against 335 MB and 191 MB.
- The top-level listing added in 0.2.1 cut killi's input tokens per task by 16% against
  0.2.0, with the same pass rate.

0.2.2 added the slash menu. It ranks with pi-tui's fuzzy matcher, which Kimi Code uses,
ported as is, and keeps Kimi Code's keys: Enter runs a command, Tab only completes, Esc
closes. Two additions: entries whose description contains the typed word follow the name
matches, and after `/model ` or `/effort ` the menu offers the choices, starting on the current
one. `/effort` changes only a request parameter, so the cached prefix still hits: on MiMo the
request after switching from high to low was 96% cached.

Later, Tab became a second Enter in the menu and in every list, at the user's request: picking
from a list with Tab and having nothing happen read as broken. To give a skill arguments, type
its name and a space. A model or effort pick now outlives the session. `/model` and `/effort`
write it to `~/.killi/last-model.json`, and a new session starts on it: `-m` first, then that
pick, then `default_model`. Only an explicit pick is saved, so editing `default_model` still
takes effect until the next pick. The session log notes each switch as a `model` entry, so a
resume continues on the model and effort the conversation last ran on. The file answers "what
should new sessions use", the log answers "what was this conversation on", and neither reads
the other.

`/btw` asks a side question, as in Claude Code. It sends the history as it stands plus the
question, with the same tools, so the cached prefix hits. The model is told not to call tools.
Neither the question nor the answer goes into history or the log, so the main thread's cache
and context stay as they were. It works mid-turn, which is when it is useful. The running
agent can't be borrowed then, so it shares its history with the TUI through `Controls` as a
`Weak` to an `Rc<Vec>`. Appending never copies, and the question copies the history once. Tool
calls still running get a placeholder result in that one request only.

Seen in testing, left as they are:
- MiMo now and then writes its native tool-call markup (`<tool_call><function=…>`) into the
  reply text and sends a broken call; the invalid-arguments error lets it recover on the next
  step. Kimi Code doesn't handle this either.
- Paths that look like `-Users-<name>-…` in the working directory lead the model to guess other
  paths under `/Users/<name>/`. Real projects don't have such paths.

## Baseline to beat

- Kimi Code's system prompt plus the six core tool descriptions: about 22 KB, roughly 5.5k tokens per request. On MiMo, `system.md` plus the bash, read and glob descriptions measured 4,112 prompt tokens.
- pi, rebuilt by hand from its source: system prompt about 676 tokens (about 346 without its section on pi's own docs); prompt plus its 4 default tools (read, bash, edit, write) about 1.3k tokens; each tool about 90–280 tokens. Details in `docs/reference/pi-harness-inventory.md`.

## Open

None. All design branches were settled on 2026-10-22.
