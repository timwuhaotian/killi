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
   - Renderer: a small inline renderer on `crossterm` plus `pulldown-cmark`, informed by `packages/pi-tui`.
   - No embedded runtime and no Node prerequisite; `bash` runs through `std::process`; copy uses `pbcopy` / OSC 52.
3. **Test model: `mimo/mimo-v2.6-pro`.** OpenAI Chat Completions wire (kosong `type = "openai"`), token-plan endpoint; image/video/audio input, thinking, tool use; 1M context, 128k output.
   - Reasoning round-trip: keep kosong's handling of `reasoning_content` / `reasoning_details` / `reasoning`.
4. **Tools:** `read`, `write`, `edit` (`old_string` / `new_string` / `replace_all`), `bash`, `grep`, `glob`, `fetch_url`, images, subagents with background tasks, `ask_user_question`.
5. **Workflow:** plan mode (read-only), todo list, desktop notification when a turn finishes, undo via file history.
6. **Permissions:** no approval prompts by default. The only other mode is plan mode.
7. **Extensions:** MCP (servers start lazily, tools load on demand), skills (names only in the prompt), hooks (Claude Code event names: `PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`, `SubagentStop`, `UserPromptSubmit`).
8. **Dropped:** web UI, kap-server, remote control, vis / inspect, VS Code extension, ACP, klient / node-sdk, minidb search, transcript store, OAuth, telemetry, legacy migration, plugins and marketplace, custom agent profiles, tower / swarm / cron / goal / btw, smart bash approval (tree-sitter-bash), session index and export.

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
12. **`fetch_url` runs locally.** Port Kimi Code's local backend (`app/web/providers/local-fetch-url.ts`, readability and linkedom, imported on first use). No Moonshot login service.

13. **Skills: names only in the prompt, plus a `skill` tool.** About 1k tokens for the user's 204 skills (full descriptions would be about 15.7k).
    - The system prompt lists skill names only. It's fixed at session start, so a skill added mid-session shows up in the next session.
    - `skill(name)` loads the full `SKILL.md`. `skill(query)` searches the descriptions locally, costing no model tokens, and returns the top 5 names with their descriptions.
    - The user can type `/skill-name` to load a skill directly.
    - Default folders: `~/.agents/skills`, plus each project's `.agents/skills` and `.claude/skills`. The list can be changed in config. `~/.claude/skills` is left out because its entries are symlinks into `~/.agents/skills`.

14. **Config: killi's own `~/.killi/config.toml`, in the same layout as Kimi Code's.** killi replaces Kimi Code rather than running alongside it.
    - Same `[providers.<name>]` and `[models."<provider>/<model>"]` layout and keys, plus `default_model` and `[thinking]`.
    - `killi import-kimi` copies providers and models once from `~/.kimi-code/config.toml`. It skips providers that sign in through Kimi's account login (`type = "kimi"`) and reports them.
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
    - A message typed while the agent works steers it: it's added after the current tool call finishes. This is cache-safe because it goes at the end.

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
    - At most 4 subagents run at once.
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
      - Bash: 60 s default timeout (300 s max); on timeout the command moves to the background instead of being killed.
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

The first slice (phase 0 plus the headless core of phase 1) was built on 2026-10-22:
- config, `import-kimi`, the OpenAI chat adapter, the agent loop;
- `read` / `write` / `edit` / `bash` / `grep` / `glob`;
- the session log, `-p` / `--json`, `update`, `install.sh`, `scripts/release.sh`.

Measured on MiMo:

| Measure | Result |
|---|---|
| Release binary | 4.33 MB |
| First request (system prompt, 6 tools, 130-token global `AGENTS.md`, user message) | 1,359 prompt tokens |
| Fix-a-failing-test task | 5 requests, 71% cached, 20 s |
| Read-only task | 4 requests, 90% cached; killi's own peak memory 18.5 MB footprint, 29.8 MB maximum resident |

Still to port from phase 1: tool-argument repair beyond accepting alternative `edit` shapes; `killi eval`.

## Baseline to beat

- Kimi Code's system prompt plus the six core tool descriptions: about 22 KB, roughly 5.5k tokens per request. On MiMo, `system.md` plus the bash, read and glob descriptions measured 4,112 prompt tokens.
- pi, rebuilt by hand from its source: system prompt about 676 tokens (about 346 without its section on pi's own docs); prompt plus its 4 default tools (read, bash, edit, write) about 1.3k tokens; each tool about 90–280 tokens. Details in `docs/reference/pi-harness-inventory.md`.

## Open

None. All design branches were settled on 2026-10-22.
