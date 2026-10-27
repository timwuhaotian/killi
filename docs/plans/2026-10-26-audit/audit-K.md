## Project Risk & Operational Readiness - Audit Results
**Score**: 3/4
**Files reviewed**: 35 (ci.yml, RELEASING.md, AGENTS.md, README.md, CHANGELOG.md, install.sh, .gitignore, both Cargo.toml, scripts/{release.sh, eval.py, sessions.py, live_check.py, frames.py, fake_llm.py, bench.py, screenshot.py, probe.py, chart.py}, both `.agents/skills/*/SKILL.md`, docs/decisions.md (Status, 39, 42–49), docs/bench.json, and in `crates/killi/src`: main.rs, tools/{bash,web,files}.rs, hooks.rs, mcp.rs, broker.rs, tui/mod.rs (guard, notify, turn end), tui/paste.rs, config.rs (first run), session.rs, update.rs, agent.rs (tests, dispatch, account))
**Issues found**: 13 (P0 0 · P1 0 · P2 5 · P3 8)

Scratch work is in `scratchpad/K/` (tools/, logs, scratch builds). Nothing in the repository was changed. killi only ran under scratch HOMEs.

### P0 - Critical (must fix before merge / blocks production)
None.

### P1 - High
None.

### P2 - Medium

- **[scripts/release.sh:12-20, RELEASING.md:7-16]** Nothing stops a release on red CI or from an uncommitted tree, and nothing checks the version or the changelog heading.
  Scenario:
  (a) CI on `main` was red for 12 runs over 31 h (10-24 08:44 → 10-25 15:10 UTC) while 0.4.7–0.4.13 shipped. release.sh only reruns the tests locally, where the environment-dependent test passed, and no step looks at CI.
  (b) RELEASING.md's order builds at step 4 with the bump, lock and changelog still uncommitted. In practice the code changes are uncommitted too: daa86fa's release commit also touches agent.rs, config.rs and llm/*. release.sh never runs `git status`, so a modified-but-unstaged or untracked source file goes into the shipped binary but not into the commit.
  (c) The self-eval-improve skill (step 7) documents the other order: commit, then release.sh, then commit `releases/`. 0.4.12 used it (90efb18, then 75c5a0c "releases: 0.4.12 binary"). Every other release used a single commit.
  (d) No check that `version` is above the last released one. A forgotten bump replaces the binary under the same version, and `killi update` (update.rs:45, `<=`) never delivers it to installed copies.
  (e) The changelog step is manual and has slipped once: 6c64a50 (0.4.2) renamed the `## 0.4.1` heading instead of adding one above it, so 0.4.1's entry now reads as part of 0.4.2.
  Impact: the only gate is a local `cargo test` on whichever Mac cuts the release, and CI results arrive after users can already `killi update`. No damage yet: all 67 release commits are internally consistent, and 0.4.20 rebuilds identical apart from LC_UUID (see Checked and fine). The red-CI case has already happened once.
  Verified: traced. Release history scripted over `git log -- releases/killi-arm64`, with per-commit version, lock, sha256 and rustc checks. CI history from `gh run list` (80 runs).
  Suggested fix: adopt the skill's commit-first order in RELEASING.md: commit and push the code and bump, let CI run (~70 s), then run release.sh and commit `releases/`. release.sh then refuses (`--no-ci` override) unless:
  - `git status --porcelain -- . ':!releases'` is empty;
  - the CI run for `HEAD` on main concluded `success` (`gh run list -b main --json headSha,conclusion`);
  - `version` sorts above `git show HEAD:releases/latest.toml`;
  - `grep -q "^## $version " CHANGELOG.md` succeeds;
  - `releases/killi-arm64 --version` prints `killi $version`.

  About 10 lines, about 2 s.

- **[crates/killi/src/tui/mod.rs:3843-3847, main.rs:63-73]** A panic (release builds abort) kills none of killi's children and leaves no trace in the session log.
  Scenario: with `panic = "abort"`, the TUI hook restores the terminal and prints the message; `-p` has no hook at all. No destructors run and `tools::kill_all` is never called. Every child runs in its own session (decision 39), so the closing terminal's SIGHUP never reaches them either. Background tasks (dev servers, watchers), Chrome and hooks keep running as orphans. The log stops after the last assistant message, and `scripts/sessions.py` files it under "interrupted, killed or stopped by the repeat breaker".
  Impact:
  - An orphaned dev server keeps its port, so the next session's `npm run dev` fails with "address in use".
  - The self-eval loop (decisions 31–46) cannot see crashes at all, which its own skill classes as a "log gap".
  - What a user can send when something breaks is `/session` plus the jsonl, which has carried `version` since 0.4.11. A panic adds nothing to that bundle.
  - The presence file is cleaned by the next reader (live_check "a crashed session's file is removed" passes).
  Verified: reproduced. Scratch copy with a test-only `panic_now` arm in `Agent::dispatch`, built with `CARGO_PROFILE_DEV_PANIC=abort`. The scripted provider starts `bash /bin/sleep 304` in the background and then calls `panic_now`. Result: exit 134, `sleep 304` still running (pgrep), and the log's last line is the `panic_now` call (`tools/panicsrv.py`, `home-panic/`).
  Suggested fix: install one panic hook in `main()` for every mode. It should call `tools::kill_all()` (with `try_lock` on `GROUPS`), `live::leave()` and `restore_terminal()`, and append `{"type":"error","panic":…}` to the current log through a `OnceLock<PathBuf>` set when the `Session` is created. sessions.py already prints `error` entries verbatim.

- **[crates/killi/src/main.rs:121-139, tools/bash.rs:121-146, tools/web.rs:245-266, hooks.rs:105-114, mcp.rs:372]** Quitting on SIGHUP or SIGTERM ends only bash process groups; Chrome, hook and directly spawned MCP groups survive. This confirms intel-0a obs 18.
  Scenario: `quit_on_signals` runs `kill_all`, which covers only `GROUPS` (filled by bash `Process::spawn`), and then `exit`. No destructors run, so `Running::drop` (Chrome) and the hooks' `Group::drop` never fire, and both are setsid'd beyond the terminal's reach.
  Impact:
  - Closing the window during `fetch_url` render or screenshot (up to 45 s) leaves a headless Chrome running (it "often doesn't exit afterwards", web.rs:267, :297), plus its `killi-chrome-*` profile in `$TMPDIR`.
  - A long hook survives the same way.
  - Direct MCP children (broker unreachable) survive too, but normally exit on stdin EOF. The broker surviving is by design.
  - The signal path also skips `discard_if_unused` (tui/mod.rs:269), so a session closed before its first prompt leaves an empty log. `-c` and `/resume` skip prompt-less logs, so this is clutter only.
  - pdftotext (web.rs:139) has no own session and no `kill_on_drop`. It shares killi's group, so a SIGHUP reaches it, but Esc orphans it until it finishes.
  Verified: reproduced (`tools/sigrun.sh`). Headless killi with a scripted provider, three child types:
  - `KILLI_CHROME` pointing at a script that runs `exec sleep 301`, reached through `fetch_url render`;
  - a `PreToolUse` hook running `/bin/sleep 302`;
  - as control, bash running `echo start; /bin/sleep 303`.

  After SIGTERM (exit 143) and SIGHUP (exit 129), 301 and 302 were still running and 303 was gone.
  Suggested fix: `GROUPS` holds `watch::Receiver<Option<Exit>>` for its pid-reuse check, so the other guards can't register there directly. Add a second registry, a pgid set that `Running::start`, the hooks' `execute` and the direct-spawn branch of `Connection::open` insert into and their `Drop` removes. `kill_all` then sweeps both, using the same `killpg(pgid, 0)` liveness check as `group_running`.

- **[crates/killi/src/main.rs:94-98, config.rs:505-523, 928-943]** The signal handlers are installed before the blocking first-run questions. Ctrl+C, SIGTERM and SIGHUP do nothing until the guide ends, and the swallowed Ctrl+C then fires later.
  Scenario: tokio's handler replaces the default action, but its task can't run while `read_line` blocks the only runtime thread, and SA_RESTART restarts the read.
  Impact:
  - A new user who presses Ctrl+C to back out gets a written config, then a session that dies the moment it appears.
  - The same mechanism makes killi unkillable except by SIGKILL during any blocking call on the runtime thread, for example a hung `git` in `snapshots.take()`, which has no timeout (intel-0b).
  - EOF counting as yes is B's obs 8 and is not repeated here.
  Verified: reproduced (pty, scratch HOME, no config; `tools/ctrlc_firstrun3.py`, `tools/sig_probe.py`):
  - At "Write a starting config to edit? [Y/n]", Ctrl+C echoes `^C` and killi stays alive.
  - Enter (= yes) writes config.toml.
  - Enter at the command guide: the TUI draws, and killi exits 130 that same instant.
  - `kill -TERM` and `kill -HUP` at the first prompt: still alive after 6.6 s, and it needed SIGKILL.
  Suggested fix: call `quit_on_signals()` after the first-run block (main.rs:95-99). Nothing needs cleaning up before then, so the default dispositions are right. `read_secret` turns ISIG off and already treats Ctrl+C as a byte, so it is unaffected.

- **[crates/killi/src/tools/web.rs:874-881, :754-758, plus the local-server tests in tools/web.rs and login/chatgpt.rs]** The test suite still reaches the internet and depends on the machine's network setup.
  Scenario: what the test binary does under each condition:

  | Condition | Result |
  |---|---|
  | Offline (sandbox-exec denying outbound IP and the mDNSResponder socket, loopback allowed) | 338/338 pass |
  | Empty scratch HOME | 338/338 pass; nothing written to HOME, so no dependency on `~/.killi` or `~/.gitconfig` |
  | Dead proxy (`HTTP(S)_PROXY`/`ALL_PROXY=http://127.0.0.1:9`) | **12 fail**: web.rs 686, 697, 749, 757, 770, 847, 888, 901, 917 and chatgpt.rs 793, 813, 826 |

  The proxy failures happen because reqwest routes 127.0.0.1 through the environment proxy, and nothing sets `NO_PROXY` or `.no_proxy()`. Individual tests:
  - `every_source_failing_names_them_all`, the 0.4.14 fix for the red streak, still calls `api.search.brave.com` with key "k" on every run. It gets 422 in about 0.6 s, so the outcome is stable, but it is a third-party call per run, and on a black-holed network it takes 20 s × 2 attempts.
  - `an_unresolvable_host_*` needs a resolver that answers NXDOMAIN for `.invalid`. It breaks under DNS-hijacking resolvers, captive portals and proxies.
  - `searxng_reads_the_instance_results` falls through to the live search.brave.com page whenever its local server can't be reached.
  Impact: not a CI flake today, since runners set no proxy. But it is the class of test that caused the 31-hour red streak, and on a Mac with a corporate proxy in the environment it means 12 red tests and a blocked release.sh. The same proxy routing sends a user's `localhost:8888` SearXNG or local model through a corporate proxy in production (cross-ref 0b obs 12 / E).
  Verified: tested. Test binary under `sandbox-exec -f nonet.sb`, under proxy env vars, and with `HOME=<empty>`. curl under the same sandbox confirmed that DNS and IP traffic were blocked.
  Suggested fix:
  - Give `Source::BraveApi` a base URL field (or parameter) and point the test at a dead local port.
  - Set `NO_PROXY` for tests. Tested: `.cargo/config.toml` with `[env] NO_PROXY = { value = "127.0.0.1,localhost", force = true }` made 11 of the 12 pass under the dead proxy.
  - Let the `.invalid` test also accept a connect or proxy error, or mark it `#[ignore = "needs NXDOMAIN DNS"]`.

### P3 - Low / Nits

- **[crates/killi/src/tools/bash.rs:781-784 vs :46-54, :398-402]** `a_command_that_ends_first_is_not_moved_when_a_message_waits` asserts a timing property the code does not guarantee. This is a test defect.
  Scenario: `until()` sleeps `WAKE_POLL` (100 ms) before its first check, and its doc says only that a call ending sooner wins. When process spawning under load pushes `echo quick` past 100 ms, it is moved to the background by design.
  Impact: release.sh aborts whenever other agents or builds load the Mac, and each CI flake adds to "red means nothing". The failing outputs were `(no output)\n[A message from the user is waiting, so it moved to background task 1 after 0s…]` and `quick\n[…moved…]`.
  Verified: tested.
  - 6 concurrent test binaries on 10 cores: 8/18 runs failed.
  - Alone or in pairs: 0/20 failed.
  - CI: 0 of the last 7 runs failed, but the runner has 3 vCPUs.
  Suggested fix: accept either valid outcome, or have the wake closure turn true only after the first poll. A message arriving mid-command is the real product case; a message already waiting gets `SKIPPED_FOR_MESSAGE` in `execute_calls`.

- **[crates/killi/src/tui/mod.rs:3618-3623]** `notify()` spawns `/usr/bin/osascript` with std's `Command::spawn` and drops the `Child`, leaving one zombie per notification for the rest of the session.
  Scenario: affects terminals without OSC 9 (Terminal.app, VS Code). Kil ≥0.0.11, iTerm2, WezTerm, Ghostty and kitty use OSC 9 and are unaffected.
  Impact: small. One process-table slot per turn longer than `notify_after` (10 s), released when killi exits.
  Verified: tested. A scratch bin in the crate copy uses tokio 1.54 current-thread with `tokio::process` and `tokio::signal` active, as killi does. After 5 drop-spawns, `ps` showed 5 zombie children. tokio's SIGCHLD handling reaps only its own children.
  Suggested fix: use `tokio::process::Command`, which reaps dropped children in the background, or wait in a detached thread.

- **[.github/workflows/ci.yml:11-30]** CI never runs the binary, and `cancel-in-progress` on `main` can drop a release's run.
  Scenario:
  - `live_check.py` is ready for CI: stdlib only (no pyte), scratch HOME, ephemeral port.
  - It is the only end-to-end check of the real binary, covering pty, peers, SIGHUP cleanup and the undo warning.
  - cargo-audit isn't run. The yanked tokio 1.54.0 (RUSTSEC-2026-0384) has shipped in every release since the first lock (intel-0b).
  - `concurrency: ci-${{ github.ref }}` with `cancel-in-progress: true` cancels release N's run when release N+1 lands within about 70 s. One run in history was cancelled this way.
  - A CI run takes 70 s today (tests 32-35 s).
  Impact: a regression in startup, pty handling or live sessions passes CI. An advisory against an unchanged lock is never flagged.
  Verified: tested. live_check passed 29/29 in 34 s, over 4 runs including 3 at once. `eval.py verify` passed 14/14 in 37.6 s.
  Suggested fix, cheapest first:
  1. `cargo build --locked` + `python3 scripts/live_check.py --killi target/debug/killi`: about +50 s.
  2. cargo-audit from a prebuilt binary plus a weekly `schedule:` trigger: about +15 s (estimated, not measured on a runner).
  3. `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}`.

  Not worth adding here:
  - A release build in CI: about 3-4 min on a 3-vCPU runner, there is no `cfg(debug_assertions)` code, and release.sh builds it anyway. Better spend: have release.sh run live_check against `target/release/killi`.
  - An MSRV job: clippy's `incompatible_msrv` under `-D warnings` plus resolver 3 cover most of it.
  - frames.py: it has no pass/fail check.
  - SHA-pinning the actions: no secrets and a read-only token.

- **[scripts/frames.py:385, :20; scripts/live_check.py:148; AGENTS.md]** The two checks AGENTS.md requires after a `tui/` or `live.rs` change both default to `target/release/killi`. Only release.sh or `cargo build --release` refresh that binary, yet frames.py's own help says "a built killi (`cargo build`, or --killi)".
  Scenario: edit `tui/`, run `cargo build`, run `scripts/frames.py out/`. The frames show the last release, not the change, and look fine.
  Impact: TUI and live-session changes can be "checked" against the old binary. Also:
  - The system python3 here has no pyte and there is no requirements file.
  - `out/`, the folder AGENTS.md names, isn't in `.gitignore`. The self-eval skill already warns "this folder accumulates strays (out/)".
  Verified: traced. `git check-ignore out/x` confirmed `out/` is not ignored, and `python3 -I -c 'import pyte'` failed.
  Suggested fix: default `--killi` to the newer of `target/{debug,release}/killi` and print which one ran, or refuse when it is older than the newest file under `crates/killi/src`. Add `/out/` to `.gitignore`.

- **[RELEASING.md:16-18, crates/killi/src/update.rs:45, install.sh:6]** There is no procedure for a bad release.
  Scenario: `killi update` refuses any version that isn't newer, so reverting `latest.toml` does nothing for users who already updated. The real path is fix-forward: revert, bump, release (2-3 min of release.sh).
  - Users whose updater is broken can reinstall any committed release by SHA: `curl -fsSL …/install.sh | KILLI_RELEASES_URL=https://github.com/timwuhaotian/killi/raw/<sha>/releases sh`. This is undocumented.
  - RELEASING step 6 runs the old binary's updater and never runs the new binary.
  Impact: in an incident the recovery steps would have to be worked out on the spot.
  Verified: tested. `…/raw/4ace99f/releases/latest.toml` returned version 0.4.19, and the binary returned 200 with 7,256,816 bytes.
  Suggested fix: add a "Bad release" section to RELEASING.md covering both paths, and have step 6 also run `killi --version` after updating.

- **[Cargo.toml:9, scripts/release.sh:12-14, .github/workflows/ci.yml:25]** Each release is built by whatever compiler the releasing Mac has. Three different compilers built the 67 committed binaries, and CI uses a fourth.
  Scenario: rustc paths embedded in the 67 committed binaries:

  | Compiler | Releases | Committer |
  |---|---|---|
  | rustup 1.97.1 (8bab26f4f) | 41 | "Tim, WU" |
  | Homebrew rustc 1.94.0 (`/private/tmp/rust-20260305-…/rustc-1.94.0-src`) | 24 | "timwuhaotian" |
  | 58cf8f9c3 | 2 | "timwuhaotian", CM1deMac-mini |

  Consecutive releases flip: 0.4.1–0.4.12 were built with Homebrew 1.94.0, 0.4.13 onwards with 1.97.1. CI uses floating stable (1.99.1).
  Impact: the clippy and test results come from a different compiler than the shipped binary, and a new stable's lints can turn CI red with no code change.
  Verified: tested. `strings` over every committed binary, grouped by commit author.
  Suggested fix: add a `rust-toolchain.toml` (`channel = "1.97.1"`, `components = ["clippy"]`) so every Mac and CI build with one compiler. Confirm which wins when both the file and CI's `dtolnay/rust-toolchain` step are present, or set the step's `toolchain:` to the same version.

- **[README.md:11, :35, docs/bench.json, CHANGELOG.md (0.4.2 entry)]** Some numbers and claims have drifted.
  Scenario:
  - README says "6 MB" twice; the 0.4.20 binary is 7,256,816 bytes.
  - docs/bench.json and the charts come from 0.2.33 (2026-10-23). The refresh-metrics skill hasn't been run since, more than 60 releases ago.
  - The 0.4.2 entry says every child runs in its own session and "is still ended with everything it started". pdftotext doesn't run in its own session, and on a signal exit hooks and Chrome aren't ended (P2 above).
  - "About 2,600 tokens" still holds: 10,547 of 12,000 chars.
  Impact: readers get outdated size figures and an overstated cleanup guarantee.
  Verified: traced.
  Suggested fix: refresh the size line, and either fix the code or reword the 0.4.2 claim.

- **[crates/killi/src/agent.rs:1917-1939]** `idle_agent` creates `$TMPDIR/killi-<name>-<pid>` directories that no test removes.
  Scenario: every `cargo test` run leaves them behind.
  Impact: about 11 directories (~0.9 MB) per run, with several runs a day across release.sh, CI and agents. macOS purges them after about 3 days unused.
  Verified: tested (directory listing before and after one run).
  Suggested fix: remove the directory at the end of each test, or use a Drop guard.

### Checked and fine
- All 67 release commits are consistent: `latest.toml` version = Cargo.toml = Cargo.lock, and its sha256 matches the committed binary.
- 0.4.20 rebuilt from 09c33a7 (same toolchain and RUSTFLAGS) is byte-identical except LC_UUID and the signature hashes, 47 bytes in all. The build path enters the UUID. So no uncommitted code shipped in the latest release. A hash-compare reproducibility check needs the UUID masked or the same build path.
- `install.sh` and `killi update` fail closed on raw-CDN skew ("checksum mismatch"). install.sh verifies before `mv`.
- Session logs are unbuffered O_APPEND line writes, so they are complete up to a signal or abort.
- At an idle TUI, SIGTERM, SIGHUP and SIGINT exit within 0.1 s (143/129/130), remove the presence file and kill bash groups. Reproduced; live_check also covers SIGHUP.
- Tests don't touch `~/.killi`, `~/.gitconfig` or the clipboard, and never call `sips`, `osascript`, `pbcopy` or a real Chrome. Env-mutating tests use unique variable names.
- `scripts/sessions.py` parses current logs (run over synthetic scratch logs). The `--json` summary matches `eval.py`'s `killi_metrics`. `eval.py verify` passes 14/14. All scripts parse.
- 0 TODO/FIXME/HACK/`todo!`/`unimplemented!`. No `cfg(debug_assertions)` code, so release and test builds differ only by `panic = "abort"` and LTO.
- The red-streak root cause (Brave's public page in the test chain) is gone since 0.4.14, with 7 green CI runs since.
- Every command and path in AGENTS.md exists and behaves as described, apart from the frames.py default-binary trap above.
- intel-0a lead 18: confirmed. The signal leaks (P2) are reproduced, and the `notify` zombies (P3) were tested in a scratch program. The `notify` content/injection half belongs to E.
