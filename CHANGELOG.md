# Changelog

Notable changes to killi. `install.sh` and `killi update` always install the latest release
from [`releases/`](releases); the versions below match `Cargo.toml`.

## Unreleased

## 0.6.12 — 2026-10-28

- Fixed a flaky session-lock test: after a session ends, the lock's release is
  awaited instead of assumed instant, since another test forking at that
  moment can pin the lock past the drop.

## 0.6.11 — 2026-10-28

- While a turn's opening snapshot runs, the wave's word no longer cycles through
  Chinese words; it types out one warm line (给这一刻留个影...) a character at a
  time, 0.4s apart, dots last.

## 0.6.10 — 2026-10-28

- A `/` command or skill typed in full sends on the first Enter, also while the
  agent works: the completion menu no longer stays open over a complete name
  (like the `@` menu over a fully typed path) and swallow the key.

## 0.6.9 — 2026-10-27

- Scripts only: `sessions.py` reports cache-miss streaks (3+ uncached requests
  on a growing prompt), a per-version table, a `[vX]` tag on every finding, no
  longer flags read-only `git stash list|show`, and counts refusals from the
  peer guard; `probe.py cache` retries a 429 and can end "inconclusive".

## 0.6.8 — 2026-10-27

- While another killi session works in the same git work tree, tree-moving git
  commands (`checkout`, `switch`, `restore`, `reset`, `clean`, and `stash` other
  than `stash list`/`show`) are refused instead of run: use a separate git
  worktree or run the command yourself. Subagents are guarded too; `[agent]
  peers = false` turns the guard off.
- A usage record whose reasoning was billed but not kept now carries
  `reasoning_seen`: the field the stream used for reasoning (present but empty)
  and any delta fields the provider sent that killi doesn't read. The one-time
  warning says which case it was, and `scripts/sessions.py` splits lost turns
  into the three cases per model.
- While the agent works, the slash menu offers skills again: picking one and
  sending it queues the skill's expanded prompt for the agent's next step, with
  `/name args` shown in the queue like a typed message.

## 0.6.7 — 2026-10-27

- Fullscreen mode selects its own text now: a drag across the conversation
  highlights it in reverse video and letting go copies it, across scrolled
  pages and while the agent works. A drag held at the edge scrolls on its
  own. Needs a terminal that reports drags; Kil's half (forwarding them,
  Shift+drag staying native) follows in Kil 0.0.19.

## 0.6.6 — 2026-10-27

- The `osascript` notification fallback is plain again: wrapping it in
  `tell application id ...` never transferred ownership (`display notification`
  is a Standard Addition, so the banner belonged to Script Editor either way —
  verified). A session started before this update still runs the old code in
  memory, so restart it to get Kil's own notifications.

## 0.6.5 — 2026-10-27

- While a question waits, ← walks back over this turn's answered questions (and →
  forward again to the waiting one), showing each question with the answer given.
  Changing an earlier answer records a correction that rides with the waiting
  question's answer, so the model hears both; Esc on a revisited question just
  returns. Only model questions take part, not plan approval or config flows.
- The working rows' elapsed time and `Esc to interrupt` now start at the left edge,
  aligned with the braille wave instead of indented under the action word. While a
  turn's opening snapshot runs, the wave's word cycles through Chinese words
  (结印、封存、留影、藏锋、掠影、蓄势, 0.8s each) instead of the internal `snapshot…`.

## 0.6.4 — 2026-10-27

- Notifications in Kil are always Kil's own, never Script Editor's. killi no longer checks
  Kil's version before sending the terminal notification: a Kil pane outlives the app, and its
  shell keeps the `TERM_PROGRAM_VERSION` it started with, so a pane from before Kil 0.0.11
  sent every notification through `osascript` (whose notifications open Script Editor when
  clicked) even on a current Kil.

## 0.6.3 — 2026-10-27

- The working rows name the turn's action instead of the `killi` wordmark: the braille wave
  is unchanged, then `thinking`, `writing`, `working` and the like fade in letter by letter,
  with what the turn is doing after the word in plain text. The elapsed time and
  `Esc to interrupt` sit on their own row under the action word. The separate
  `∴ thinking` row is gone; its first sentence moved onto the action row.

## 0.6.2 — 2026-10-27

- The task list no longer stays at `0/8` through a long turn: when 10 tool calls go by with
  an item open and no `todo_write`, the model is reminded mid-turn which item still shows as
  current, not only when the turn ends. A reply ending in a full-width `？` now counts as a
  question too, so a Chinese reply that asks the user something isn't re-prompted about its
  open items.

## 0.6.1 — 2026-10-27

- `ask_user_question` options wrap onto as many rows as they need instead of being cut
  with an ellipsis when the window is too narrow to show them in one line.

## 0.6.0 — 2026-10-27

- A structure-only release: every source file is back under about 500 lines (the TUI had
  grown to 5,727) by splitting the big modules along their seams and moving each module's
  tests to its own `<module>/tests.rs`. Pure moves behind unchanged paths — no behavior
  changed and the test suite is identical.

## 0.5.2 — 2026-10-26

- `/backup-model <alias>` sets this session's fallback: when the primary model exhausts its
  retries on a 429 rate limit (not a spent quota, which resets on its own), the session switches
  to the backup and carries the conversation on. The pick lives in the session's log — a resumed
  session keeps it, a new one starts without it. `/backup-model none` clears it; the status line
  shows `backup <alias>` while one is set.

## 0.5.1 — 2026-10-27

- Interrupting a swarm no longer throws away the subagents that already finished: their
  reports are recorded as the swarm's result, marked per item, so the next prompt only
  re-runs the items that never finished instead of re-spending the whole swarm's tokens.
- Up and Down arrows in the prompt now move between the rows you see, including a long
  prompt that wraps across them, instead of jumping to history recall; history still
  comes back from the first row and the draft returns from the last.

## 0.5.0 — 2026-10-27

- Error hints now follow the error's kind and status instead of digits inside its text: a
  "prompt is too long: 214013 tokens" overflow no longer suggests checking your API key,
  and a Bedrock-style throttle no longer triggers a needless compaction. Transport errors
  keep their underlying cause, and unfixable ones (bad URL, TLS failure) fail at once
  instead of retrying for minutes. A response that isn't an event stream fails on the first
  attempt and shows its body.
- A rejected image no longer wedges the session. `read`, paste and drop check the file's
  magic bytes against its type (a Git LFS pointer now says so instead of failing later);
  switching to a text-only model replaces images with a short placeholder; and if a
  provider still rejects one, killi strips the images once, records the rewrite in the log,
  and retries the turn.
- Tool results the tools already bounded now reach the model whole instead of being cut to
  about 5 KB, so `read`'s "continue at offset N" hint points at the right place again.
  `grep` names the files it skipped for being over 10 MB instead of saying "No matches",
  and `edit` refuses a file that isn't valid UTF-8 instead of rewriting bytes it can't show.
- Compaction can't storm anymore: after a compaction it waits for the context to grow by a
  fifth of the limit before compacting again, a fresh subagent inherits its parent's
  estimate instead of compacting on step one, and `/model` puts the whole existing
  conversation back into the estimate.
- A terminal closed during startup no longer leaves killi spinning at 100% CPU ignoring
  signals; SIGHUP/SIGTERM are handled on their own thread and the startup read checks the
  terminal is still there first.
- Untrusted text (model replies, tool output, todos, questions, peer messages) can no longer
  drive the terminal: escape sequences are stripped and tabs expanded before styling. Tabs
  in the prompt lay out correctly, multi-line questions wrap instead of staircasing, and
  `/name` followed by a non-breaking or ideographic space no longer panics.
- A retry ends the partial reply cleanly: the discarded fragment is committed as its own
  block in the conversation and in `-p` output, and notices print after it instead of being
  glued onto its last line. An unclosed code fence no longer inverts code/prose styling for
  the rest of the session.
- A session owns its log: opening a log another live session is writing fails with "session
  is in use" instead of interleaving two conversations into an unresumable file. `-c`
  picks the newest log that isn't locked, the `/resume` picker marks running sessions and
  refuses them, and two sessions started in the same second no longer collide. A resumed
  session can start subagents again.
- In-place config edits (the key guide, `/web-search-config`, `/model`'s save) no longer eat
  the next provider's key: headers with comments or spaces are matched, `[x]` lines inside
  the `[prompt] system` block are left alone, every edit is re-parsed before writing, and
  writes are atomic (temp file, sync, rename) so concurrent sessions can't tear them.
- `/undo` stays available after a failed snapshot (the turn gets a notice instead of disarming
  undo), a failed undo keeps its snapshot so a retry undoes one turn, restores only touch
  changed files, and sessions no longer share one git index.
- A message typed during a turn that ends in an error is no longer silently carried into the
  next conversation: it comes back into the prompt with "Your queued message wasn't sent;
  it's back in the prompt." And `/undo` now tells the model what changed (the prompt it
  restores and the files, capped at 30) instead of "The user interrupted your previous
  turn", and never reaches across `/new` or `/resume`.
- Plan mode now holds on the harness side: MCP tool calls are blocked (listing stays allowed),
  writers hiding behind read-only names (`sort -o`, `git branch -D`, `cargo clippy --fix`,
  …) are refused, a subagent started in plan mode hears it first, and a resumed log keeps
  the mode it ended in.
- Hooks run around `agent` and `agent_swarm` calls too; a hook that blocks one says which
  call it stopped. The interrupted, MCP and instruction notes are only taken after the
  prompt-submit hook passes, and a hook that times out stays fail-open with one notice.
- Small truth-telling fixes: length continuations keep the whole reply (subagent reports and
  `--json` no longer lose their beginning); the open-todos nudge stays quiet when the reply
  ends with a question; recovery marks an interrupted tool result; nested `AGENTS.md`
  discovery follows bash's `cwd`; compaction runs before steered messages are pushed; and the
  compaction summary says a `/name …` request ran a skill whose text isn't kept.
- Tool edges: numbers sent as strings work (`read` offset/limit, `bash` timeout,
  `task_output` wait); `mcp` arguments given as a JSON string are parsed; long `fetch_url`
  text spills to a file; `summary` only strips the working directory at a `/` boundary;
  MCP images that can't attach show as `[image omitted: …]`, never raw base64; `later`
  results are numbered and `done` names the item it removed.
- `/new` and `/resume` share one reset (queue, todos, totals, peer notes, snapshots, meter),
  so stale todos and totals no longer leak across conversations. `/compact` updates the
  context meter and refuses an empty session like `/recap` (as does `/btw`); a `/swarm off`
  typed mid-turn sticks; no desktop notification follows a turn ended with Esc.
- `/web-search-config` survives `/new`, `/reload` and `/resume`, and `/reload` says which
  settings apply at the next start instead. First-run guides can be cancelled: Ctrl+C works
  and an EOF answer counts as "no" without writing anything.
- Every error display goes through `gist`, non-JSON error pages show their title or first
  line, and hints come from the error's kind. The live area sheds the task list before the
  running call; "Shift+Enter" is only advertised when the kitty protocol is active
  (Ctrl+J otherwise); at 60 columns the status line keeps the context figure; Ctrl+L
  resets the inline scroll; `/effort` mid-turn offers the menu levels; a dismissed question
  is no longer a red error; wrapped code keeps its gutter; Ctrl+S keeps typed `@file`
  text; an apostrophe before `@file` no longer eats the attachment; secret answers stay out
  of history.
- `/prune` skips sessions that are still running. A log torn mid-line gets its newline on
  open, bad-UTF-8 lines are skipped per line with a notice of how many, and a tool call left
  without a result gets a placeholder on load. Session logs are 0600 in 0700 folders, as are
  snapshot repos.
- A message from another session can no longer break out of its "not from the user" frame,
  and `@file` contents or tool output containing `</file>` or `<system-reminder>` read as
  text instead of borrowing killi's authority. The MCP broker only takes orders from its
  own user, over a socket in a 0700 folder. OpenCode Go drops models whose URLs leave
  `https://opencode.ai/inference/go/`, provider keys no longer appear in debug output, and
  a project that wants commands started at launch asks once per repository (remembered by
  repo path plus a hash of the commands) instead of just running them.
- The snapshot before a prompt no longer freezes the keys and spinner: it runs on a
  background thread ("snapshot…" in the live area) while the first request is already
  going, and the first tool call waits for it. The branch name refreshes at most every few
  seconds off the main thread, the `@` menu reuses its filter per query, and resuming or
  continuing parses the chosen log once instead of up to four times.
- Kills leave nothing behind: every child process group (shell commands, hooks, MCP
  servers, the browser, `pdftotext`) is covered by one registry, and a panic writes a
  `crash` line to the session log before the terminal is restored. Updates time out, cap
  at 64 MB, and prove their version before replacing the binary. The lock file is refreshed
  (no more yanked `tokio`), releases refuse a dirty tree or a missing changelog entry, and
  the test suite no longer touches the network, leaves no folders behind, and passes three
  runs in a row.
- Whitespace-only assistant text is no longer sent to Anthropic (it rejects it), a
  Responses refusal ends the turn instead of an empty reply, and a server named twice in
  eager MCP starts once.

## 0.4.20 — 2026-10-26

- A running swarm's panel has a blank row above it, so the lines its finished subagents left
  in the conversation no longer run into the panel's own rows.

## 0.4.19 — 2026-10-26

- A recap is no longer cut mid-sentence with `…`: it may run to 160 characters (the model is
  still asked for 100), and a longer one keeps the whole sentences that fit.

## 0.4.18 — 2026-10-26

- A resumed session shows a skill run as the `/name request` that started it, not the skill's
  whole text: in the replay, the banner, `/resume`'s note, the session list and the prompt
  history. The files a prompt named with `@` are left out the same way. A compaction keeps that
  short form of each request too, so a long skill no longer cuts the user's request out of the
  summary.

## 0.4.17 — 2026-10-26

- **Sessions know about each other.** Every killi on the machine is listed in `~/.killi/live`
  (a file it holds locked, so one that crashed drops out). The banner names the sessions working
  in the same repository, `/sessions` lists them all, and the agent is told once when one starts
  or stops in its repository: to leave changes it didn't make alone and to switch branches in a
  worktree. `killi sessions [<pid>]` shows what each is doing; `killi ask <pid> "…"` has
  another session's model answer from its conversation, the way `/btw` does, without keeping
  it; `killi tell <pid> "…"` leaves it a message, marked as not from the user, for its next
  step or its next prompt. No new tool and no change to the system prompt, so the cache holds.
  `[agent] peers = false` opts out. About 130 KB of binary.
- `/undo` says when another session in the repository worked since the snapshot, since undo
  would put its files back too; `/undo` again goes ahead. A snapshot that meets another
  session's `index.lock` waits for it instead of turning undo off for the session.
- After a compaction the agent is told again that plan mode or swarm mode is on. Before, the
  compaction summarized the reminder away and nothing said it again.

## 0.4.16 — 2026-10-26

- **`killi login` asks which subscription to sign in to**, or takes its name: `killi login
  chatgpt` or `killi login opencode-go`. `killi logout` lists the ones killi is signed in to.
- **OpenCode Go.** `killi login opencode-go` takes the API key from the OpenCode Console (or
  `$OPENCODE_API_KEY`) and adds Go's models to the config, each on the API it answers on: Chat
  Completions, Responses or Anthropic Messages, with the limits and inputs models.dev lists.
  Models on Gemini's API are left out. A model can now set its own `base_url`, and a provider's
  `session_header` sends the session's ID on each request (`x-opencode-session` for Go, which
  routes and caches by it). Every request names killi in its user agent.

## 0.4.15 — 2026-10-26

- **Use your ChatGPT plan instead of an API key.** `killi login` signs in with ChatGPT in the
  browser, through OpenAI's Sign in with ChatGPT for open-source tools: killi registers as your
  own client, keeps the credentials in `~/.killi/chatgpt.json` (owner-only), and adds a
  `type = "chatgpt"` provider with your plan's models to the config. Requests go to
  `api.openai.com/v1/responses` on the plan; the hourly token is refreshed five minutes early
  and after a 401, under a lock so several killi processes never spend the same rotating
  refresh token. A spent plan says so with a link to ChatGPT's usage settings, and `killi
  logout` ends the session. Starting on a ChatGPT model without a sign-in offers one. About
  82 KB of binary and 0.1 MB of peak memory.

## 0.4.14 — 2026-10-26

- **Claude behind an OpenAI-compatible gateway can use its prompt cache.** Bedrock caches
  only what a request marks, so such a model was billed for its whole prompt on every turn
  (0% cached over 71 turns in the logs). `cache_control = true` on a `[models]` entry marks the
  system message and the newest message of each Chat Completions request; the stored history
  is left as it is. Measured through the SPH gateway: a second request read 8,898 of 8,909
  tokens from the cache at a tenth of the cost.

## 0.4.13 — 2026-10-25

- **A swarm is one live panel, not a scroll of subagent calls.** While `agent_swarm` runs, the
  live area shows a header with a progress meter and the counts (`3/40 done · 8 running · 29
  queued`), then a row per subagent in item order: its item, how long it has run, its calls so far
  and what it is doing now. A swarm too big to list shows a strip of one mark per subagent
  (`✓ ◐ ○ ✗`) and rows for the ones working. The subagents' calls no longer print a line each;
  a subagent that ends leaves one (`↳ ✓ pr-1581 · 3m02s · 12 calls`).
- **Swarm mode is on by default.** The agent is told to split work across `agent_swarm`
  subagents from the first prompt of every session, when the task splits into independent
  parts; a small or single-part task is done directly (the reminder used to tell it to say why
  it needed no subagents and wait). `[agent] swarm = false` starts sessions with it off;
  `/swarm on|off` still switches it.
- **The desktop notification says what happened.** A finished turn no longer posts a bare
  "Done": the headline carries the outcome and how long the turn took, and under it the
  first line of the reply (or the reason it stopped). A question waiting for an answer
  names the question. In Kil, which shows the notification itself, Kil adds the tab and
  folder; clicking it goes to the pane. (Kil before 0.0.18 could report an old version
  from its long-lived session server, sending killi down the `osascript` path whose
  notifications opened Script Editor; Kil 0.0.18 fixed the server's version stamp.)

## 0.4.12 — 2026-10-25

- **Whether billed reasoning was kept is recorded with each request's usage.** One
  dialect-agnostic verdict — reasoning text or structured items, whichever the provider
  sent — now rides on the `usage` log entry itself, so a provider that starts hiding
  reasoning on one API shows up as a falling kept-rate in `scripts/sessions.py` instead of
  having to be reconstructed from dialect field names.

## 0.4.11 — 2026-10-25

- **A failed edit points at the closest line.** `edit` failed 8% of the time in the newest
  sessions, mostly on a stale `old_string` in a long multi-file session. When nothing matches,
  the error now names the line nearest the first line of what was looked for, so the model's
  next step is a targeted read instead of a blind one; an unrelated file gets no hint.
- **The session log says which binary and model produced each number, and why a compaction
  fired.** The `session` entry records the killi version and the model's context window,
  each `usage` the model it ran on, each tool result the tool's name, and each real
  compaction its cause: billed context, estimated suffix, limit, reserve and the summary's
  size, taken before the summary request overwrites them. `scripts/sessions.py` reads all of
  it and flags a session that compacts three or more times.

## 0.4.10 — 2026-10-25

- **Drag a file into the prompt.** A file dropped on the terminal arrives as a paste of its
  path; killi now reads it as one. An image (png, jpg, gif, webp) is attached as `[Image #N]`,
  the way Ctrl+V attaches the clipboard's, and any other file is named as `@path`, so its
  contents go to the model with the message. A path with spaces works for `@` now too, written
  `@my\ notes.txt` as a shell would, by the drop and by the `@` menu. Terminal.app and iTerm2
  deliver drops this way today; Kil doesn't forward a drop to its pty yet.

## 0.4.9 — 2026-10-25

- **Compaction scales with the model's context window, and a summary no longer carries a pasted
  skill forever.** The limit was a flat 256000 tokens for every model — after the output
  reserve, a 1M-window conversation compacted at 128k, an eighth of what it paid for. It now
  aims at 60% of the window by default (`[agent] compact_at` still overrides, and 85% of the
  window stays the ceiling), and the reserve for one reply is capped at a quarter of the
  limit instead of the model's whole output cap, which the per-request output cap already
  fits into the window. A scholar-daily session that compacted 45 times showed the other
  floor: prompt 1 embedded a whole skill, and the verbatim `<user-requests>` section carried
  its ~4.5k tokens into every summary, keeping the fresh context near the trigger. Each kept
  prompt is now cut at 4000 characters, and the summary instruction asks for under ~1500
  tokens with no command transcripts.

## 0.4.8 — 2026-10-24

- **Reading a screenshot no longer compacts the conversation.** The compaction estimate
  counted an inline image's base64 as text: a 300 KB screenshot came to 200k estimated
  tokens for about 1.5k billed, so an 8k-token conversation was compacted after every
  `read` of an image, and with MiniMax the summary came back empty and the turn ended in
  `the model returned an empty summary`. An image now counts as a fixed 2k-token reserve,
  as the output cap already did. A proactive compaction that fails is a notice and the turn
  goes on, since the request may still fit and an overflow compacts the reduced history on
  the way back; an empty summary says what the model did instead (called tools, or only
  reasoned) and leaves a `warn` record in the session log.

## 0.4.7 — 2026-10-24

- **`web_search` falls back when its service fails, and `fetch_url` says what went wrong.**
  A transient failure (429, 5xx, network) is retried once, then `web_search` tries any
  other source the `[search]` table already makes usable — a `url` instance beside a
  keyed provider, Brave's public page last — and names every failure when none answer,
  instead of leaving the model to guess addresses. `fetch_url` expands the error's cause
  chain: an unresolvable domain says the host is likely invented and not to retry spelling
  variants, an error page is named by its `<title>` instead of raw HTML, a short page
  (home, index) appends its own links so the next address comes from the page, and a PDF
  is read as text through `pdftotext` (poppler) when installed, with a clear error when
  it is not or the PDF has no extractable text.

## 0.4.6 — 2026-10-24

- **`web_search` can ask a SearXNG instance, and `/web-search-config` sets the service up.**
  `[search]` gains `"searxng"` as a provider and a `url`: killi asks
  `{url}/search?q=…&format=json`, with an optional `api_key` sent as a bearer token, so the
  instance must enable the json format. `/web-search-config` lists the services — brave, an
  API key or its public results page; tavily, an API key; searxng, self-hosted, with the
  docker command and the settings.yml change printed — then asks for the URL (prefilled with
  `http://localhost:8888`) and the key, masked in the transcript. The pick is written into
  `~/.killi/config.toml` in place and applies to the running session without starting the
  prompt cache over. `/web-search-config <service>` skips the list.

## 0.4.5 — 2026-10-24

- **`@` names a file.** Type `@` at the start of a word and a menu offers the workspace's
  files and folders, fuzzy-ranked like the slash menu (`@tuimod` finds `src/tui/mod.rs`);
  Tab or Enter inserts `@path`, a folder as `@folder/` to keep typing in it. The listing
  honors `.gitignore` and is read afresh each time an `@` is typed. When the message is sent
  — idle, steering, or as a skill's argument — the files it names follow the text in one
  system reminder, so the model has their contents without a read. A binary file is named
  only; a long one is cut at the `read` tool's 50 KB.
- **The resume command stands on its own line** after killi quits, so it copies alone.

## 0.4.4 — 2026-10-24

- **A first run can start from omp too.** Without a config, the import offers now include
  omp beside Kimi Code, pi and opencode: its `~/.omp/agent/models.yml` providers and models
  — pi's shape spelled as YAML, read by a small block-style reader of killi's own, so no YAML
  library ships — the API keys behind them (a key naming a set environment variable stays
  one; a `!command` key is skipped), its `modelRoles.default` as the default model with its
  thinking level, and its MCP servers. Only the providers that file defines come over: the
  ones omp builds in carry no endpoint or key there. Custom request headers come over from
  pi's config too now.

## 0.4.3 — 2026-10-24

- **The working row counts a retry wait down.** When a request fails and killi waits to try
  it again — up to about 40 s of backoff, or longer when the provider says so — the row said
  `working 45s` while nothing was happening, and the `⚠ retry 2 in 8s` line above it stood
  still. It now reads `retrying in 8s · attempt 2` with the seconds ticking, and returns to
  the turn's elapsed time once the next request is in flight.

## 0.4.2 — 2026-10-24

- **A command can no longer take the terminal from killi.** Every process killi starts — bash
  commands, hooks, MCP servers, the headless browser — now runs in a session of its own, not
  only a process group. An interactive shell such as `zsh -i` opens `/dev/tty` and could make
  itself the terminal's foreground group; killi's next keystroke then stopped it with SIGTTIN,
  and the user's shell left the prompt in raw mode with the mouse still reporting, so scrolling
  typed `65;48;33M` into it. A new session has no controlling terminal, so a child cannot grab
  it, and killi stays in the foreground. Each child is still ended with everything it started.
- **A suspended killi gives the terminal back.** Should killi be stopped anyway — now only by an
  explicit `kill -TSTP`, since Ctrl+Z is a key in raw mode — it restores the terminal first
  (cooked mode, mouse reports and bracketed paste off, the live area erased), then takes it
  again and repaints when continued, so a stopped session never leaves the shell in raw mode.

- **`/update-config`, a built-in skill about killi's own configuration.** It lists every
  file under `~/.killi` and in the project, every key of `config.toml`, `mcp.json` and the
  hooks with its default, and what applies on `/reload` against the next start, so the agent
  can show the setup or change it. It describes the version running: a test fails when a
  config key is added without being described. A skill folder of the same name still wins.

## 0.4.0 — 2026-10-24

- **Streams cannot end in the middle and pass as replies.** Malformed SSE data, an
  unconsumed stream tail or a missing protocol terminator now retries instead of recording a
  partial answer or acting on cut-off tool arguments. Replies stopped at the output limit are
  returned for continuation, while filters and refusals are no longer repeated unchanged.
- **Structured reasoning survives a tool call.** OpenAI-compatible `reasoning_details` are
  kept by item and replayed to the endpoint that produced them, including Kimi's stream-only
  indexes being left out on replay; Anthropic-compatible sessions still receive the matching
  scalar reasoning when they cannot use those structured items.
- **Output fits the context that is left.** Each wire format caps its output by the estimated
  remaining window (with images reserved as images, not as base64 text), older Claude thinking
  budgets stay below `max_tokens`, and compaction counts the messages added since the last
  billed request. A conversation that already overflowed is summarized from a reduced copy
  without editing signed reasoning.
- **Reads run together.** Consecutive `read`, `grep` and `glob` calls in one reply run in
  parallel and answer in call order; hooks and every other tool keep the old serial order.
  A repeated call whose result keeps changing warns instead of ending the turn, and a turn's
  subagents share one swarm budget.
- **Search says when it could not search.** Ordinary grep reads line by line, traversal and
  read failures are reported instead of becoming "no matches", and a failed command keeps the
  start of its output with the tail, in the same 50KB budget, so the first error is not buried.
- **Eval reports count every repeat.** `eval.py` keeps all runs of each tool's latest version
  and model, excludes records with failed metrics from performance means instead of counting
  them as zeroes, and shows `N/A` when there is nothing valid to average.

## 0.3.1 — 2026-10-24

- **A queued message leaves the queue once it is sent.** A message typed while the agent
  works went into the conversation at its next step, often within seconds now that a message
  ends a wait, but it kept saying "Ctrl+S sends it now" until the turn ended, and Ctrl+S then
  did nothing because nothing was waiting. Once the agent takes it, it now prints as your
  message, and the queue holds only what Ctrl+S can still send.
- **Ctrl+C clears a draft first.** While the agent works, or asks you something, Ctrl+C
  interrupted it even with a half-typed message in the prompt. It now clears the prompt, as
  when idle; on an empty prompt it interrupts.

## 0.3.0 — 2026-10-23

- **Finished calls are quiet.** Every tool row began with a lagoon tick, which marked nothing.
  A finished call's `✓` is now faint, so what is running or has failed is what the eye finds;
  the `$` of a bash row is faint too, and a path keeps its folders muted so the file stands
  out. Lagoon is left for work in progress, modes, commands and the selected row, and the
  prompt's caret is coral, like the messages it becomes.
- **Diffs on stripes, code on chips.** In 24-bit color an added or removed line sits on a
  soft green or red stripe to the window's edge, its sign colored and its text left alone;
  inline code sits on a chip instead of being teal. 256 colors keep the colored text. A code
  block no longer ends in an empty gutter row.
- **Colors follow your terminal.** killi asks the terminal for its background and foreground
  and derives its grays, tints and stripes from them, so they belong to any theme: before, the
  band behind your messages was a blue-green slab on Kil's Ember and Dusk, and on a light theme
  Kil's dim made secondary text darker than body text. With no answer, or `[ui] theme` set, the
  fixed shades stay.
- **One palette.** The banner's gradient now starts at lagoon, and the status line's three
  hues (effort, tokens, branch) share one lightness and chroma instead of being the most
  saturated thing on screen. The status line keeps the model and mode at the left and the
  figures at the right edge, with the model muted.
- **A cut always says so.** Menu descriptions, the banner's lines and the hints under a list
  were cut at the window's edge without a mark (`effort hig`, `Esc cl`); they end in `…` now.
  Hints drop their least useful items before cutting one, the banner's hints wrap between
  items, and a blank row sets a list's hints off from the status line.
- **A compact banner.** In a window narrower than 70 columns or shorter than 30 rows the
  banner is the gradient `killi` and the setup lines: 4 rows instead of 10 at 80x24.
- **Fewer rows that repeat.** A task-list update prints no row unless it failed, since the
  list is on screen; `Ctrl+O` is named once per session, by the first row that leaves output
  out; the turn's trailer reads `18.8k in · 78% cached · 922 out`, like the status line.
- **`scripts/frames.py`** draws killi's screens at any size and on any of Kil's themes from a
  scripted provider, with no model and none of your config.

## 0.2.41 — 2026-10-23

- **The prompt wraps at spaces.** A long message broke in the middle of a word; now rows
  break between words, and only a word longer than a row is split.
- **`/help` wraps under its column.** A long description went back to the left edge; it now
  hangs under the description column.
- **Singular counts.** "1 prompt", "1 request", "1 tool call", "1 line", "1 file" instead of
  "1 prompts" and "file(s)".
- **A long folder in the banner is cut at a folder boundary** (`…/code/killi`), not
  mid-name.

## 0.2.40 — 2026-10-23

- **A message ends a wait.** A message typed while `bash` or `task_output` with `wait` was
  waiting only reached the model when the call ended, up to five minutes later, which read as
  a hang (a CI wait held back three "is it stuck?" messages in one session). Now the wait
  stops when the message is queued: `bash` moves its command to a background task, as it
  does at its timeout, and `task_output` returns what it has; the model reads the message on
  its next request and can wait again. Subagents are not cut short.
- **`/effort` works while the agent works.** `/effort low` typed mid-turn is checked against
  the model's efforts at once and applied when the turn ends, before the next prompt; the
  slash menu offers it, and its choices, meanwhile.

## 0.2.39 — 2026-10-23

- **A message typed mid-turn comes before the next command, not after the batch.** It
  already waited only for the tool call running, in the docs; in the code it waited for every
  call in the reply. Now the calls after the running one are answered as not run, so the
  message reaches the model as soon as that call ends and the model runs them again if they're
  still needed. Ctrl+S still cuts the running call itself.

## 0.2.38 — 2026-10-23

- **A first run that starts from the agent you already use.** Without a config, `killi`
  offers to import from the coding agents it finds on the machine — Kimi Code, pi and
  opencode — carrying over their providers, models, API keys, default model, thinking level
  and MCP servers into killi's layout; providers those tools only reach through an account
  login are skipped. omp's config is YAML, which killi doesn't read, so it isn't offered yet.
- **A first run that ends on a session.** When the model killi would start on has no
  key — `$OPENAI_API_KEY` isn't set, say — it offers to paste the key, stores it under
  `[providers.*]` in `~/.killi/config.toml` with echo off, or to start on another configured
  model, rewriting `default_model`. Declining, a `-p` run or a piped start still ends on one
  line naming the variable to export or the line to set, and the first-launch command
  question is only asked once a model can start.

## 0.2.37 — 2026-10-23

- **A recap after each turn, and `/recap`.** A turn that completed and used tools is followed
  by a recap of the session in 100 characters or fewer, in your language, printed under the
  turn's totals; `/recap` asks for one at any time and `[ui] recap = false` turns the automatic
  one off. It is asked like `/btw`: neither the request nor the reply enters the conversation or
  the log, the cached prefix still hits, sending anything else cancels it, and the limit is cut
  in code when the model writes more. Details in `docs/decisions.md`, item 36.
- **Waiting on a task is no longer a repeat.** The repeat breaker counted a `task_output` wait
  as the same call each time, so the third wait on a long CI job was told "try a different
  approach" and the eighth ended the turn. A wait now repeats only when its result does.
- **`ask_user_question` takes labelled options.** An option given as `{label, description}`
  shows as "label — description" instead of being refused.
- **`scripts/sessions.py` paired reasoning with the wrong request.** A request's `usage` is
  logged just before its assistant message; the script matched each message to the next
  request's usage, so it reported about a third of the reasoning as lost (Muse: 417 of 634
  kept; really 632 of 660). It also no longer lists `task_output` waits as repeated calls.

## 0.2.36 — 2026-10-23

- **First-launch command guide.** The first interactive start asks for another command to
  start killi with (such as `dw`), creates it as a symlink beside the binary, and remembers
  the choice; pressing Enter keeps `killi`. Existing commands are never overwritten.
- **Queued messages name their shortcut.** A message queued while the agent works now
  reads "(sent at the next step · Ctrl+S sends it now)", so the way to send it at once is
  there on the row instead of only in the help.

## 0.2.35 — 2026-10-23

- **Memory and waits, from an audit.** The session log is written a line at a time, not a
  fragment at a time (200 tool results: 3.7 s to 41 ms). `read` refuses a text file over
  10 MB and `fetch_url` stops at 10 MB and checks the type before downloading, where each took
  hundreds of MB. `bash` keeps the tail of a huge output in 22 MB, not 166 MB, and finished
  background tasks give their output back. `cmd &` without a redirect returns at once instead
  of at the timeout. A hook's timeout covers its input and ends what it started, an
  interrupted page render removes its browser profile, and `edit` with `replace_all` over
  100,000 matches takes 68 ms, not 32 s. A conversation too long to summarize is cut down and
  summarized instead of failing every turn; the repeat breaker starts over with each prompt.
  Details in `docs/decisions.md`, item 35.

## 0.2.34 — 2026-10-23

- **The size and memory chart compares opencode 2.** killi is 6.4 MB and runs a prompt in
  15.3 MB; opencode 2.1.3 is 204.4 MB and 332 MB, and sends 15,014 tokens before you type to
  killi's 3,564. killi's own row is re-measured on 0.2.33. `scripts/bench.py` takes
  `--tools killi,opencode` to measure some tools and keep the others' rows. The binary itself
  is unchanged apart from its version.

## 0.2.33 — 2026-10-23

- **Bash rows start with `$`, not the word `bash`.** Most tool rows are shell commands, so the
  same word opened nearly every line. They now read `✓ $ cargo test · 12 passed`, with a dim
  `$`; the other tools keep their bold name, so they stand out among the shell rows.

## 0.2.32 — 2026-10-23

- **Ctrl+S sends your message now.** While the agent works, Enter queues a message for its
  next step; Ctrl+S ends the step like Esc, answers the tool calls it cut, and starts the next
  turn at once with the queued messages and whatever is typed, as one prompt. With nothing
  queued or typed it does nothing.

## 0.2.31 — 2026-10-23

- **`killi update` shows what it is doing.** On a terminal it draws a live row while it checks
  for a release and downloads it: the working row's braille wave and wordmark, then the version
  being fetched with a gradient bar, the size so far and the speed, dropping the speed and then
  the sizes on a narrow terminal. It ends with a check mark, and Ctrl-C clears the row and
  restores the cursor. Piped output and `NO_COLOR` get the same plain lines as before.

## 0.2.30 — 2026-10-23

- **A new working animation.** The `killi` wordmark alone was too quiet to notice. The working
  row now starts with a braille ribbon that rises from a flat line into a wave rolling to the
  right a dot column per tick, then the wordmark fading in, the logo's gradient flowing through
  both. It replaces the glint that crossed the wordmark.

## 0.2.29 — 2026-10-23

- **The working row has room, and a livelier wordmark.** A blank row now separates the
  `killi working` row from the tool output or prose above it (running tool calls still follow
  their finished ones directly). The wordmark shows the logo's whole sky-to-violet gradient,
  not a slice of it, flowing through the letters; a soft glint with a long tail crosses every
  few seconds and the word rests in between, with no bold flicker.

## 0.2.28 — 2026-10-23

- **The working row has some presence.** Elapsed time is bold, `Esc` follows the key-hint
  convention in lagoon, and the separator is faint — the whole row in dim looked stingy.
- **The live area no longer touches the scrollback.** Streaming preview text is separated
  from the conversation by a blank row, the way the other live-area sections already are.

## 0.2.27 — 2026-10-23

- **A skill sees the image pasted beside it.** `/lean-build make it nicer [Image #1]` dropped
  the image: the command carried only its text, so the model got the literal `[Image #1]` and
  said it couldn't see it. A skill or `/swarm` now sends the image the way a plain prompt does.

## 0.2.26 — 2026-10-23

- **Waiting on a job ends when the job ends.** `task_output` with `wait` now waits for what a
  command left running too, not only its shell: after `nohup job &` it used to return at once,
  so the agent fell back on `sleep 270; tail log` and slept the full time, even on a job that
  had failed in a second (one session lost 3,045 s that way). A foreground command that opens
  with `sleep` of 30 s or more is not run; the error points to `task_output` with `wait`, or an
  `until <check>; do sleep 5; done` loop. `run_in_background` is not affected.
- **`/btw` says what the agent is waiting on.** While a `bash` or `task_output` call is in
  flight, the side question is told how long it has run, how long since its last output and its
  last five lines, so "is it stuck?" gets an answer from facts. Before, it only knew "Still
  running".
- `scripts/sessions.py` flags sessions that spent 120 s or more in commands that open with a
  fixed `sleep`.

## 0.2.25 — 2026-10-23

- **Fullscreen mode** (`[ui] fullscreen = true`; on by default in Kil, off elsewhere). killi
  takes the alternate screen: the prompt and status line stay on the bottom rows and only
  the conversation scrolls above them, with the wheel, PageUp and PageDown (typing returns to
  the newest row). The conversation is laid out again whenever the pane or tab changes
  width, so tool lines, prose, your messages, tables and the banner follow it; before, only
  the prompt did and the rest kept the old width. Quitting prints the whole conversation to
  the terminal again; a signal or a panic restores the terminal but can't print it (the
  session log has it). Outside Kil, mouse reports mean selecting text takes Shift (Option in
  some terminals).
- **The prompt and status bar stay on the bottom row** also in the default mode while the
  conversation is shorter than the screen, with the banner above and blank space between;
  opening the slash menu or a question fills that space instead of moving the screen. Once
  the conversation is longer, nothing changes. killi asks the terminal where the cursor is
  at start (`CSI 6n`), so a terminal that doesn't answer it waits 2 s and keeps the old
  layout.
- **A finished turn no longer leaves a stale task list.** When the agent ends its reply with
  `todo_write` items still open, killi asks it once to mark what is done (or carry on), as it
  already does for background tasks left running; `-p` runs and subagents are left alone.
- **`ask_user_question` with the wrong arguments** (no `question`, or options that are objects
  instead of strings) is sent back to the model as an error. Before, the user was shown a
  blank question with no choices.

## 0.2.24 — 2026-10-22

- **Sessions clean up after themselves.** A command that leaves something running, such as
  `nohup server &`, becomes a background task instead of being forgotten. When a turn ends
  with tasks still running, killi asks the agent once to stop what nothing needs or say why
  it stays. Ending the session or a `-p` run stops the rest and names them (`--json`:
  `stopped`), now also when the terminal window is closed, on SIGTERM, and on Ctrl+C under
  `-p`.
- **The slash menu while the agent works** offers what runs then: `/btw`, `/plan`,
  `/swarm`, `/later` and `/help`. Before, `/` showed nothing until the turn ended.

## 0.2.23 — 2026-10-22

- **Images in the prompt.** Ctrl+V pastes the clipboard's image (a screenshot, or an image
  file copied in Finder) as `[Image #1]`; Cmd+V does too in Kil 0.0.13, which tells killi
  when the clipboard holds only an image. Large images are shrunk as `read` shrinks them,
  deleting the placeholder drops the image, and a model without `image_in` says so. Images
  pasted while the agent works go with your message at its next step.
- **The banner keeps up.** After `/model`, `/effort` or `/reload` it is drawn again with
  what is now in use, while it is still on screen; before, it kept the model the session
  started with.
- **The banner lists the setup in its column**, one row each: `rules` (the AGENTS.md files
  and their tokens), `skills`, `mcp`, a custom `prompt` and what is noted for `later`, with
  the model in bold and effort and tokens in the status line's colors. It used to be one dim
  line that ran off narrow screens. The keys under it are in lagoon, and an MCP server that
  didn't start is a warning of its own.

## 0.2.22 — 2026-10-22

- **A reply that leaves something for later without noting it gets a nudge.** When the
  agent's final message says it left work out, deferred it or made it a follow-up, and it
  didn't call `later` in that turn, a dim line under the reply says so.
- **`/session` names the later list** when it has open items, so another agent reading the
  handoff sees what was put off too.

## 0.2.21 — 2026-10-22

- **`later`: what the agent won't do now is written down the moment it sees it.** A new
  tool notes a blocker, a bug in passing or an improvement, with where, into one list per
  folder (`~/.killi/sessions/<folder>/later.md`, open items only), and the prompt tells the
  agent to use it right then rather than in the final message. `/later` shows the list
  numbered, `/later done 3` (or the tool's `done`) crosses one off, and the banner says how
  many are open.
- **Diffs show whole.** An edit's diff is shown up to 40 rows (was 16), so a typical change
  no longer ends in `… 5 more lines`; a longer one says `· Ctrl+O`, and Ctrl+O now shows the
  diff itself rather than the tool's text.
- **Kil shows killi's notifications itself** from Kil 0.0.11, which takes OSC 9 and goes to
  the pane when the notification is clicked. On an older Kil the notification is still
  posted in Kil's name, so clicking it brings Kil forward.

## 0.2.20 — 2026-10-22

- **`/session` hands a session to another agent.** It copies the session's id, the path of
  its log, the model, effort, prompts, requests, tool calls and tokens, the prompts so far,
  and a line on how the log is laid out. Paste that into another agent and it can read every
  message, tool call, result and usage record of this session from the log, to debug or
  tune killi. The copy goes through the terminal (OSC 52) and `pbcopy`.
- **`/prune` clears old sessions.** It offers how many of the newest sessions to keep (5 to
  200, each saying how many it removes), or `/prune 20` keeps that many at once. Every other
  session on the machine goes, with its saved outputs and subagent logs; the running session
  always stays, and the notice says how many went and how much space they took.

## 0.2.19 — 2026-10-22

- **A light terminal gets the light palette on its own.** At start killi asks the terminal
  for its background color (OSC 11) and picks the theme by its brightness, so iTerm2,
  Ghostty, WezTerm, kitty and Terminal.app on a light background no longer need
  `COLORFGBG` or `[ui] theme`. A terminal that doesn't answer within 200 ms keeps the old
  detection; `[ui] theme` skips the question.
- **Clicking a notification comes back to the terminal.** On a terminal without its own
  notifications (Kil, Terminal.app, VS Code), the "Done" notification is posted in the name
  of the app the terminal runs in, so clicking it brings that app to the front instead of
  opening Script Editor. iTerm2, Ghostty, WezTerm and kitty keep their own (OSC 9).
- **The README's screenshots show the current design**, drawn from a real run by
  `scripts/screenshot.py`: it runs killi in a pseudo-terminal and draws the screen as an
  SVG, cell for cell, so a screenshot is one command instead of a cropped window grab.

## 0.2.18 — 2026-10-22

- **The design system's own colors.** On a terminal that announces 24-bit color
  (`COLORTERM=truecolor`, as iTerm2, Ghostty, WezTerm and kitty do), lagoon, fin, warn,
  danger, the faint marks, the status line's effort, tokens and branch, and the tint behind your messages and the selected row are now the
  exact tokens of `docs/killi-design-system.html`, so the band behind your message is the
  teal slab the design shows instead of a grey one. Other terminals keep the nearest
  256-color shades.
- **The empty prompt says what it takes.** A faint `ask, or / for commands and skills` sits
  after the caret until you type, and while the agent works it reads `a message for the next
  step · /btw asks on the side`, so the keys the banner showed stay discoverable after it has
  scrolled away.

## 0.2.17 — 2026-10-22

- **The status line no longer flashes while the agent works.** The live area is painted in one
  write inside a synchronized update, with the cursor hidden, and a spinner tick rewrites only
  the rows that changed. In a 4 s turn the status line was rewritten 59 times, now about 10, and
  only when its content changed.
- **The status line colors each kind of figure.** The effort, the token counts with the cache
  rate and the git branch each have their own hue (`effort`, `tokens`, `branch` in the design
  system), `│` is faint, and the context figures and a dirty `*` turn yellow, the meter from
  80%. The banner shows the effort in its color too.
- **Room to read.** One rule decides where blank rows go: notices, errors, questions,
  `/help`, `Ctrl+O` output and `/btw` answers are each set apart, and a thought no longer
  touches the prose after it. A blank row sits above the prompt and above the task list, and
  under the prompt before a picker or a question's choices, so the conversation, the work and
  the prompt don't touch. `/help` lines up its columns. A failed request shows `HTTP 401:
  Invalid API key` instead of the raw JSON, and no `0 in, 0% cached` summary after it.
- **`/btw` is a block you can see.** While it is answered it shows as `◐ btw <question> · 2s`
  in the live area. The answer is one block: the question, the answer hung off the `│` gutter
  and a last line saying it is not kept. If the model is in the middle of a paragraph the
  block waits for its end instead of cutting it in two.

## 0.2.16 — 2026-10-22

- **A resumed session shows its conversation again.** `killi -r`, `-c` and `/resume` print the
  last 8 prompts with the model's replies and each tool call with its result, a long prompt
  folded after 6 lines. The context meter starts at the size the session had.
- **A spent usage limit fails at once.** A 429 that says the quota or usage limit is used up
  (or a `Retry-After` over a minute) is no longer retried ten times; the error shows the
  provider's reset time and points to `/model`.
- **The repeat breaker stops sooner and says why.** It stops at 8 identical tool calls (was
  12), warns that the result was the same, and the notice says what to do next. A command
  that printed nothing now reports `(no output, exit code 0)`.
- **`bash` takes a `cwd`.** Run a command in another folder, such as a worktree, without
  `cd … &&` on every call.
- **Logs keep time.** Every log record has a `ts` (milliseconds); a request's usage record
  has its `ms`, and a tool result keeps how it ended and how long it took. The log notes once
  when a provider bills reasoning tokens but sends no reasoning. Sessions that never got a
  prompt no longer leave a log behind.

## 0.2.15 — 2026-10-22

- **A polished conversation, following the new design system** (`docs/killi-design-system.html`).
  Two colors with one meaning each: lagoon for killi (the prompt caret, running and finished
  work, plan and swarm mode, commands, inline code, links) and orange for you (your messages,
  questions, your answers). Finished tool calls are `✓` or `✗` with the tool's name in bold,
  a running call turns `◐` in the same column, and output and diffs hang off a faint `│`
  gutter. The task list marks tasks `✓ ◐ ○`; the context meter is lagoon and turns yellow
  from 80%. The `killi` spinner now shows once, on a working row that stays while the agent
  works. Every state keeps a glyph, so it still reads with `NO_COLOR`.

## 0.2.14 — 2026-10-22

- **`/reload` takes effect without a restart.** Skills added or edited since the session
  started can now be loaded by the model and run as `/<skill>`, and they show in the slash
  menu. AGENTS.md changes and `[prompt]` reach `/new` sessions too. A broken `config.toml`
  now shows an error instead of being ignored. A session that started with no skills still
  needs a restart before the model can load one; `/<skill>` runs it meanwhile.

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
