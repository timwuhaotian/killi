## Code Quality - Audit Results
**Score**: 3/4  (careful code overall: almost every slice is boundary-checked, preconditions hold, tests are thorough. The problems are a few local bugs: one keystroke crash, one tool contradiction that misleads the model, tab/control-character width in the live area, and several places where duplicated logic has drifted.)
**Files reviewed**: 36 of 39 read in full or in the relevant parts. All 39 were also scanned with strict clippy and grep. hooks.rs, update.rs and login.rs were checked by grep only.
**Issues found**: 21 (P0 0 · P1 2 · P2 6 · P3 13)

Method: strict clippy run on a scratch copy (`-W clippy::indexing_slicing -W clippy::string_slice -W clippy::arithmetic_side_effects -W clippy::unwrap_used -W clippy::expect_used -W clippy::cast_*`; 886 warnings, every string-slice, slice, unwrap, expect and cast site triaged by hand). Scratch tests in `scratchpad/A/crates/...` (run with `CARGO_TARGET_DIR=scratchpad/A/target cargo test --locked audit_`). Real-pty reproductions with a modified `scripts/frames.py` copy (`scratchpad/A/scripts/frames_a.py`, plus a `fake_llm.py` copy with a TABCMD scenario), run against a debug build of the unchanged source.

### P0 - Critical (must fix before merge / blocks production)
None.

### P1 - High
- **[tui/menu.rs:56-58,67]** `menu::suggest` slices `&typed[space + 1..]` after `typed.find(char::is_whitespace)`, which assumes the whitespace is one byte. The same assumption is in `start: 1 + space + 1`.
  Scenario: the prompt is `/effort` (or `/model`, `/btw`, `/compact`, any `/name`) followed by U+00A0 (Option+Space on a Mac keyboard, a common slip) or U+3000 (the space a CJK IME types). `suggestions()` runs on every key and every draw, so the next keystroke panics with `byte index 7 is not a char boundary; it is inside '\u{a0}'`. The busy-turn menu goes through the same path, so it also fires mid-turn.
  Impact: release builds use `panic = "abort"`, so killi dies. A turn in flight is lost, and background process groups are orphaned (see the panic-hook P3).
  Verified: tested (`suggest("/effort\u{a0}")` and `"/model\u{3000}x"` both panic) and reproduced in a pty: type `/effort`, then send `\xc2\xa0`, and the process exits with the panic message on screen (`frames_a.py --scenes nbsp --inline`).
  Suggested fix: `let (name, argument) = typed.split_once(char::is_whitespace)?;` and `start = before.len() - argument.len()`. Add the two inputs above to `menu.rs` tests.

- **[tools/search.rs:19,343-345 vs tools/files.rs:13,26-33]** `read` refuses files over 10 MB with "too large to read whole. Search it with grep…". `grep` silently `continue`s past every file over `MAX_FILE_SIZE` (also 10 MB). It adds no note and doesn't mark the result incomplete, so a grep on that exact file returns "No matches found.", not as an error.
  Scenario: the model debugs a 12 MB `app.log`. `read` says to use grep, `grep "ERROR"` says "No matches found.", and the model concludes the log is clean.
  Impact: a silent false negative in what the model receives, triggered by the tool's own advice. Common with logs, data dumps, lock files and minified bundles.
  Verified: tested (`audit_read_sends_big_files_to_grep_which_skips_them`: read gives the grep advice, grep on the file returns `No matches found.` with `is_error=false`, although the file contains the pattern).
  Suggested fix: the non-multiline content and count paths already stream (`text_reader` + `next_line`), so drop the size skip there. Keep a cap only for `multiline` (which `fs::read`s), and in that case record the skipped files in `notes` and treat the result as incomplete.

### P2 - Medium
- **[tui/text.rs:224-239 `width`; tui/text.rs:612 `ellipsize`/`fit`; tui/editor.rs:251-272 `render`; tui/mod.rs:2646-2793 `live`]** Live-area rows and the prompt editor are never tab-expanded or stripped of control characters, and `text::width` counts `\t` and `\n` as 0 columns. Committed entries are safe because `history::clean` (history.rs:47) runs in `Entry::render`, but `fit()`, `Editor::render` and the live rows bypass it.
  Scenarios, all reproduced in a pty:
  1. Pasting tab-indented code (Go, Makefiles, TSV) into the prompt puts the cursor at the wrong column and makes rows wider than the window. At 80×24 inline, the bottom rule was drawn on the wrapped tail of a code line. At 60×24 fullscreen, characters of the pasted line were overwritten ("…long message t" / "the edge…").
  2. A bash call whose command has a TAB (a model writing `cut -d'\t'` in JSON) plus a todo whose content has `\n`, at 80×24 inline. The screen showed the running bash row twice, a stale "◓ todo_write · 0s" row that had already finished, "me/ve…" spilled onto its own row, and the todo's "then count" detached on a row of its own.
  The same unsanitized live rows include the swarm description and `doing`, the queued rows and the thinking row (`first_sentence` keeps tabs and `\r`).
  Impact: corruption in the default inline renderer. Rows that the per-row repaint (`changed_rows`) assumes are one screen row each wrap, so ghost and duplicated rows pile up until the next full redraw.
  Verified: reproduced (`frames_a.py --scenes tabpaste tabcmd`, inline and fullscreen; screen dumps in `scratchpad/A/frames-inline.txt`, `frames-full.txt`, `frames-tab.txt`). Also tested: `audit_tab_in_prompt` gives cursor col 4 for `"> a\tb"` where it should be 9, and `audit_fit_with_tab_and_newline`.
  Suggested fix: run the display text through `history::clean` (tab to spaces, drop other controls) inside `ellipsize` and in `Editor::render`, computing cursor columns on the cleaned text. The sent text stays as typed. Collapse whitespace in todo content and summaries the way `Swarm::new` already does for items. This is the width side of intel lead 6, which B owns for the escape-injection side.

- **[tui/mention.rs:71-73 (`mentions` uses `words`), :94-114]** `words()` is the shell-style tokenizer meant for dropped paths, and `@` mentions use it too: any `'` or `"` opens a quote that swallows the rest of the prompt.
  Scenario: "what's wrong in @a.rs" tokenizes to `["whats wrong in @a.rs"]`. No word starts with `@`, so the file is silently not attached. The same happens to "I don't get @a.rs", and to `/skill args` and `/swarm args`, which go through the same function.
  Impact: decision 40's attachment silently fails for ordinary English prompts that have an apostrophe before the mention. The `@` menu found the path (it splits on whitespace, `menu::word_start`), but `expand` tokenizes differently, so two tokenizers for one concept have drifted. The model then pays a `read` or answers without the file.
  Verified: tested (`audit_apostrophe_swallows_mention`).
  Suggested fix: give mentions their own splitter (whitespace with backslash escapes, which is what `mention::escape` and the menu insert) and keep quote handling for `paste::dropped_files` only.

- **[setup.rs:65-72; mcp.rs:186-207,331-337]** The eager list is `config.mcp.eager` followed by the project's `.killi/config.toml` eager list, then `retain`, then `eager.dedup()`. `Vec::dedup` only removes consecutive repeats.
  Scenario: global `eager = ["godot", "blender"]` and project `eager = ["godot"]` combine to `["godot", "blender", "godot"]`. `start_eager` (cached `tools()`) pushes godot's native definitions twice into the frozen tool list.
  Impact: the same tool name appears twice in every request of every session started in that project. Anthropic answers 400 "tool names must be unique" (not checked against a live API, per the rules), so each session there fails on its first request. `native_name` truncating at 64 characters and mapping punctuation to `_` can collide the same way, and `self.native.insert` then routes calls to the wrong tool.
  Verified: tested (`audit_eager_listed_globally_and_per_project_is_defined_twice` gives `["mcp__t__echo", "mcp__x__echo", "mcp__t__echo"]`). The provider rejection itself is unverified.
  Suggested fix: dedupe with a `HashSet` in `Setup::new`. In `start_eager`, skip a native name already inserted and report the clash in `problems`.

- **[config.rs:1377-1383, 1311-1314 vs login/opencode_go.rs:134-141]** The opencode importer has drifted from the OpenCode Go login. The login maps `@ai-sdk/openai` to `openai_responses` and trims `/v1` from an `@ai-sdk/anthropic` URL, because "killi adds `/v1/messages`". The importer maps `@ai-sdk/openai` to Chat Completions (its explicit arm is identical to the `_` fallback) and copies `options.baseURL` verbatim.
  Scenario: in an opencode config, an `@ai-sdk/anthropic` provider's `baseURL`, which by the AI SDK convention ends in `/v1`, becomes `https://gw/v1` + `/v1/messages` = `…/v1/v1/messages`, giving a 404 on the first request after the first-run import.
  Impact: imported providers break silently. The existing test (config.rs:1921) covers no anthropic or openai npm case.
  Verified: traced. anthropic.rs:39 builds `{base_url}/v1/messages`. The AI SDK baseURL convention is from its docs, not tested here.
  Suggested fix: share one npm-to-(protocol, base_url) function between `opencode_protocol` and `go_models`, and add an import test with an anthropic provider.

- **[agent.rs:46-52 `kept_prompt`; also agent.rs:1355-1366, tools/files.rs:58-59, tools/web.rs:202-206, agent.rs:1453-1455, 1473]** Byte lengths are used and reported as "characters". `kept_prompt` counts `chars()` but cuts with byte-based `llm::truncate`.
  Scenario: compaction keeps a 5,000-character Chinese prompt as its first 1,333 characters and tells the model "[… 1000 characters cut]" when 3,667 were cut. A 1,334 to 4,000-character CJK prompt, which decision 43 says stays whole, gets cut. Likewise, `shorten` writes "N characters omitted" with a byte count (`MAX_RESULT_CHARS` is compared with `.len()`), read writes "[line truncated: N chars]" and fetch writes "[Truncated: N more characters.]", all in bytes.
  Impact: after compaction a CJK user's own requests lose up to two thirds of their words, and the model gets wrong numbers in several truncation notes.
  Verified: tested (`audit_kept_prompt_counts_chars_but_cuts_bytes`). The other sites are traced.
  Suggested fix: add a `truncate_chars` (cut at `char_indices().nth(n)`) and use it wherever the limit is named in characters, or rename the constants and texts to bytes.

- **[main.rs:299-322 (`Printer::event`)]** The headless `-p` counterpart of [V2]. Deltas are printed to stdout as they stream. When an attempt fails mid-reply, `Event::Retry` only ends the line (`end_text`), and `last` stays `Text`.
  Scenario: stdout gets "Here is the fi\n" followed by the full retried reply.
  Impact: scripts that capture `killi -p` stdout get duplicated, broken output after a transient mid-stream failure. `--json` is unaffected.
  Verified: traced (llm/mod.rs:223 emits text deltas before the failure; main.rs:319-322).
  Suggested fix: in `-p` mode, hold the text of the current attempt and flush it only once the request succeeds (or once a newline arrives and no retry is pending), or note on stderr that the partial above was discarded.

### P3 - Low / Nits
- **[tui/mod.rs:3843-3847 vs main.rs:134-136]** The panic hook calls `restore_terminal()` only. `quit_on_signals` also calls `tools::kill_all()` and `live::leave()`. Under `panic = "abort"` no `Process::drop` runs, so bash and background groups (each in its own session via setsid) keep running unless SIGPIPE happens to end them, and the live socket is left behind. This makes the P1 crash worse. Verified: traced. Fix: call `tools::kill_all()` and `live::leave()` in the hook (GROUPS is only locked briefly; `try_lock` would make it safe). Related to K's lead 18.
- **[Doc comments that contradict the code]** Lead 22 is confirmed: config.rs:99 says `compact_at` is "default 256000". The code uses 60% of the window, or 256k only when the window is unknown, with an 85% ceiling and 0 to disable (agent.rs:704-711). `skills/update-config.md:52` already says this correctly. Four doc blocks belong to a neighbor: agent.rs:1673-1674 (`LEFT_RUNNING`'s doc sits on `SKIPPED_FOR_MESSAGE`), tools/mod.rs:104-106 (`summary`'s doc sits on `count`), tui/mod.rs:3684-3686 (`queued_row`'s doc sits on `working_row`), and setup.rs:159 (a stale "What the system prompt includes, in one line." on `overview()`). Verified: traced.
- **[tools/mod.rs:139]** `summary()` does `detail.replace(&cwd, ".")`, which mangles sibling paths (`/Users/me/code/killi-old/src/main.rs` → `read .-old/src/main.rs`). With cwd `/`, every slash goes (`/etc/hosts` → `.etc.hosts`). The label ends up in the model-visible "Image from …:" text (agent.rs:835,865), and `Printer` repeats the replace (main.rs:328). Verified: tested. Fix: strip only a `{cwd}/` prefix at a path boundary.
- **[tui/mod.rs:3281-3299]** `error_hint` substring-matches "401", "403", "404" and "429" anywhere in the error text, and the auth check runs first. "prompt is too long: 214013 tokens > 200000 maximum" therefore gets "Check api_key…" in place of the /compact hint. Verified: tested. Fix: match `HTTP 401` and similar, or the status the message starts with.
- **[tui/mod.rs:3391-3395 vs agent.rs:202]** `first_sentence`, used on the live "∴ thinking" row and the "Thought for …" line, cuts at the first `.` anywhere ("Looking at main.rs to…" → "Looking at main."). `fit_recap` requires a space or non-ASCII character after the `.`. Two sentence detectors have drifted. Verified: traced.
- **[tui/mod.rs:3199-3211]** `clip` checks `width(out) + 2 > max` before pushing, so a wide character at the edge overflows by one column (`clip("abcdefgh中文字", 10)` has width 11). Picker names are often CJK first prompts, and their column misaligns. It is also O(n²). Verified: tested.
- **[tui/text.rs:335-345]** `Rows::place` pushes leading spaces with no width check while `placed` is false. A line with at least a window's width of leading spaces yields a row wider than the window (100 spaces plus "x" at width 60 gives a row of width 100). In fullscreen, `Row`/`Words` entries aren't re-wrapped, so the row spills. Verified: tested (`audit_leading_spaces_overflow`).
- **[config.rs:1458-1461]** `read_yaml` reports "could not read the YAML … past line {at}" with `at = lines[0].1`, the first line, not where parsing stopped. Verified: traced.
- **[config.rs:587 vs 751]** `set_provider_key` recognizes the table header with `starts_with(header)` and `remove_provider_key` with `==`, two rules for the same in-place edit. Both miss `[providers.x] # comment` and `[ providers.x ]`. Verified: traced. H owns the write-safety side (lead 19).
- **[Swallowed errors that mislead]** These are all traced.
  - skills.rs:97: an unreadable or deleted SKILL.md gives "No skill named `x`. Closest: x". `/x` then says "Unknown command".
  - later.rs:31: `read_to_string(..).unwrap_or_default()`, so a read error (for example a non-UTF-8 hand edit) makes the next `later` or `done` overwrite the list with only the new item, losing the old ones. Lines not starting with "- " are dropped on every save.
  - prompt.rs:80-88: a non-UTF-8 or unreadable AGENTS.md is silently left out of the system prompt.
  - setup.rs:227-228: a malformed project `.killi/config.toml` silently disables its eager servers.
  - tools/web.rs:163-173: `capped_body` cuts a PDF over 10 MB without saying so, so pdftotext's "couldn't read" hides the real cause.
- **[tui/mention.rs:50-57]** `attachment` does `fs::read` on the whole file before checking `is_binary` and cutting at 50 KB. `@demo.mov` (several GB) is read synchronously on the runtime thread just to report `binary="true"`. Verified: traced. Fix: read at most `MAX_BYTES + 1`. This may overlap with C.
- **[tui/mod.rs:2464-2471, 1563-1565]** A /web-search-config "secret" answer is typed into, and prefilled with the current key in, the visible prompt editor. `Editor::take` (editor.rs:86) pushes it into the ↑ history, so an accidental ↑ Enter sends the API key to the model and the session log. `secret` only changes the echo. Verified: traced. This may overlap with E.
- **[llm/openai.rs:138-140; agent.rs:767-771 vs 1188-1192]** Drift-prone duplication.
  - A mid-stream `error` is classified with `code as u16` (truncating) through `from_status`, with no `QUOTA_WORDS` check, while the HTTP path (`classify`) checks those words. The three adapters classify mid-stream errors three different ways. F owns the adapters.
  - Usage is summed field by field in `account`, `run_children` and the TUI's `add_usage`. They agree today; an `AddAssign for Usage` would keep a fifth field from being missed.

### Checked and fine
- agent.rs:1511 `expect("a message is an object")` is unreachable: it is only reached when `message["role"] == "assistant"`, which implies an object. **intel-0a lead 11's `reduced` expect is refuted.**
- anthropic.rs:191 `expect("user content is a list")`: every user message in `out` is built with an array.
- bash.rs:553 `expect("task exists")`: tasks are never removed from `Tasks.list`. All `expect("… lock")` calls on std `Mutex` are safe because nothing can poison under `panic = "abort"`. The mcp.rs and broker.rs stdio `expect`s come right after `Stdio::piped()`.
- tui/mod.rs:436-437,1148: the `select!` branch expects sit inside `async` blocks that are never polled while their precondition is false.
- `choice_rows` (`options.len() - visible`): every caller bounds `visible ≤ len`, including picker count 0. `on_key_menu`/`menu_rows` `count - 1` and `% count`: `suggest`/`files` return `Some` only with items.
- `llm::truncate`, `cut_middle`, `shorten`, bash `tail`/`failed_tail`/`floor_boundary`, the edit.rs `Normalized` byte map (multi-byte folds included) and its `line_diff`, and the YAML parser's slices are all boundary-safe, with every subtraction precondition met. The YAML slices are on ASCII delimiters, and the quote and bracket strips are length-guarded.
- `finish_turn` and `prompt_turn` `Totals` subtractions are monotone within a turn. `compaction_due`'s `limit - reserve` is safe because reserve ≤ limit/4.
- The two token estimators (agent.rs `estimate_chars` at chars/2, llm `estimated_value_bytes` at bytes/4) differ on purpose and both reserve `IMAGE_TOKEN_RESERVE` per data-URL image. The llm base64-object branch is dead, because history is OpenAI format. No drift bug.
- session.rs:57 and snapshot.rs:35 flatten paths the same way. The collision itself is H's lead 22.
- Committed conversation entries (`Entry::render`) expand tabs and strip control characters correctly. Only the live area, the editor and raw inline `commit()` lines miss that.
- Casts: float-to-int `as` saturates (banner, palette). The u64→usize casts are 64-bit only. `as u16` cursor moves are bounded by the terminal height. `resize` and `App::new` clamp height to at least 5 and width to at least 20.
- The `menu::fuzzy_match` ranking and `hint_row` drop order behave as documented. `fit_recap` cuts correctly, except for the abbreviation false positives already noted against `first_sentence`.
