## Code Design & Architecture - Audit Results
**Score**: 3/4  (the core is deliberate and holds its invariants: layering, cache prefix, append-only history. The bugs sit where state is copied or derived more than once and then kept in sync by hand: config text edits, Setup → Settings, App resets, the log's owner, error kinds)
**Files reviewed**: 20 (agent.rs, setup.rs, main.rs, session.rs, snapshot.rs, config.rs, tools/mod.rs, tools/bash.rs (part), llm/mod.rs, llm/openai.rs, llm/responses.rs, llm/anthropic.rs (part), tui/mod.rs (most), mcp.rs (part), later.rs, live.rs (part), prompt.rs (part), login/chatgpt.rs (part), docs/decisions.md, AGENTS.md)
**Issues found**: 12 (P0 0 · P1 1 · P2 6 · P3 5)

Verification harness: scratch copy at `<scratch>/D`, which adds `audit_d` test modules to config.rs, setup.rs, tui/mod.rs, session.rs and llm/mod.rs. It also adds a test-only `Agent::audit_search_provider` and makes `agent::is_overflow` `pub(crate)`. Command:
`cd <scratch>/D && HOME=<scratch>/D/home CARGO_TARGET_DIR=<scratch>/D/target cargo test --locked -p killi audit_d -- --nocapture`
(`<scratch>` = `/private/tmp/claude-503/-Users-wuhaotian-sphnet-com-sg-code-killi/a381b734-e6e5-4c1d-b105-209d2882b618/scratchpad`). Result: 9 tests fail, each demonstrating a bug below (config 4, setup 3, tui 1, session 1). The llm prefix test passes.

### P0 - Critical (must fix before merge / blocks production)
None.

### P1 - High
- **[config.rs:587-588, 750-751, 774-775, 808-809 (writes at 604, 759, 786, 829)]** Five text-level editors change the user's hand-edited config.toml, and each one decides "which table am I in" by its own rule: `starts_with(header)`, `== header`, or "any `[…]` line". None of them allows a trailing comment on a header (`starts_with('[') && ends_with(']')`). Only `with_models` (:736) checks its result with `toml::from_str` before writing.
  Scenario (each case is a failing test):
  (a) `[providers.openai]` has `api_key_env`, and the next table is `[providers.anthropic] # personal` with `api_key = "sk-ant-…"`. The first-run key guide (`guide_model` → `set_provider_key`) doesn't see the commented header as a new table, so it stays "in" openai and drops anthropic's `api_key` line. `add_models` takes the same path on a re-login with a key. `killi logout` (`remove_provider_key`) drops the next table's key lines the same way.
  (b) The provider's own header has a comment: `[providers.openai] # work`. The key line is inserted, but `api_key_env` stays, and `Provider::key` prefers the variable (:154-160). So the key is still "missing" and the guide asks again. The second store writes a duplicate `api_key`, and the TOML no longer parses ("duplicate key").
  (c) `[search] # web` followed by /web-search-config appends a second `[search]` table, and the config no longer parses.
  Impact: either an API key the user may not have anywhere else is silently deleted, or config.toml no longer parses and killi refuses to start until it is fixed by hand. Every write also truncates and then writes, unlike the temp + fsync + rename in chatgpt.rs:78-86 (H owns lead 19; the `'''` multi-line case in lead 19 is the same walker).
  Verified: tested (`config::audit_d::*`: 4 fail. Outputs show the deleted `sk-ant-keep` line, the duplicated `api_key` and the duplicate `[search]`).
  Suggested fix: one `config/edit.rs` that owns every in-place edit:
  - one section walker: a header is `^\s*\[[^\]]+\]\s*(#.*)?$` with quotes and whitespace normalized, and lines inside `'''`/`"""` strings are skipped;
  - every editor checks its output with `toml::from_str::<Config>` before writing, as `with_models` already does;
  - one `write_atomic(path, bytes, 0o600)` lifted from chatgpt.rs.

  The five editors then reduce to "edit the lines of table X". This removes duplicated behavior and needs no TOML-preserving crate.

### P2 - Medium
- **[setup.rs:76-85, 128-136, 140-142; agent.rs:486-488, 389-390]** `Settings` is derived from `Config` once, in `Setup::new`, and then exists as four copies of the search choice: `Setup.config.search`, `Setup.settings.search`, `Agent.settings.search` and `ctx.search`. `set_search` updates only `config.search`. `refresh` (/reload) updates only `skills` and `instruction_files`. Meanwhile `start_on` (/new) and `reload_agent` hand agents the stale `self.settings`.
  Scenario: the user picks Tavily in /web-search-config, and the notice at tui/mod.rs:217 says "here and in new sessions".
  - `/new`: the new session's `ctx.search.provider` is `""`, the startup Brave.
  - `/reload`: `reload_system` copies the stale `settings.search` into the running session's `ctx.search`, so the pick is reverted even though config.toml has it.

  In the same way, /reload keeps the startup `compact_at`, `plan_allow`, `swarm_parallel` and `hooks` (intel lead 10, owned by I, confirmed here), and the TUI keeps the startup `[ui] recap` and `notify_after` (tui/mod.rs:144-145).
  Impact: web_search quietly keeps using the service the user just replaced, for example Brave's captcha-prone public page instead of a keyed API. This contradicts decisions.md §41 and the doc comment at setup.rs:138.
  Verified: tested (`setup::audit_d::*`: 3 fail. `left: ""  right: "tavily"`, and after /reload `left: (None, [], 0)  right: (Some(1234), ["make -n"], 2)`. The 0 is the scratch helper's `Settings::default()`; in production the startup value, 8 by default, stays. Either way the edit is ignored).
  Suggested fix: one `fn settings(config: &Config, skills: &Skills, prompt: &SystemPrompt, mcp: Rc<Mcp>) -> Settings`, used by both `new` and `refresh`, and `set_search` also sets `self.settings.search`. This is cache-safe, because none of these fields touches the prompt or the tools. Patching search alone would leave four fields with the same staleness.

- **[tui/mod.rs:173-190 vs 546-563; 2042-2056; 2164-2166; 2710-2723]** A change of conversation is handled by two hand-written reset lists.
  - `/new` clears peer notes, `todos` and the usage totals, then wipes the screen and redraws.
  - The `/resume` picker arm clears peer notes and sets `context`, nothing more.

  Scenario (traced): session A has open todos and 120k tokens on the status line, and the user resumes session B with /resume. Afterwards:
  - the live area still shows A's task list (2710);
  - `killi sessions <pid>` reports A's todos to other sessions (2164-2166);
  - the status line keeps A's totals, while /usage (`agent.stats`, 2321) shows B's.

  The TUI keeps a second usage ledger (`App.status`, summed at 2042-2049) beside `Agent.stats` (agent.rs:767-771, 1187-1194), and only one of them is reset.
  This shares a root cause with [V2]/[V3] (B owns those symptoms): `App`'s roughly 60 flat fields have different lifetimes, and each is reset at call sites chosen by hand.
  - Per reply: `partial`, `held_blank`, `thinking`, `markdown`. The Retry arm and `markdown.in_code` are the resets V2 and V3 report missing.
  - Per turn: `running`, `swarm`, `retry`. `end_busy` resets these.
  - Per conversation: `todos`, the status totals, `details`, `undo_warned`, `peer_tells`. This finding is the missing reset at this level.

  Impact: after /resume the task list and token figures on screen are wrong, and other sessions get a wrong status.
  Verified: traced (the full Resume arm; `reset_usage` has one caller, at :556).
  Suggested fix: group the fields by lifetime.
  - A `Stream { partial, held_blank, thinking, markdown }` with a `reset()` called at reply boundaries (Retry, `end_busy`).
  - A per-conversation struct rebuilt by one `App::replace_agent(&Agent)`, called by both /new and resume. It also owns `stop_tasks`, `discard_if_unused` and the peer-notes clear.
  - Status totals read from `agent.stats` instead of a second sum.

  Leave `on_agent` where it is.

- **[main.rs:102; session.rs:84-90, 307-322; tui/mod.rs:2352-2369; live.rs:54, 133-143]** Nothing owns a session log while a session is writing it.
  - `-c` takes the newest log in the folder, and `-r` and `/resume` list every log, including the current one and logs that live sessions are appending to.
  - `Session::open` opens the file for append without taking a lock.

  Scenario (traced): killi is working in tab 1. In tab 2, in the same folder, the user runs `killi -c`, which resumes tab 1's live log because it is the newest by mtime. Both processes now append `message` records to one JSONL.
  - The next resume loads interleaved turns: assistant(call Z), assistant(call Y), tool(Z)… Providers reject that ordering (tool calls must be followed by their results), so every request on that log fails.
  - The two processes' subagents also share `<id>/agent-N.jsonl`: [V8], across processes.
  - The same missing ownership is why `/prune` deletes live sessions' logs (lead 9, owned by H).

  Impact: corrupts the log that resume depends on. 41% of active hours had two or more sessions running, and 39 pairs overlapped in one folder (decisions.md §49).
  Verified: traced.
  Suggested fix: make the log file its own lock, the way the presence files already work (live.rs:133-143).
  - `Session::create` and `Session::open` take an exclusive `File::try_lock` and hold it for the session's life.
  - A failed `open` names the session holding the log (pointing at `killi ask`).
  - `list` marks or skips logs it can't lock, and `prune` skips them.

  The kernel releases the lock on any exit, so no stale locks are left behind.

- **[llm/mod.rs:156-158, 320-333; agent.rs:1666-1671; tui/mod.rs:3281-3299; login/chatgpt.rs:210-217]** Provider errors are turned into strings too early. `Client::complete` flattens the typed `Failure` (status, retryable, retry_after) into `anyhow!("{message} (after N attempt(s))")`. Three consumers then work out the error's kind again from that text, each with its own word list, and the lists have drifted: the agent (`is_overflow`), the TUI (`error_hint`) and `chatgpt::advice`.
  Scenario:
  - A Bedrock-style 429 "Too many tokens, please wait" counts as an overflow, so killi compacts ([V5]), while the hint for the same error says "rate limited".
  - OpenAI's "Your input exceeds the context window" makes the agent compact, but the hint is None.
  - "HTTP 400 … max_tokens: 4010 > 4000" gets the "Check api_key" hint, because "401" is a substring.

  Impact: V5's needless compaction (a history rewrite plus a summary request, losing detail), and misleading hints.
  Verified: tested (`tui::audit_d`: 2 of 3 classifications disagree, and the third shows the "401" substring). The exact Bedrock wording is unverified (taken from V5).
  Suggested fix: llm returns a typed error, for example `ProviderError { status: Option<u16>, kind: Overflow|Quota|RateLimit|Auth|NotFound|Network|Other, message }`, with the same Display text.
  - It is classified once, next to `classify`, from the status and the body. Overflow requires a 400 or 413 plus an overflow phrase, which removes V5's 429 false positive.
  - The callers `downcast_ref` it and match on `kind`.

  This removes three word lists. A patch to `is_overflow` alone would leave the TUI disagreeing.

- **[tools/mod.rs:39, 51; agent.rs:389, 426, 467-471; tui/mod.rs:1387] — root cause of [V1]** `Context.images` is a plain `bool` inside an `Rc` shared with the `Aside` (as a Weak) and with subagents. Its only writer uses `Rc::get_mut`, which silently does nothing whenever a Weak or a clone exists. The same fact is also stored twice: `show_model` updates `App.sees_images`, so the paste gate is right, but `ctx.images` isn't updated, so `read`, screenshots and MCP images follow the old model.
  Impact: [V1]. This is the only production `Rc::get_mut`, and the only piece of mutable state behind a shared `Rc` that has no interior mutability.
  Verified: traced (the orchestrator verified V1 itself).
  Suggested fix: make it `images: Cell<bool>`, the way `search: RefCell<…>` already is (decision 41). `switch_model` then calls `ctx.images.set(…)`, and `ctx_images` is deleted.
  - Why it is safe: `/model` runs only when the agent is idle (`BUSY_COMMANDS`, tui/mod.rs:3747), so no subagent or tool future holds `ctx` at that moment.
  - Subagents use the parent's client, so sharing the Cell gives them the right value.

  Rule worth keeping: no `Rc::get_mut` with a silent fallback.

- **[agent.rs:526-621, 720-810, 1265-1335, 351-376, 1339-1348]** None of the following runs under `cargo test`: `Agent::run`, `request`/`record_reply`/`account`, `compact_with`, the overflow → reduced-compaction path, `Agent::new`/`resume`, or recovery after an interrupt.
  - The only calls to them are in production code (agent.rs:1169, main.rs:196).
  - The Python harnesses with a scripted provider (`live_check.py`, `frames.py`, `eval.py verify`) do exercise them, but they aren't in CI (intel-0b).
  - Critical path #2 (prefix stability) has no test at all.

  Impact: critical paths #3–#6 (call/result pairing, interrupt by drop, compaction, resume) can regress without any test failing. V1, V5 and V8 would each have been caught by one test.
  Verified: traced (grep: no test calls run, compact, new or resume). The prefix property itself: tested. `llm::audit_d::audit_d_appending_keeps_the_sent_prefix` passes on OpenAI chat, Anthropic and Responses: earlier items are byte-identical once cache marks are removed.
  Suggested fix: no production change is needed. `ResolvedModel.base_url` is configurable, and tools/web.rs:643 and login/chatgpt.rs:611 already run a local `TcpListener` in tests.
  - Add a helper of about 40 lines to the agent.rs tests: `scripted_provider(Vec<&str /*SSE body*/>) -> String /*base_url*/`. It drives `run` end to end, including drop and recover, overflow then compaction, and resume then subagent.
  - Add the prefix test from the scratch copy as written.

### P3 - Low / Nits
- **[agent.rs:371→374, 489→490, 1319→1325, 1397→1398] intel-0a lead (`make_mut` / `share()` invariant): confirmed, and it holds today.** Every history mutation is followed by `share()`, and `switch_model`/`set_thinking` re-share the new client. But the invariant rests on discipline: a fifth site that forgets `share()` silently breaks /btw, recap and peer asks ("no conversation to ask about yet").
  The two rewrite paths also change memory before logging (1319/1326, 489/491), the reverse of `push_meta`'s log-first rule (critical path #1). Because a failed proactive compaction is only a notice (655-661), a failed `compaction` record leaves memory compacted, the log not compacted, and `accounted_messages`/`stats.context` not updated.
  Verified: traced.
  Suggested fix: two private helpers own the invariant.
  - `append(message, meta)`: log, then push, then share.
  - `replace_history(messages, record)`: log, then swap, then share.

  The four sites become two calls with one ordering. Add a test that `controls.aside` sees the newest message after a push, a compaction, a reload and a resume.

- **[tui/mod.rs:293-372, 3807-3995; config.rs:480-967, 969-1650] intel-0a leads (`tui/mod.rs` size, module cycles): size confirmed, but size is not the bug. Cycles are harmless.**
  - **tui/mod.rs.** The shape that invites bugs is the flat state described in the second P2. The lowest-risk cut is pure motion into `tui/terminal.rs` (about 270 lines): `TerminalGuard`, the six statics, `resumed`, `take_terminal`, `suspend_cleanly`, `leave_fullscreen`, `restore_terminal` and `query_ground`/`parse_*`. That isolates the TUI's only cross-thread, global state from `App`. Child modules can see `App`'s private fields, so `impl App` blocks can move with no visibility changes.
  - **config.rs.** Move the importers and the hand-written YAML parser (969-1650, plus about 170 test lines) to `import.rs`. Move the first-run guide and the stdin helpers (`set_up_if_missing`, `guide_model`, `pick_model`, `command_guide_if_first`, `confirm`, `read_line`, `read_secret`) to `guide.rs`. That removes config → login (`pick_model` calling `signed_in`, :560), the only cycle with behavior behind it.
  - **Other cycles.**
    - config ↔ hooks, tools, prompt, skills, live, snapshot, session and mcp exist only because `home()`/`killi_home()` live in config.
    - llm ↔ login is real coupling (the per-attempt ChatGPT token); keep it.
    - mcp ↔ broker is one constant.
    - Rust has no initialization order, so none of these needs fixing for its own sake.
  - **agent.rs** (about 1.7k lines without tests) is cohesive; no cut pays for itself now.

  Verified: traced.

- **[login/chatgpt.rs:78-86 vs session.rs:185, mcp.rs:149, later.rs:62, config.rs:852, 975, 1421; session.rs:57 vs snapshot.rs:35; agent.rs:767-771, 1188-1192, tui/mod.rs:2043-2045]** Small duplications, each better as one seam:
  - **Atomic writes:** one correct writer, used only by chatgpt.rs; the rest call plain `fs::write`. Use the `write_atomic` from the P1 (H owns lead 19).
  - **Path flattening:** `replace('/', "-")` appears twice, in session.rs and snapshot.rs. Use one helper, so the collision fix (H, lead 22) lands in one place.
  - **Usage:** summed field by field three times. Use `impl AddAssign for Usage`.
  - **Token estimators:** agent.rs:1428-1444 (chars/2) and llm/mod.rs:272-302 (bytes/4) differ on purpose and agree on 2048 tokens per image. Decision 42's fix reached one before the other; a shared image-reserve rule would keep them from drifting. No current bug.

  Verified: traced.

- **[mcp.rs:98, 149; session.rs:61-66; tui/mod.rs:590, 645]** Lost updates and edge-case collisions:
  - **mcp-off.json:** `Mcp` reads the file once at start, and `toggle` writes back its whole in-memory set. If two sessions turn different servers off, the last writer erases the other's choice for new sessions (traced). Fix: re-read and merge at toggle time.
  - **Log names:** `<secs>-<pid>.jsonl` opened with `create_new` means a `/new` in the same second the current log was created fails with "File exists" (tested: `session::audit_d`, os error 17).
  - **Log-write failures:** if a log write fails (for example a full disk) inside `agent.recover()?` or `session.reload_agent(agent)?`, the error leaves `tui::run` and killi exits. Fullscreen then loses the printed conversation and the resume hint. A plain error line would be better (traced). Background tasks still die, because `Process::drop` kills their groups.

  Verified: mixed, as marked in each item.

- **[agent.rs:412, 1214-1215; agent.rs:645-649] Shapes for [V8] and [V9] (G owns both).**
  - **V8:** `Session::child` should take the next free N (`create_new` in a loop). That also covers the two-process case from the session-log P2. Resetting `children` on resume is not enough.
  - **V9:** gate the `newly_finished()` call itself on `depth == 0`. The destructive read is the bug. The shared `ctx`, with one task namespace, is otherwise right: a subagent's leftover tasks must outlive it.

  Verified: traced.

### Checked and fine (one line each: what you looked at that holds up — saves the next reader time)
- Layering: agent never imports tui. The TUI drives the agent only through `Agent` methods, `Controls` (Rc Cells/RefCells) and the sink channel, and headless and TUI start agents from the same `Setup`.
- `Aside::side` clones the history, client and tools synchronously and returns a `'static` future. No RefCell borrow or strong `Rc` spans an await, and `make_mut` never deep-copies the history.
- Every mutation of Rc-shared state (/model, /web-search-config, /mcp, /reload, /new) runs only while the agent is idle (`BUSY_COMMANDS`), so Cells there are race-free.
- Cache prefix: tools and the system prompt are frozen per session. All three adapters convert history deterministically and keep earlier items byte-identical after an append (scratch test passes).
- The three adapters already share what can be shared (`system_text`, `content_text`, `arguments_object`, `cap_output` in llm/mod.rs). The rest of each conversion is protocol-specific, and a shared trait would add a layer for nothing.
- `Settings` and `Client` behind `Rc` are replaced whole on every change (`set_search`, `switch_model`, `set_thinking`), which is the right shape for values like these.
- `prompt::Instructions` is plain data cloned into subagents, so nothing shared leaks between agents.
- Subsystems degrade rather than abort a turn: a snapshot failure turns undo off with a notice, and MCP and `later` errors become tool errors. On an early `?` return, `Process::drop` still kills the bash process groups.
