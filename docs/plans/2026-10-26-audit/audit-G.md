## Agent Loop & Tool Harness - Audit Results
**Score**: 2/4  (0 = critical issues everywhere, 4 = excellent)
**Files reviewed**: 23 (agent.rs, tools/{mod,bash,edit,files,search,web}.rs, plan.rs, swarm.rs, hooks.rs, skills.rs, later.rs, prompt.rs, mcp.rs, setup.rs, session.rs, llm/{mod,openai,anthropic}.rs in part, tui/mod.rs turn/command/agent-event paths, main.rs headless path, prompt/system.md, decisions 20–49)
**Issues found**: 25 (P0 1 · P1 5 · P2 11 · P3 8), plus extensions to [V9] and [V10]

The core invariant holds: in every await of `Agent::run` traced (hooks, request, parallel reads, bash/task waits, subagents, swarm, questions, plan approval, compaction's summary request), an Esc leaves history valid, because results are pushed only after each await and `recover` answers whatever the last assistant left open. The problems are in recovery from provider rejections, the compaction heuristics, the ordering of queued messages, and edge cases in tools.

All tests below ran in a scratch copy of the crate. Two test files were added there, `src/audit_g.rs` (included from agent.rs) and `src/audit_g_mcp.rs` (included from mcp.rs), with a small fake OpenAI SSE server and a fake MCP server in bash. Command (all 17 pass, which proves each bug exists):
`SP=/private/tmp/claude-503/-Users-wuhaotian-sphnet-com-sg-code-killi/a381b734-e6e5-4c1d-b105-209d2882b618/scratchpad; cd $SP/G/crate && RUSTUP_HOME=~/.rustup CARGO_HOME=~/.cargo HOME=$SP/G/home CARGO_TARGET_DIR=$SP/G/target cargo test --locked audit_g -- --nocapture --test-threads=1`
Each finding names its test, for example `g7`.

### P0 - Critical (must fix before merge / blocks production)
- **[agent.rs:566-573, llm/mod.rs:324-331, tools/files.rs:83-134]** An image the provider rejects breaks the session for good. Every later request fails with the same 400, `/compact` included, and only `/new` gets out.
  Scenario: `read logo.png`, where the file is a Git LFS pointer (text), or a PNG saved as `.jpg` (Anthropic checks the media type against the bytes). Or the image is under 500 KB but too large in pixels: over 8000 px, or over 2000 px once a request holds more than 20 images, which screenshot-heavy sessions reach because ~300 KB screenshots are never shrunk (SHRINK_ABOVE is 500 KB). Or `/model` moves from an image model to a text-only provider while images are in history. No adapter removes `image_url` parts when `model.images` is false, so this happens even with [V1] fixed. `image_type` decides by extension only, and `image_url` base64-encodes whatever bytes are in the file. The 400 is fatal (`classify`), and `is_overflow` doesn't match it. So the image stays in history, and every turn and every `/compact` (Full mode sends the images too) gets the same 400.
  Impact: The user loses the conversation (`/new` is the only way out). Decision 26 says "When the provider rejects a request: reduce, then strip, older media", but that is implemented only inside `reduced()` for compaction requests.
  Verified: tested (`g7`). A fake provider that rejects `image_url` fails the first turn, an unrelated second turn, and `/compact`. The specific rejection reasons are known Anthropic/OpenAI API behavior and were not checked live.
  Suggested fix: (1) Check magic bytes in `files::image_url` (PNG, JPEG, GIF, WebP), take the media type from the bytes, and refuse anything else. (2) In `Agent::run`, when a non-retryable 4xx follows a request whose history holds images, run one `compact_with(Reduced)` (which already drops `image_url`) or a rewrite that only strips media, logged as a `compaction` record, then retry once. Breaking the cache is justified because the alternative is a dead session. Also strip images in the adapters, or on `/model`, when the new model lacks `image_in`.

### P1 - High
- **[tui/mod.rs:1768-1783, :442-446, :537-541; agent.rs:626-630]** Queued messages that outlive their turn disappear from the screen, then reach the model *after* its next prompt, so an old instruction reads as the newest one.
  Scenario: The user types "stop, use tabs instead" while the agent works, and the turn then ends in an error (for example after a run of retries). `finish_turn` runs `queued.clear()` but clears `controls.steer` only in the Esc arm. The same happens when the message is typed while a `/btw` answer is still pending at the end of a turn (`run_turn` keeps reading keys until the aside answers), during `/compact`, or during a Stop hook. On the next prompt, `run` pushes the new prompt and then `before_request` drains the old queue after it.
  Impact: The model obeys an outdated instruction placed as the newest message. In the error and `/btw` cases the user saw the queue vanish and may type it again, so it gets sent twice.
  Verified: tested agent side (`g4`: the first request ends `user: format the file with spaces`, `user: actually, use tabs`). The TUI paths were traced.
  Suggested fix: When `finish_turn` ends a turn without an interrupt (and when `/compact` ends) and `steer` is non-empty, either set `send_now` so `read_input` sends the queue as the next prompt, which is the Ctrl+S path, or clear it with a notice. Never leave it queued silently.
- **[agent.rs:693-718, :655-661, :1319-1332]** Compaction has no floor and no check that it made progress, so a context it can't shrink compacts again before every step.
  Scenario: The part compaction can't touch is over `limit - reserve`. That part is the system prompt and tools, plus the instruction files (decision 25 mentions a 47 KB AGENTS.md, about 12k tokens), plus up to 80k characters of kept prompts (about 20k tokens in English, far more in CJK). This happens on a 32k–64k window, or with a low explicit `compact_at`. After each compaction, the first request reports a context above the limit, so the next `before_request` compacts again.
  Impact: Every step costs an extra full-context summary request, and the work in progress is re-summarized at each step. The cause logging from decision 45 would show it, but nothing stops it.
  Verified: tested (`g2`: 8 requests for a 4-step run, 4 of them summaries).
  Suggested fix: After a compaction, remember the context the first request reports. If that alone exceeds `limit - reserve`, stop proactive compaction (the overflow path still protects) and show one notice: "instructions and kept requests alone exceed compact_at; raise it or use a larger window".
- **[agent.rs:1216-1217, :1243, :1252]** A fork starts with `stats.context = 0` and `accounted_messages = 0`. Its first step therefore estimates the inherited history at chars/2 (about twice its real size) and compacts what it was forked to inherit.
  Scenario: Window 200k, so the limit is 120k and the threshold 90k. The parent holds 70k billed tokens. `agent {fork: true}` gives an estimated suffix of 141,736, so the child's first act is a summary request over the whole inherited conversation, and it then works from a summary.
  Impact: This defeats decision 20's reason for fork ("cheap because that history is already cached"). It costs an extra large request, and the subagent loses the detail it was forked for.
  Verified: tested (`g11`).
  Suggested fix: In `subagent()`, when `fork` is set, use `accounted_messages = history.len()` and `stats.context = self.stats.context`. The fork history is exactly the prefix the parent's last request was billed for. `run_children` doesn't merge `context`, so this is safe.
- **[mcp.rs:295-300]** An MCP image that isn't attached goes into the tool result as raw JSON with its base64.
  Scenario: The model can't see images (text-only, so `images=false`), or a tool returns several images (only the first is attached, `image.is_none()`). Every other image part falls through to `_ => text.push(part.to_string())`, and so does a `resource` with a `blob`.
  Impact: Up to 50k characters of base64 per call (cut to a head and tail of about 5 KB, plus a spill file, above that) enter the context as noise. This applies to the user's godot, blender and unity servers whenever they return screenshots to a text-only model.
  Verified: tested (`m1`: text holds `{"data":"BBBBSECONDIMAGEBASE64BBBB",...}`, and both images for a text-only model).
  Suggested fix: For parts that aren't attached, emit `(image omitted: image/png, N KB — this model can't see images)` or `(extra image omitted)`, and do the same for blob resources and audio.
- **[agent.rs:954-971, plan.rs:5-41]** Plan mode doesn't hold on the harness side. `mcp` and native MCP tools are dispatched *before* the plan checks (`:956-957` before `:963-971`), and the bash allowlist lets commands that write through. (E owns the full allowlist review; this is the harness side.)
  Scenario: In plan mode, `mcp {server:"blender", tool:"execute_code"…}` or a native `mcp__unity__…` call runs unchecked. Bash: all 10 of `git branch -D main`, `git tag -d v1.0`, `git tag v9.9`, `sort -o src/main.rs src/main.rs`, `cargo clippy --fix`, `fd -x rm {}`, `rg --pre ./script.sh`, `git diff --output=src/lib.rs`, `tree -o out.txt`, `find … -fprintf out.txt` are allowed. `find` is checked against `-fprint` only.
  Impact: Plan mode ("research and plan only", decision 23) can change files, branches and outside programs.
  Verified: dispatch order traced; allowlist tested (`p1`).
  Suggested fix: Move the `mcp` and native arms below the plan checks, refusing MCP `tool` calls in plan mode and allowing only listing (`mcp {server}`). Treat prefixes like `git branch` and `git tag` as read-only only with no further arguments, or with listing flags, and block `-o`, `--output`, `--fix`, `-x`, `--exec`, `--pre`, `-fprint*` and `-fls`.

### P2 - Medium
- **[agent.rs:425-431, :715]** After `/model`, proactive compaction can't see the conversation already there: `switch_model` sets `stats.context = 0` but leaves `accounted_messages`, so the suffix estimate covers only new messages.
  Scenario: 100k tokens of history on a 200k model, then `/model` to a 64k-window model. Pressure becomes `context=0 suffix=0 limit=38400`, so the next request sends about 100k tokens. Recovery depends on `is_overflow` matching that provider's text (affected by [V5]; Moonshot's "exceeded model token limit" isn't in the list, unverified), and when it matches it is the lossier Reduced compaction.
  Impact: A wasted failing request. With an unrecognized overflow text, every turn fails until the user switches back or runs `/compact`.
  Verified: tested (`g1`).
  Suggested fix: Keep the last billed `stats.context` on a switch (the meter showing more than 100% of the new window is the right signal), or set `accounted_messages = 0` so everything is estimated.
- **[agent.rs:625-662]** `before_request` pushes its notes first and compacts last. When a proactive compaction fires, everything just pushed is summarized at once, and the plan, swarm and peer flags are reset but not announced again until the following step.
  Scenario: In plan mode, with compaction due, the first request after it is `[system, summary]` with no "Plan mode is on". In the same call, a finished-task note (`reported=true`), a nested AGENTS.md note (`known`), and a peer `tell` are pushed and compacted straight away, so they are never repeated. A steered message becomes a `<request>` in the summary instead of the newest message.
  Impact: For one step the model doesn't know plan or swarm mode, or that peers are in the tree (decision 49 resets these flags precisely to avoid this). Notices can vanish into the summary.
  Verified: tested (`g3`). The loss of notes was traced.
  Suggested fix: Compact first in `before_request`, then push the steered messages, mode reminders, peer notes, task notes and AGENTS.md notes. Everything stays appended at the end, so the cache rules hold.
- **[agent.rs:578-581, :615; :1169-1170; main.rs:229]** After `length` continuations, `run` returns only the *last* reply's text, so a subagent report that hit the output limit reaches the parent without its beginning. `-p --json`'s `final_text` has the same problem.
  Scenario: The child's report is "FINDINGS PART ONE: the bug is in parser.rs line 40 because…" (`finish_reason: length`), then " the index is off by one." The parent's tool result is only the second part.
  Impact: The parent acts on a fragment that has lost the key facts, and an eval harness reading `final_text` sees a truncated answer.
  Verified: tested (`g12`).
  Suggested fix: Concatenate `reply.text` across `length` continuations within a turn, and return the whole.
- **[agent.rs:597-607, :1643-1654]** The open-todos reminder fires even when the model ended its turn by asking the user something in prose. It says "If work is left, do it. Don't repeat your last reply."
  Scenario: Midway through a todo list the model writes "Should I migrate the schema or add a shim?" and stops. killi adds a step that pushes it to continue (choosing for the user), and the final message no longer carries the question. It also costs a request at the end of every turn that ends with open items, including a multi-turn plan the user asked to pause.
  Impact: The model may act without the user's decision, plus a wasted request. Only the stale-list rationale is recorded (CHANGELOG 0.2.25); this case isn't covered in decisions.md.
  Verified: traced. Whether the model proceeds depends on the model.
  Suggested fix: Word the reminder so stopping is allowed ("If you are waiting for the user, leave the items open and stop; don't repeat the question"), or skip it when the final reply ends in a question.
- **[tui/mod.rs:513-533]** (lead 5, confirmed) `/undo` reverts files but only calls `agent.mark_interrupted()`. The model is told "The user interrupted your previous turn." even after a turn that completed, and is never told which files changed back.
  Scenario: `/undo` restores 3 files, then "now also add X". The model edits from its stale view: `edit` misses (wasted reads), or `write` rewrites a whole file from memory and brings back the undone change.
  Impact: The model is misled about the state of the files, which can undo the undo.
  Verified: traced (the list of `changes` is at hand at :526).
  Suggested fix: Add `Agent::note_undo(changes)` so the next prompt says "The user undid your last turn with /undo; these files are back to how they were before it: …; read them again before editing." in place of the interrupted note.
- **[tools/search.rs:19, :343-345; tools/files.rs:26-33]** `read` refuses files over 10 MB with "Search it with grep", and `grep` skips files over 10 MB without a word and answers "No matches found." (not an error).
  Scenario: An 11 MB `server.log` ending in `FATAL: …`. `read` sends the model to grep, and `grep FATAL path=server.log` reports no matches.
  Impact: A false negative from a tool: the model concludes the error isn't there. Non-multiline grep streams line by line, so the cap protects no memory there.
  Verified: tested (`t1`).
  Suggested fix: Apply `MAX_FILE_SIZE` only in multiline mode (which reads whole files), and report skipped files ("skipped server.log: over 10 MB").
- **[mcp.rs:252]** `mcp` `arguments` given as a JSON *string* (a common mistake, which `edit` accepts for `edits`) is silently replaced by `{}`.
  Scenario: `{"server":"t","tool":"x","arguments":"{\"view\":\"main\"}"}`. The server receives `{}` and runs with defaults, and the result isn't marked as an error.
  Impact: The tool silently does something else; decision 26 says "never a silent `{}`".
  Verified: tested (`m2`: the server answered `got {}`).
  Suggested fix: Parse a string `arguments` as JSON. If that fails, or any other non-object is given, return an error showing what was sent.
- **[tools/web.rs:202-207]** `fetch_url` cuts text at 50 KB with "[Truncated: N more characters.]", with no spill file, no offset parameter, and no way to the rest.
  Scenario: An API reference whose relevant section lies past 50 KB of extracted text. Fetching again returns the same head, and the system prompt steers the model away from curl.
  Impact: The model can't reach the information, and wastes round trips. Decision 26 requires "truncation hints that say exactly how to get the rest".
  Verified: traced.
  Suggested fix: Save the full text with `tools::spill`, as bash and `shorten` do, and say "Full text saved at X; read it with offset."
- **[agent.rs:1241; tui/mod.rs:1937-1956, 1977-1980]** (lead 22, confirmed) Sibling `agent` subagents start from the same cloned id set, so a provider whose ids repeat per conversation (`call_1`, Kimi-style `functions.read:0`; killi's own `fake_llm.py` does this) gives every sibling the same ToolStart id. `take_running(id)` then takes the wrong sibling's row.
  Scenario: Two parallel `agent` calls both emit `call_1__2`. B's ToolEnd removes A's row and prints A's summary under B's label, and the reverse.
  Impact: The `↳` rows show the wrong command and elapsed time per subagent. Display only; the model is unaffected and swarms, keyed by slot, are fine.
  Verified: tested (`g6`: `[("a","call_1__2"),("b","call_1__2")]`).
  Suggested fix: Have `take_running` match `(label, id)` for `Sub` events, or tag ids with a per-child index in the sink wrapper in `subagent()`. Either is UI only, and history is unchanged.
- **[agent.rs:1247, :632, :963-970, :1072-1074]** A fresh (non-fork) subagent in plan mode is never told that plan mode is on. Its history is only the system prompt, and depth > 0 never announces. When it is blocked, the message tells it to "call exit_plan_mode", which then answers "Only the main agent can leave plan mode".
  Scenario: The parent, in plan mode, delegates research. The child calls `write notes.md`, is blocked, calls `exit_plan_mode`, is refused, and only then reports.
  Impact: Wasted subagent steps, and contradictory instructions.
  Verified: tested (`g9`).
  Suggested fix: When plan mode is on, start a fresh child's history with `plan::ON` after the system message (the child's history is new, so no cached prefix is lost). At depth > 0, the block message should say "report your plan instead".
- **[agent.rs:842-860, 925-949]** (lead 17, bypass confirmed; fail-open is E's) PreToolUse and PostToolUse never run for top-level `agent` or `agent_swarm` calls, which skip `execute`. A hook that blocks every tool still lets subagents start and spend requests.
  Impact: A Claude-Code-style hook guarding `Task`/`agent` (cost or audit) is silently ignored, against decision 22's protocol parity. Configs have no hooks today (decision 22), which limits the reach.
  Verified: tested (`g10`: a child request was made, and the parent got "child report").
  Suggested fix: Run PreToolUse per call before `run_subagents`/`run_swarm` start children (a block becomes that call's error result), and PostToolUse on each result.
- **Extensions to verified issues (not counted above).** [V9]: a child shares `ctx`, so it can also `task_stop` or `task_output` the parent's tasks, and the parent is told about tasks its children left running (`newly_finished`, `left_running`) that it never heard of. [V10]: in the normal read→edit loop, `read` shows U+FFFD, the model copies it into `old_string`, the exact match on the decoded text succeeds, and the write-back turns every invalid byte in the file into U+FFFD. [V8] confirmed: `assemble` sets `children: 0` (agent.rs:412), and `Session::child` uses `create_new` (session.rs:74-79).

### P3 - Low / Nits
- **[agent.rs:531-549]** `run` consumes `interrupted`, the MCP toggle notes and the instruction-change notes, and sets `session_started`, *before* the hook awaits. A UserPromptSubmit hook that blocks (or an Esc during a slow hook) drops them all, and the SessionStart context is lost for the rest of the session. Only with hooks configured. Tested (`g5`). Fix: take the notes after the hooks, or put them back on `Err`.
- **[agent.rs:1344]** `recover` marks the next prompt as interrupted only when calls were left open or the last message is a user message. An Esc during the request that follows tool results isn't noted, so the model continues the old task as if nothing happened. Tested (`g8`). Fix: mark it whenever the last message isn't an assistant reply.
- **[agent.rs:940-942, :900-902; prompt.rs:200-211]** Discovery of nested AGENTS.md uses only `args["path"]`. bash's `cwd` is never checked (decision 25 says it is). A directory path (`grep path=web`) skips `web/AGENTS.md`, because `touched` walks `relative.parent()`. Found files stay in `known`, so after a compaction the reminder is gone and never repeated. Traced. Fix: also call `touched` for the bash `cwd` and directory paths, and announce known nested files again after a compaction.
- **[tools/web.rs:139]** `pdftotext` has no timeout, no `own_session` and no `kill_on_drop` (intel-0b obs 9, which contradicts decision 39's "every child gets its own session"). A pathological PDF holds the call until Esc and then keeps running orphaned. A PDF cut at the 10 MB download cap fails as "pdftotext could not read", without saying it was cut. Traced.
- **[agent.rs:1022-1036; later.rs:45-55]** `later done N`: the model is never shown item numbers (`add` says only "N open"), and numbers shift after each removal, so crossing off two items by their original numbers removes the wrong second one. Other sessions share the list too. Traced. Fix: say "noted as item N", and accept the item's text.
- **[tools/files.rs:48-52; tools/bash.rs:375, 531, 578]** Numeric arguments sent as strings, which models often do: `read {"offset":"300"}` silently reads from line 1, a bash `timeout: "120"` silently becomes 60, and `task_output {"id":"1"}` fails with "needs `id`" without saying it must be an integer. Traced. Fix: accept numeric strings.
- **[tools/files.rs:58-59; tools/search.rs:18; swarm.rs:83-84; agent.rs:1000]** Caps disagree with the 50k result cap. A truncated long line (`[line truncated: N chars]`) gives no way to read the rest. `glob` returns up to 1000 paths, which a long tree pushes past 50k characters, so `shorten` keeps the head and tail. A swarm of many long items gets a report share floored at 200 that can exceed the limit and gets cut to head and tail as well. Traced.
- **[agent.rs:828; hooks.rs:45; agent.rs:543]** Any hook, even a Stop-only one, makes read, grep and glob batches serial (`hooks.is_empty()`; checking PreToolUse/PostToolUse would do). SessionStart always sends `source: "startup"`, even on resume. Claude Code scripts that read `tool_input.file_path` get nothing, because killi sends `path`/`edits`, so decision 22's "work unchanged" holds only for bash hooks. Traced.

### Checked and fine (one line each: what you looked at that holds up — saves the next reader time)
- Esc at every await of `run`/`execute_calls` leaves history valid: results are pushed only after their await, and `recover` answers the last assistant's open calls (repo test `a_message_typed_during_a_call…`, and traced per await).
- Compaction can't be half-done: `self.messages` is replaced only after the summary reply, so an Esc mid-summary leaves history and the log as they were. An empty summary leaves a `warn` and changes nothing.
- Consecutive user messages (steer, plan/swarm/peer reminders, task and AGENTS.md notes, images, continuation reminders) are valid: OpenAI chat and Responses accept them, and Anthropic's `push_user` merges them with tool results first.
- `SKIPPED_FOR_MESSAGE` answers every remaining call before the steered message is appended, and images go in one trailing user message after all results.
- `side_request` adds placeholders only for open calls and copies history before awaiting; the `share()`-after-`make_mut` invariant is kept at both sites.
- `unique_id` meets the id rules of Anthropic and OpenAI (charset, 64 characters) and renames duplicates within an agent.
- `reduced()` keeps the 8 newest messages verbatim, drops oversized signed reasoning as a whole bundle, and strips images.
- `edit` multi-edit is atomic (everything matched against the original, overlaps rejected, no write on any failure). The `Normalized` byte map returns correct original offsets, including folded multi-byte characters.
- bash `tail`/`failed_tail`/`floor_boundary` slice only on UTF-8 boundaries. The process group is killed on drop; `Waiting`, the hook `Group` and Chrome `Running` guards clean up on interrupt.
- `task_output`'s `expect("task exists")` is safe (`Tasks::list` never removes), and its borrow is scoped before the await.
- The swarm budget (128 per turn) is enforced, a duplicate prompt is rejected, and reports keep item order.
- The `task_output` repeat key, which includes the result, is decision 36's deliberate choice; the breaker otherwise stops at 8 calls with the same call and the same result.
- `read` paging hints are exact (`Use offset=N`), the 10 MB refusal points to working tools, and images are gated on `ctx.images` (but see [V1]).
- The MCP gateway refuses servers that are off with a clear message, lists tools with their full schemas, and caches the listing per session.
