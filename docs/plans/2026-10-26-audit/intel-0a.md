## Codebase Intelligence — Architecture & Data Flow

Scope: `crates/killi/src` at 0.4.20 (commit 09c33a7), ~23k LoC, one binary. All paths below are relative to `crates/killi/src/`. Line numbers come from reading the code. Nothing was run. Read `docs/decisions.md` for the reasons behind the design; decision numbers (D9, D21, …) are cited where they matter.

### Project Structure

```
main.rs        CLI dispatch, headless `-p` (run_once + Printer), signal handler, 3k-token budget test (:375)
setup.rs       Setup: config + system prompt + frozen tool list + Settings; builds Agents (start/start_on/resume/reload)
agent.rs       Agent loop, Controls/Aside (shared UI handles), subagents/swarm, compaction, recovery, repeat breaker (2214 lines)
llm/mod.rs     Client: retries, backoff, error classification, SSE streaming, Reply/Usage/ToolCall, output cap
llm/openai.rs  Chat Completions adapter (reasoning_details replay, optional cache_control marks)
llm/anthropic.rs  Messages adapter (merging, thinking signatures, 3 cache breakpoints)
llm/responses.rs  Responses adapter (store:false, encrypted reasoning replay)
tools/mod.rs   Context (per-session tool state), Output, tool definitions (fixed order), summary(), spill()
tools/bash.rs  bash/task_output/task_stop, Process (setsid process group), Tasks, GROUPS/kill_all, steering wake
tools/edit.rs  multi-edit with exact→normalized matching, line diff
tools/files.rs read (N\t view, 10 MB cap, images via sips), write, TextFile (BOM/CRLF)
tools/search.rs grep/glob on `ignore` in spawn_blocking
tools/web.rs   fetch_url (10 MB cap, readability, pdftotext, headless Chrome render/screenshot), web_search
session.rs     JSONL log (record/load/replay/list/prune), last-model.json, user_prompts
config.rs      TOML config, model resolution, first-run guides, in-place edits, importers (Kimi/pi/omp/opencode, hand YAML parser) (2091 lines)
prompt.rs      system prompt assembly, instruction files, Instructions (nested AGENTS.md + change detection)
plan.rs        plan-mode read-only bash allowlist; ON/OFF reminders
swarm.rs       agent_swarm prompt expansion + report packing; ON/OFF reminders
skills.rs      skill discovery, `skill` tool, built-in update-config skill
hooks.rs       Claude-Code-protocol hooks (/bin/sh, JSON stdin, exit 2 blocks)
later.rs       per-project later.md list
snapshot.rs    /undo via a separate bare git dir (~/.killi/snapshots/<tree>.git)
live.rs        presence files (flock) + per-session unix socket; `killi sessions|ask|tell`
mcp.rs         `mcp` gateway tool + eager native tools; stdio (via broker) and streamable HTTP
broker.rs      `killi mcp-serve`: shared stdio MCP servers, id remapping, 15 min idle exit
login.rs, login/chatgpt.rs (OAuth PKCE, flock token refresh), login/opencode_go.rs (key + models.dev catalog)
update.rs      self-update with SHA-256 from releases/latest.toml
tui/mod.rs     App, input loop, run_turn, commands, drawing, terminal guard, suspend (4547 lines)
tui/{editor,text,history,menu,palette,swarm,banner,mention,paste}.rs  prompt editor, markdown styler, fullscreen
               history, slash/@ menus, theme derivation, swarm panel, banner, @file attach, clipboard/drop
prompt/system.md (2.8 KB built-in guidance), skills/update-config.md
```

Process-level files: `~/.killi/{config.toml, last-model.json, mcp.json, mcp-off.json, chatgpt.json(+.lock), command-guide.json, live/<pid>.json, sessions/<cwd-flattened>/{<secs>-<pid>.jsonl, <id>/agent-N.jsonl, <id>/*-<nanos>.txt, later.md}, snapshots/<tree>.git}`; sockets in `$TMPDIR` (`killi-live-<uid>/<pid>.sock`, `killi-mcp-<uid>-<ver>.sock`).

### Request Lifecycle

**Interactive (`killi`, `-c`, `-r`)**
1. `main.rs:63` builds a current-thread tokio runtime (`:65`) → `dispatch` `:75`. The subcommands (`init`, `import-kimi`, `update`, `login`, `logout`, `mcp-serve`, `sessions|ask|tell`) return early at `:77-91`.
2. `:93` `parse`. `:94` `quit_on_signals` spawns a task that waits for SIGHUP/SIGTERM/SIGINT and then runs `tools::kill_all` → `live::leave` → `tui::restore_terminal` → `exit` (`:121-139`).
3. Interactive starts only: `config::set_up_if_missing` (config.rs:505), `guide_model` (:530), `login::chatgpt::offer`. All three are **blocking stdin reads on the runtime thread**.
4. `Config::load` (config.rs:271). The resume log is resolved at `:102-107` (`-c` → `session::list(cwd)` newest).
5. `tui::run` (tui/mod.rs:114):
   - `:116-119`: the sink is an mpsc unbounded sender.
   - `:120-126`: `Controls{interactive}`, then `live::join(cwd, listen=true)`.
   - `:128-129`: OSC 10/11 ground query, then theme.
   - `:130`: `Setup::new` (setup.rs:55-87) → resolve the model, `Skills::discover`, `prompt::build`, `tools::definitions()`, then push `skill`, the `mcp` gateway and eager native MCP tools (`start_eager` awaits the servers).
   - `:133-136`: `Agent::resume` (agent.rs:351) or `Agent::new` (:325). `new` runs `Session::create`, logs a `session` entry, and pushes the system message.
   - `:137`: `Snapshots::new` (sync git and a tree walk).
   - `:146`: `TerminalGuard::enter` (raw mode, bracketed paste, kbd flags, alt screen + mouse 1000/1006, panic hook, `suspend_cleanly`).
6. The loop `:160-262`: `App::read_input` (:1110), then one of:
   - `Input::Prompt` → `mention::expand` → `prompt_turn` (:620): `begin_turn`, `snapshots.take()` (:632), `live_turn`, then `run_turn(agent.run(..))` (:637) and `finish_turn` (:638). `agent.recover()` runs when the turn was interrupted **or errored** (:644-646; :1773, :1783). Last, the auto recap future (:647-649).
   - Commands go to `handle_command` (:465-615).
   - `Resume`, `Model`, `Search`, `Mcp`, `Prune` and `Effort` are handled inline at `:173-257`.
   - Each iteration ends with `refresh_branch()` (:259, 2 sync git calls), `live_sync` and `draw`.
7. `run_turn` (:401-460) selects over agent events, keys, the turn future, peer requests, peer answers, the /btw future and a 100 ms tick. Esc, Ctrl+C or Ctrl+S break the loop, then `drop(future)` (:456) cancels everything in flight.
8. Exit `:263-274`: `finish_screen` (fullscreen prints history to the normal screen), `stop_tasks`, guard drop → `restore_terminal`, `discard_if_unused`, then print the resume command. `app.live` (Registration) drops and removes the presence file and socket.

**Inside a turn: `Agent::run` (agent.rs:526-621)**
- Clear the repeat map and `swarm_used`. Build notes: interrupted, MCP toggles, changed instruction files, `SessionStart` hook (once), `UserPromptSubmit` hook (a block → Err).
- `push_peer_notes`, then push the user message (notes go in a `<system-reminder>` prefix), then `push_images`.
- Loop:
  - `before_request` (:625-663). Drain the steer queue into user messages. Announce plan/swarm changes (depth 0). Push peer notes. Add `peers_note` (live tree changes), `newly_finished` tasks and nested AGENTS.md. Run `compact` if `compaction_due` (a failure becomes a notice).
  - `request` (:720) → `Client::complete`. An overflow error triggers one `compact_with(Reduced)` (:568-572).
  - `record_reply` (:787) assigns unique ids, stores reasoning under the learned key plus `reasoning_items`, logs, then appends.
  - **No calls:** continue up to 3 times on `length` (:578). Continue if steer is non-empty. Otherwise, in order: the left-running-tasks reminder (once, depth 0, interactive), the open-todos reminder (once), and the `Stop`/`SubagentStop` hook (≤3 blocks). Then return.
  - **Calls:** `execute_calls` (:817-878):
    - If steer is non-empty, the remaining calls are answered `SKIPPED_FOR_MESSAGE` (:822-826).
    - Read-only `read|grep|glob` run (only when no hooks are configured) → `execute_reads` → `join_all` (:892).
    - Consecutive depth-0 `agent` calls → `run_subagents` → `run_children` (`buffer_unordered(4)`).
    - `agent_swarm` → `run_swarm` (`swarm_parallel`, ≤128 per turn).
    - Anything else → `execute` (:925): PreToolUse → `dispatch` (:951) → PostToolUse.
    - Each result → `finish_call` (:992): ToolEnd event, `shorten` >50k chars with spill, repeat warnings at 3/5/7, stop at 8 identical calls with the same result. Images go in one trailing user message.

**Headless (`-p`) `main.rs:160-255`**
- `Setup::new`.
- The sink is `Printer` (stdout text, stderr tool lines), or a no-op for `--json`.
- `Controls::default()` (interactive=false, so `ask_user_question` errors and `exit_plan_mode` auto-approves).
- `live::join(cwd, listen=false)`: a presence file with no socket (:174).
- `start`/`resume` → `agent.run` (:196) → `stop_tasks` (:197) → JSON summary or a stderr trailer.
- The exit code is success only for `Stop::Completed` (:254). No recap and no left-running reminder (`interactive` gates :587).

### Data Flow Map

- **User input.**
  - Keys → `on_key_idle` (tui/mod.rs:1173) / `on_key_busy` (:1429) → `Editor` (tui/editor.rs).
  - Submit → `submission()` (:3719) → `Input::{Prompt,Command}` → `mention::expand` (tui/mention.rs:34) appends `@file` contents as a `<system-reminder>`.
  - While busy, `send_busy` (:1480) pushes `(expanded text, images)` onto `controls.steer` and mirrors it in `app.queued`. The agent drains steer in `before_request`, and the `wake` closure (agent.rs:1409-1412) ends bash/task_output waits.
  - Ctrl+S sets `send_now`, which interrupts; `read_input` then joins the steered messages into the next prompt (:1118-1123).
  - Esc clears steer (:1779). `/btw` sets `aside_asked` → `Aside::ask` future polled in `run_turn`.
  - Images: `[Image #N]` placeholders (editor.rs:65-79) → `Agent::run(images)` → a user message after the prompt (agent.rs:882).
- **Agent events → screen.**
  - `Sink = Rc<dyn Fn(Event)>` (agent.rs:58). In the TUI the closure sends to an unbounded channel (tui/mod.rs:116). `run_turn` → `App::on_agent` (:1826).
  - `on_agent` updates `partial`/`markdown`, `running`, `swarm`, `todos`, `asking`, `details`, then `commit` (:2554) or `emit` (:2569).
  - `emit` lays out an `Entry` through `Entry::render`, so `history::clean` drops control characters except ESC and `\n` (history.rs:34-61). Inline, the rendered rows go to `pending`; fullscreen, the entry goes to `history`.
  - Direct `commit()` lines are pushed to `pending` **raw** in inline mode.
  - `Event` variants (agent.rs:60-81): Text, Reasoning, Retry, ToolStart, ToolEnd, Usage, Todos, Ask, Notice, Swarm, Started, Finished, Sub. Two consumers: `App::on_agent` (tui/mod.rs:1826) and `Printer::event` (main.rs:299).
  - `draw` (:2843) composes `live()` (:2646: partial preview, thinking, running rows/swarm panel, btw/peer rows, working row, queued, todos, rule, editor, menu/picker/question, status) and repaints changed rows inside `CSI ?2026`. `draw_screen` (:2905) is the fullscreen equivalent.
  - Subagent events are wrapped as `Event::Sub{label, slot, event}` (agent.rs:1220-1222) and routed by `on_sub` (:1937) into the swarm panel or `↳` rows.
- **TUI anchor map** (tui/mod.rs unless noted):
  - Commands: `COMMANDS` :3768, `BUSY_COMMANDS` :3747, `KEYS` :3791, `run_local` :2113 (what runs while busy), `command_while_busy` :1517, `effort_after_turn` :1531.
  - Menus and pickers: `Picker`/`PickerChoice` :746-795, `suggestions` :1335, `sync_menu` :1364, `sync_files` :1375, `on_key_menu` :1303, `on_key_picker` :1263, `menu_rows` :2800; menu.rs `suggest` :50, `files` :73, `fuzzy_match` :140.
  - Questions and plan: `Asking` :737, `on_key_asking` :1542, `answer` :1572; the plan block is rendered in `on_agent`'s `Event::Ask` arm.
  - Banner and replay: `banner` :1020, `redraw_banner` :1079, `SinceBanner` :916, `replay_log` :1715 (`REPLAY_PROMPTS` 8).
  - Inline vs fullscreen: `wants_fullscreen` :279, `resize` :2620, `scroll` :2951, `finish_screen` :2960, `History::push`/`KEPT` 20k (history.rs:81, :12), `content_rows`/`padding` :3267.
  - Status line: `status_line` :2826, `status_layout` :3130. Editor: `render` :242, `row_starts` :294 (editor.rs).
- **History → request → reply → history/log.**
  - `Agent.messages: Rc<Vec<Value>>` holds the OpenAI-format messages exactly as appended.
  - `complete` (llm/mod.rs:134) calls `prepare` once (body bytes reused across retries). `attempt` (:169) adds the ChatGPT token per try, sends the POST, reads SSE line by line (:211-225) into a `StreamParser`, and returns a `Reply` through `validate_reply` (:251).
  - Retries: 10 attempts, backoff 500 ms·2ⁿ up to 32 s plus 25% jitter (:336), Retry-After honored, quota 429 or Retry-After >60 s is fatal (:324-333). One automatic token refresh on 401 for ChatGPT (:194-199).
  - **Cache marks:**
    - Anthropic marks the system block (anthropic.rs:23), the last tool (:141-143) and the last message block, skipping thinking blocks (:179-183).
    - OpenAI marks only when `[models] cache_control` is set: system plus newest message, on the copy (openai.rs:36-38, 73-95).
    - `prompt_cache_key` is the session id (openai.rs:54, responses.rs:28), plus the optional `session_header` (llm/mod.rs:146).
  - **Reasoning:**
    - Chat scalar is stored under the key it arrived with (`reasoning_key`, openai.rs:157-164; agent.rs:793).
    - `reasoning_details` merge by index/id (openai.rs:204-247), are stored as `reasoning_items`, and replay as `reasoning_details` minus Anthropic/Responses types (:97-129).
    - Anthropic replays thinking blocks only when signed for Claude, with a scalar fallback for compatibles (anthropic.rs:219-237).
    - Responses replays `reasoning` items verbatim (responses.rs:59-63).
  - `account` (agent.rs:763) logs `usage` and sets `stats.context` and `accounted_messages`.
- **Tool results.** `tools::run` (tools/mod.rs:88) or agent-handled tools → `Output{text,is_error,brief,display,image}` → `finish_call`. `brief` and `display` are UI-only. The text (possibly shortened and spilled) is logged with meta `{name,is_error,brief,ms}` and pushed as a `tool` message.
- **Session log** (session.rs). `record` (:108) writes one `write_all` per line with `ts` appended (`stamped` :116). Entry types: `session`, `message`(+`tool`), `usage`, `model`, `compaction`(+`cause`), `error`, `warn`. `load` (:130) replays and restarts at each `compaction` record. Subagents write to `<id>/agent-N.jsonl` (:69).
- **Config.**
  - Read: `Config::load` at start and on `/reload` (setup.rs:123).
  - Text-level in-place edits, each a `fs::write` of the whole file: `set_provider_key` (config.rs:578), `add_models` (:639), `remove_provider_key` (:742), `set_default_model` (:766), `set_search` (:793).
  - Picks go to `last-model.json` (session.rs:177-187); MCP toggles to `mcp-off.json` (mcp.rs:149).

### Module Dependency Graph

`use crate::` edges (non-test):
- `main` → agent, setup, tui, tools, live, session, config, login, broker, update, llm
- `setup` → agent, config, llm, mcp, prompt, session, skills, tools, later
- `agent` → config, llm, session, hooks, skills, live, plan, swarm, tools, mcp, prompt, later
- `tui` → agent, setup, config, session, snapshot, live, mcp, llm, tools, later. Submodules: editor→agent, mention/paste→tools.
- `llm` → config, login::chatgpt
- `login` → config, llm
- `config` → hooks, login::chatgpt, prompt, tools
- `mcp` ↔ `broker`
- `tools` → config, live (bash: `PID_VAR`), llm::truncate
- `session` → config, skills::typed, tools::summary
- `snapshot` → config, live
- `live` → config, llm
- `prompt` → config, skills
- `skills` → config, tools, llm
- `hooks` → config, tools
- `swarm` → tools, llm
- `later` → session
- `update` → tui::{banner,text,megabytes}
- `plan` is a leaf.

Cycles at module granularity:
- config ↔ login::chatgpt (config.rs:125 `signed_in`; chatgpt uses `config::add_models`)
- config ↔ hooks, config ↔ tools, config ↔ prompt (via `home()`/`DEFAULT`)
- llm ↔ login
- mcp ↔ broker
- session → skills/tools → config → … (no back edge to session)

Layering:
- **agent never imports tui** (good). The TUI uses `Agent` methods plus pub fields `session` (log path/id), `stats`, `controls` (agent.rs:296-298).
- The CLI `update` depends on TUI drawing code. `agent` imports `live` (IPC) directly for peer notes.

### Concurrency & Shared State

**Runtime: one current-thread tokio runtime** (main.rs:65). Every agent, subagent, tool, UI, IPC and MCP future interleaves on the main thread.

Off-thread work:
- `spawn_blocking`: grep/glob (search.rs:25,32), readability extraction (web.rs:199).
- One std thread for SIGTSTP (tui/mod.rs:3916). It reads a self-pipe, restores the terminal, sends SIGSTOP, then retakes the terminal.
- crossterm's EventStream internals.
- Subprocesses run in their own session via `OwnSession`/setsid (bash.rs:164-186): bash, hooks, Chrome, MCP servers, the broker.

Tokio tasks:
- Per bash `Process`: a reader+waiter (bash.rs:211).
- Live listener plus one task per connection (live.rs:290,293).
- MCP line reader (mcp.rs:380); broker per-client and writer tasks.
- The signal task (main.rs:128).

**Synchronous blocking on the runtime thread** (stalls keys, ticks and all futures):
- `snapshots.take()` before every prompt: `git add -A` + `write-tree`, with a `thread::sleep` retry up to 3 s (snapshot.rs:49-72).
- `refresh_branch` after every input (tui/mod.rs:2513-2527).
- `Snapshots::new`/`too_many_files` (snapshot.rs:33-47,124).
- `sips` (files.rs:117) and `osascript` clipboard (paste.rs:40).
- `pbcopy` + `wait()` (tui/mod.rs:3574).
- `mention::list` walk on `@` (mention.rs:14).
- `session::list` parsing every log (session.rs:307-322).
- First-run `read_line` prompts (config.rs:934).
- All std::fs I/O in read/write/edit/session.

**Shared handles** (`Rc`/`RefCell`/`Cell`, single thread):

| Handle | Writers | Readers |
|---|---|---|
| `Controls.steer: Rc<RefCell<VecDeque<Steered>>>` (agent.rs:119) | TUI `send_busy`, `read_input` drain (send_now), `finish_turn` clear | `before_request` drain; `execute_calls` peek; bash/task_output `wake` closure (top level only, agent.rs:913,974) |
| `Controls.plan: Rc<Cell<bool>>` (shared with subagents, :1232) | TUI `toggle_plan`; `exit_plan` sets false | `dispatch` checks; `before_request` announce |
| `Controls.swarm: Rc<Cell<bool>>` | TUI `/swarm` (temporarily set for `/swarm <task>`, tui/mod.rs:507-510) | `before_request` |
| `Controls.aside: Rc<RefCell<Aside>>` | `Agent::share` after every push, model switch, compaction (agent.rs:443-451) | TUI /btw, recap, peer ask, peer Status (`waiting`) |
| `Controls.peer_notes: Rc<RefCell<VecDeque<String>>>` | TUI `on_peer` (Tell); cleared on /new and resume | `push_peer_notes` |

- `Aside` holds `Weak<Vec<Value>>` (messages) and `Weak<tools::Context>` (agent.rs:140-142). `side()` upgrades, deep-copies the whole history (`side_request` :213, base64 included) and drops the strong ref before awaiting.
- `tools::Context` (`Rc`, tools/mod.rs:31): `tasks: RefCell<Tasks>`, `running: RefCell<Running>`, `search: RefCell<SearchSettings>`, `images: bool`. **One instance is shared by the parent and every subagent** (agent.rs:1227), so background task ids are one namespace.
- `Settings` (`Rc`): `mcp: Rc<Mcp>` (RefCells: `live`, `listed`, `off`, `notes`), `hooks: Rc<HooksConfig>`, `skills` (`Rc<Vec<Skill>>`). `Client` is `Rc` and replaced whole on a switch.
- No RefCell borrow is held across an `.await` that I found (task_output scopes its borrow, bash.rs:535-542; mcp `connection()` returns inside the borrow scope).
- Process-global state:
  - `GROUPS: Mutex<BTreeMap<pgid, watch::Receiver<Option<Exit>>>>` (bash.rs:121) for `kill_all`.
  - TUI statics `ENHANCED`/`FULLSCREEN`/`CURSOR_ROW`/`RESUMED`/`SUSPEND_PIPE`/`RESUMED_NOTIFY` (tui/mod.rs:3810-3828), shared with the suspend thread.
  - OnceLocks for theme/palette/NO_COLOR.
  - Bash output is `Arc<Mutex<Captured>>`, shared by the reader task, `Process` and the `Running` description.

**`Rc::get_mut` / `Rc::make_mut` audit:**
- `Rc::make_mut(&mut self.messages)` at agent.rs:1397 (`push_meta`) and :489 (`reload_system`). Strong count is 1 and the Aside `Weak` exists, so `make_mut` takes the "only weak refs" path: the 24-byte Vec header moves to a new allocation (O(1)) and **every existing `Weak` is disassociated**. That is why both sites call `share()` immediately afterwards (:1398, :490). **Invariant:** any future `make_mut` on `messages` without `share()` silently breaks /btw, recap and peer asks ("no conversation to ask about yet"). If a strong clone ever outlives a push, `make_mut` deep-clones the whole history.
- `Rc::get_mut(&mut self.ctx)` at agent.rs:468 (`ctx_images`, called from `switch_model` :426). It **always returns None** after the first push, because `share()` has put `Rc::downgrade(&self.ctx)` into `Aside` (:448). See Observation 1.
- The `Rc::get_mut(&mut agent.client)` calls are test-only (agent.rs:2014-2085).

### Business Domain Model

- **Entities:**
  - `Config`/`Provider`/`Model` → `ResolvedModel` (config.rs:192) → `Client`.
  - `Setup` (frozen prompt + tools + Settings).
  - `Agent` (conversation, client, ctx, controls, stats, session, depth).
  - `Session` log; `Message` (OpenAI JSON `Value`); `ToolCall`/`Output`.
  - `Task` (background process group); `Slot`/swarm member; `Todo`; later item.
  - `Snapshot` tree; live `Entry`/`Registration`.
  - `Skill`; MCP `ServerConfig`/`Connection`; `Hook`; `Choice` (last model and effort).
- **Workflows:**
  1. *Steering:* calls after the running one get `SKIPPED_FOR_MESSAGE`, then the message is appended (D16).
  2. *Esc:* drop → guards clean up → `recover()` (agent.rs:1339) → the next prompt is marked interrupted.
  3. *Plan mode:* the Cell plus a `plan::ON` reminder → `dispatch` blocks (agent.rs:963-971) → `exit_plan_mode` → `Event::Ask{plan}` → approve or feedback.
  4. *Compaction:* proactive (60% of the window, ≤85%, agent.rs:693-718), on overflow (`reduced`), or `/compact`. History becomes `[system, COMPACTED_MARK + kept prompts + summary]`, the announce flags reset, and the record logs a `cause`.
  5. *Resume:* `session::load` → `Agent::resume` keeps the **logged** system prompt but the **current** tool list → `recover()` → `share()`. The model and effort come from the last `model`/`session` entry (setup.rs:104-107).
  6. *Side requests:* `/btw`, recap and peer `ask` all use `Aside::side`: history plus placeholder results for running calls plus a note, sent with the same tools. Nothing is logged.
  7. *Subagents/swarm:* `subagent()` (:1213) clones client, tools, ctx, settings and plan Cell, and gets a fresh or forked history and its own log. Only depth 0 may spawn. Child stats merge into the parent.
  8. *Undo:* `snapshots.take()` before each prompt turn. `/undo` warns once if live peers worked since, then `read-tree` → delete untracked → `checkout-index -a -f` (snapshot.rs:108-121).
  9. *Peers:*
     - `ask` → `on_peer` → a side request (at most 2 at once).
     - `tell` → `peer_notes` (at most 3 per prompt) → appended as a reminder.
     - `Status` → todos, plan and waiting.
  10. *MCP:* `mcp {server}` lists tools; `{server,tool,arguments}` calls one. Stdio goes through the broker (attach/start, id remap); HTTP posts directly. Eager servers become native tools at session start.
  11. *ChatGPT login:* PKCE on 127.0.0.1:1455. Tokens go to `chatgpt.json` (0600, temp+rename). Refresh happens per attempt under a `flock` on `chatgpt.lock` (chatgpt.rs:150-165,221-238).
  12. *Update:* `latest.toml` (version + sha256), download, `shasum`, rename over the exe (update.rs:39-105).

### Critical Paths (highest-risk code)

1. **Append ordering and log atomicity.** `push_meta` logs first, then appends to memory (agent.rs:1391-1400). `Session::record` writes one line (session.rs:108-127). A write failure means no append, and the error propagates out of `run`.
2. **Prompt-cache prefix stability.**
   - Tools are frozen in `Setup::new` (setup.rs:60-75); the system prompt in `prompt::build` (prompt.rs:20).
   - The only rewrites are `reload_system` (agent.rs:484-492) and `compact_with` (:1265-1335).
   - Adapters must convert deterministically: `replay_history` (openai.rs:97), `convert_messages` (anthropic.rs:149), `convert_input` (responses.rs:54). Reminders go only at the end; mode changes go through reminders (plan.rs, swarm.rs, live.rs:465).
3. **Tool-call/result pairing.** `unique_id` (:1370-1383), `record_reply` (:787-810), `finish_call` (:992-1019), `SKIPPED_FOR_MESSAGE` (:822-826), `unanswered`/`recover` (:224-235, 1339-1348), and `side_request` placeholders (:213-221). A gap means a provider 400 on every later request.
4. **Interrupt by drop.** `run_turn` `drop(future)` (tui/mod.rs:456). Cleanup guards: `Process::drop` (bash.rs:267), hooks `Group` (hooks.rs:105-114), Chrome `Running` (web.rs:248-266), `Waiting` (bash.rs:295). This path must leave history valid (see 3).
5. **Compaction.** `compaction_pressure` (:701-718), `estimate_tokens`/`IMAGE_TOKEN_RESERVE` (:1428-1444; llm/mod.rs:267), `compact_with`, `reduced` (:1461-1521), the overflow path (:568-572). It is the only data-losing rewrite of history.
6. **Resume.** `load` (session.rs:130-164) skips bad lines; `Agent::resume` (agent.rs:351-376) rebuilds `ids` and `accounted_messages`; `Setup::resume` picks the client.
7. **Undo.** `Snapshots::undo` (snapshot.rs:108-121) deletes files and force-overwrites the tree. The shadow index is shared across sessions in the same tree.
8. **Process-group cleanup.**
   - `OwnSession`/setsid (bash.rs:164-186); `GROUPS` + `kill_all` (:121-146); `group_running` pid-reuse logic (:130-135).
   - Tasks leave groups behind (`left_running`, `stop_all` :493-514); `stop_tasks` on exit, /new and resume (tui/mod.rs:177,264,551); `quit_on_signals` (main.rs:121-139).
9. **Config and credential writes.** In-place text edits on the user's hand-edited TOML (config.rs:578-830). ChatGPT refresh under flock (chatgpt.rs:150-238); refresh tokens rotate, so a lost write means signing in again.
10. **Terminal ownership.** `TerminalGuard`, panic hook, `suspend_cleanly`, `restore_terminal` (tui/mod.rs:3836-3994), `query_ground` raw-mode window (:293-337).
11. **IPC.**
    - Presence `open_in` stages, locks, then renames (live.rs:133-143); `list_in` stale cleanup (:204-231).
    - Socket `serve` checks the uid (:301-321).
    - Broker id remap and `Route` (broker.rs:183-189, 140-160), and its server map lock (:289-297).
12. **Streaming parse and validation.** SSE line parse (llm/mod.rs:211-228), `validate_reply` (:251), parsers' `finish` (openai.rs:193; anthropic.rs:389; responses.rs:191). Partial output from a failed attempt must never reach history.

### Observations for auditors (UNVERIFIED leads from reading; each with the check to confirm)

1. **`/model` never updates `ctx.images`.** `ctx_images` uses `Rc::get_mut` (agent.rs:467-471), which is None because `Aside` holds a `Weak` (:448).
   - The paste gate is right (tui/mod.rs:1387), but `read` (files.rs:97), screenshots (web.rs:331) and MCP images (agent.rs:956-957) use the start model's flag.
   - Going from image-capable to text-only lets `read x.png` push an image. No "strip media on rejection" (D26) exists outside reduced compaction (agent.rs:1479), so every later request may 400. *Check:* add a test that switches models and asserts `ctx.images`.
2. **Resume breaks subagents.** `assemble` sets `children: 0` (agent.rs:412). `subagent()` → `Session::child(n)` → `create_new(true)` (session.rs:74-79) on `<id>/agent-N.jsonl`, which already exists from the earlier run. *Check:* resume a log that spawned agents, then call `agent`; expect "Could not start the subagent: could not create …".
3. **Subagents swallow the parent's task-finished notices.** `before_request` calls `newly_finished()` (sets `reported`) at any depth but pushes only at depth 0 (agent.rs:645-649), and `ctx` is shared (:1227). A child can also `task_output`/`task_stop` the parent's tasks.
4. **Non-UTF-8 files are corrupted by `edit`.** `TextFile::decode` uses `from_utf8_lossy` (files.rs:185) and `edit` writes the whole file back (edit.rs:34-36). Every invalid byte anywhere in the file becomes U+FFFD. `is_binary` only checks for NUL bytes (tools/mod.rs:301).
5. **`/undo` doesn't tell the model files were reverted.** The model is only told "The user interrupted your previous turn" (tui/mod.rs:532 → agent.rs:1351), so it keeps a stale picture of the files.
6. **Terminal escape injection.**
   - Live-area rows are painted without control-character stripping: tool summaries (bash command first line, paths, URLs), `todo.content`, question options, swarm items. Path: `fit`/`ellipsize` (tui/text.rs:612) → `draw` (tui/mod.rs:2843).
   - In inline mode `commit()` (:2554) is written raw, including Ctrl+O `show_detail` of tool output (:2088-2108).
   - `history::clean` keeps ESC on purpose (history.rs:47-61), so model text can carry OSC 52/OSC 8/title/CSI sequences. Only bash previews strip controls (:2010). *Check:* `todo_write` with `"\u001b]0;x\u0007"`.
7. **Plan-mode allowlist bypasses** (plan.rs:5-41):
   - Commands that write or execute: `fd -x/--exec`, `rg --pre`, `sort -o`, `uniq in out`, `tree -o`, `git branch -D/-m`, `git tag <n>|-d`, `git diff --output=`, `cargo clippy --fix`, `sed -n 'w f'`.
   - MCP and native MCP tools are dispatched before the plan checks (agent.rs:956-957 vs 963-971).
8. **The first-run guide answers yes on EOF.** `confirm` treats EOF as yes (config.rs:928-942). The signal handlers installed at main.rs:94 can't run while blocking reads hold the runtime thread (:96-98). A pty that closes mid-guide therefore imports or writes `~/.killi/config.toml`.
9. **`/prune` deletes other live sessions' logs.** It spares only `current` (session.rs:356-379) and never consults `live::list`.
10. **`/reload` and resume are stale in places.**
    - `/reload` replaces the config but keeps `settings.hooks`, `compact_at`, `plan_allow` and `swarm_parallel` from startup, for `/new` too (setup.rs:128-136).
    - Resume uses the current tool list, not the logged `tools` (agent.rs:344 vs setup.rs:106). That costs a cache miss after an update or MCP change, against D21's "resumes byte-identical". It also ignores the logged `cwd` (`-r` from another folder).
11. **Log robustness.**
    - `load` silently skips unparseable lines (session.rs:134).
    - `recover`/`unanswered` only patches the last assistant (agent.rs:224-235), so a dropped mid-history tool result 400s forever.
    - `reduced` `expect`s object messages (:1511); a hand-edited log can trip it, and panic=abort kills the process.
12. **`killi tell` framing breaks out.** `tell_note` doesn't escape `</message>`/`</system-reminder>` in the peer text (live.rs:486-491). A prompt-injected session can use `killi tell` to escape the "not from the user" framing. Same-uid only (live.rs:303).
13. **Broker has no peer-uid check** (broker.rs:249-286; compare live.rs:303). Anyone who can connect can spawn an arbitrary `Launch{command,args,env,cwd}`. Protection rests on `$TMPDIR` permissions.
14. **OpenCode Go `base_url` is unvalidated.** It comes from models.dev `provider.api` (login/opencode_go.rs:131-140), so the provider key is sent wherever the catalog points.
15. **Update integrity.** The checksum comes from the same origin as the binary, with no signature (update.rs:62-105). `reqwest::Client::new()` has no timeouts. Release binaries are committed to git.
16. **Secrets reachable by the model.** Bash children inherit the full environment, including provider keys from `api_key_env` (bash.rs:190-203). `ResolvedModel` derives `Debug` with `api_key` (config.rs:191-197).
17. **Hooks fail open.** A timeout or spawn failure means "no block" (hooks.rs:62-64,143). PreToolUse/PostToolUse never run for depth-0 `agent`/`agent_swarm` calls (agent.rs:842-860 bypass `execute`).
18. **Signal path cleanup is partial.** `kill_all` covers only bash `Process` groups. Chrome (web.rs:248), hook groups and directly spawned MCP children survive SIGTERM/SIGHUP. `notify()` spawns `osascript` without reaping (zombies) and puts model text into OSC 9 and AppleScript strings (tui/mod.rs:3603-3625).
19. **Non-atomic writes.** All config edits (config.rs:578-830), `last-model.json` (session.rs:185), `mcp-off.json` (mcp.rs:149) and `later.md` (later.rs:62) truncate then write, with no lock. Compare chatgpt.rs:78-86 (temp + `sync_all` + rename). `set_provider_key` treats any `[`…`]` line, including lines inside the `[prompt] system = '''…'''` block, as a table header (:587).
20. **Undo failure modes.** `trees.pop()` happens before the git steps (snapshot.rs:112), so a mid-way failure leaves a half-restored tree and a lost snapshot. Sessions in one tree share the shadow index, so their `add -A` and `read-tree` calls can interleave. Nested git repos are recorded as gitlinks, so their changes aren't undone.
21. **Performance hot spots.**
    - Per request, `replay_history` deep-clones every message (openai.rs:97-129).
    - The auto recap after every tool-using turn deep-copies history (agent.rs:213).
    - `session::list` parses every log for `-c` and the picker (session.rs:307-322).
    - Broker holds `servers` (tokio Mutex) across `Server::start` for up to 60 s (broker.rs:289-297).
    - MCP pending entries leak on timeout (mcp.rs:427-434).
    - The SSE `drain(..=end)` per line is quadratic on big chunks (llm/mod.rs:215).
    - `draw()` runs on every streamed delta.
22. **Smaller correctness leads.**
    - Responses sends `include: reasoning.encrypted_content` only when thinking is on, but always replays reasoning items (responses.rs:43-45 vs 59-63). Effect of `/effort off` mid-session is untested.
    - An error-ended turn is marked interrupted (tui/mod.rs:1783; D42 known).
    - Each subagent clones the id set (agent.rs:1241), so siblings can emit identical ids and `take_running` (tui/mod.rs:1977) mismatches rows.
    - The OpenAI parser `resize_with(index+1)` trusts the provider's index (openai.rs:169-172), so a huge index can exhaust memory.
    - `project_dir` flattens `/` to `-`, so different paths can collide (session.rs:56-58, snapshot.rs:35).
    - `/new` keeps the plan and swarm Cells (tui/mod.rs:546).
    - `AgentConfig.compact_at` doc still says "default 256000" (config.rs:99) but the code uses 60% of the window (agent.rs:704-711).
    - ~200-line hand-written YAML parser (config.rs:1431-1627).
