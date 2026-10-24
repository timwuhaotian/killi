# Changelog

Notable changes to killi. `install.sh` and `killi update` always install the latest release
from [`releases/`](releases); the versions below match `Cargo.toml`.

## Unreleased

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
