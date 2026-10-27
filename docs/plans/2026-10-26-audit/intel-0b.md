## Codebase Intelligence — Dependencies & Tech Stack

Agent 0b, 2026-10-26. Repository read-only; `Cargo.lock` SHA-256 `44c0c282…5ec42` before and after every command (checked with `shasum -a 256 Cargo.lock` after `cargo update --dry-run`, cargo-audit and the MSRV build); `git status` clean throughout. Network was available: crates.io index/API, GitHub, GitHub API (`gh`), the RustSec advisory DB and static.rust-lang.org all answered (`curl -m 20 …` probes returned 200).

### Tech Stack Summary

- **Language/toolchain:** Rust, edition 2024, workspace `resolver = "3"` (MSRV-aware resolution), one crate `crates/killi` (39 `.rs` files, 23,020 lines). `rust-version = "1.94"`. Local toolchain: cargo/rustc 1.97.1 (the release machine). Current Rust stable: **1.99.1** (2026-10-13, from `channel-rust-stable.toml`). CI uses floating `stable`.
- **Release profile:** `opt-level=3`, `lto="thin"`, `codegen-units=1`, `panic="abort"`, `strip=true` (Cargo.toml). `scripts/release.sh:10` adds `--remap-path-prefix=$HOME=~`. Verified: no `/Users/` strings in the shipped binary.
- **Runtime:** Tokio **current-thread** runtime for every subcommand, including `mcp-serve` (main.rs:65-66, 76-84). CPU-heavy grep/glob and readability extraction use `spawn_blocking` (tools/search.rs:25,32; tools/web.rs:199). Subprocess calls in snapshot, git status, osascript, sips and pbcopy do not (see Integration Points).
- **HTTP/TLS:** reqwest 0.12.28 with `default-features = false`, `rustls-tls`, `stream`, `http2`. That means rustls 0.23.46 on *ring*, with **compiled-in Mozilla roots (webpki-roots 1.0.10), not the macOS Keychain**. `system-proxy` is off: `HTTP(S)_PROXY`/`ALL_PROXY`/`NO_PROXY` env vars are honored (hyper-util `Matcher::from_env`), but macOS System Settings proxies are ignored. I confirmed this from the vendored reqwest 0.12.28 `Cargo.toml` and hyper-util 0.1.21 `matcher.rs:238-249`.
- **TUI:** crossterm 0.29 (`event-stream`) plus a hand-written renderer. **HTML readability:** dom_smoothie 0.18.2 (html5ever stack). **Search:** ignore/globset/regex (ripgrep's libraries, no `rg` binary).
- **Platform:** macOS arm64 only (install.sh:9, release.sh:7). It shells out to Apple binaries: `/usr/bin/osascript`, `/usr/bin/sips`, `/usr/bin/shasum`, and `pbcopy`/`open` via PATH.
- **Size:** `releases/killi-arm64` is 7,256,816 bytes. decisions.md "Status" still says 6.0 MB, which is stale.
- **Dev tooling (not shipped):** Python scripts in `scripts/` use the stdlib plus `pyte`, which is unpinned (no requirements file); `tomllib` needs Python ≥3.11.

### Dependency Inventory

Commands: `cargo tree --depth 1 --locked`, `Cargo.lock`, `cargo update --dry-run --verbose` (lock unchanged), and a crates.io API query per crate (`scratchpad/tools/latest.py`). "Excl." is the number of crates compiled only because of that dependency, from the real feature-resolved graph (`cargo tree -e normal,build --prefix depth`, analysed by `scratchpad/tools/tree_subtrees.py`).

| Package | Requested | Locked | Latest | Status | Notes |
|---|---|---|---|---|---|
| anyhow | 1 | 1.0.104 | 1.0.104 | ok | Error type everywhere. Excl. 1. |
| base64 | 0.22.1 | 0.22.1 | 0.23.1 | outdated (0.x minor = breaking) | Used only in login/chatgpt.rs (URL_SAFE_NO_PAD for PKCE/JWT, :15-16,480,558,563). Same version reqwest 0.12 uses. hyper-util pulls 0.23.1, so two copies are compiled. Two hand-rolled STANDARD encoders duplicate it (tools/files.rs:136-151, tui/mod.rs:3584-3598). Excl. 0. |
| crossterm | 0.29.0 (+event-stream) | 0.29.0 | 0.29.0 | ok | Last release 2025-04-05. Default `windows` feature is a no-op here. Excl. 6 (rustix, signal-hook, signal-hook-mio, document-features, litrs). |
| dom_smoothie | 0.18.2 | 0.18.2 | 0.18.2 | ok (pins older html5ever 0.39 / dom_query 0.28; 0.40.1 / 0.29.0 exist) | Readability for `fetch_url`. **Excl. 33**: html5ever, markup5ever, dom_query, selectors, cssparser(+macros), phf(+macros, codegen, generator, shared), string_cache(+codegen), tendril, web_atoms, servo_arc, nom, gjson, bit-set, … Smallest-community crate in the tree (≈538k recent downloads, against 10⁸ for core crates). |
| futures-util | 0.3 | 0.3.34 | 0.3.34 | ok | StreamExt. Already pulled in by reqwest/hyper. Excl. 0. |
| globset | 0.4 | 0.4.20 | 0.4.20 | ok | `glob` tool. Also pulled in by ignore. Excl. 0. |
| ignore | 0.4 | 0.4.33 | 0.4.33 | ok | grep/glob/@-mention/snapshot walks. Excl. 6 (crossbeam-*, walkdir, same-file). |
| libc | 0.2.183 | 0.2.191 | 0.2.191 (1.0.0-alpha.5 pre) | ok | flock, killpg, setsid, getuid, sigaction, poll. |
| regex | 1 | 1.13.1 | 1.13.1 | ok | Default features (full Unicode). Excl. 1. |
| **reqwest** | 0.12 (no defaults; rustls-tls, stream, http2) | 0.12.28 | **0.13.5** | **outdated (major)** | **Excl. 63** (hyper, h2, http, tower(-http), rustls, rustls-webpki, webpki-roots, tokio-rustls, url, idna plus 13 ICU4X crates for IDNA, …). 0.13 is not a drop-in (see Health Flags). |
| ring | 0.17.14 (default-features=false) | 0.17.14 | 0.17.14 | ok (security-maintenance only) | SystemRandom and SHA-256 for PKCE (chatgpt.rs:552,563). It is rustls's crypto provider anyway, so Excl. 0. RUSTSEC-2025-0007 ("unmaintained") was **withdrawn** on 2025-02-22; the rustls team now maintains it. |
| serde | 1 (derive) | 1.0.229 | 1.0.229 | ok | |
| serde_json | 1 | 1.0.154 | 1.0.154 | ok | Excl. 2. |
| **tokio** | 1 (rt, macros, process, time, io-util, sync, net, signal) | **1.54.0 (yanked)** | 1.54.1 | **vulnerable (RUSTSEC-2026-0384) + yanked** | Exposure is nominal (see Vulnerability Scan). `tokio::fs` is used at tools/web.rs:136 but `fs` is **not declared**; it compiles only because reqwest's `stream` feature enables `tokio/fs`. |
| toml | 1 | 1.1.8+spec-1.1.0 | same | ok | Config parsing. Excl. 6. |
| unicode-width | 0.2.2 | 0.2.2 | 0.2.2 | ok | Default `cjk`. Excl. 1. |

No `[dev-dependencies]`: tests use only shipped crates (plus `#[tokio::test(flavor="current_thread")]`).

**Counts (one definition each):**
- `Cargo.lock`: **217** packages, including killi.
- Compiled for aarch64-apple-darwin, normal+build edges (what builds the binary): **171** crates besides killi. Normal-only: 164.
- `--target all`: 203, including killi.
- **45 lock-only entries** that are never compiled on this target: Windows/wasm/Linux crates, plus `quinn`, `quinn-proto`, `quinn-udp`, `rand*`, `chacha20`, `getrandom 0.4.3`, `lru-slab`, `tinyvec`, `cfg_aliases`. These are locked through reqwest's weak `quinn?/ring` feature but not activated (`cargo tree -i quinn` prints nothing). cargo-audit scans all 217.

**Heavy subtrees:** reqwest (63 exclusive: HTTP/1+2, TLS, URL/IDNA/ICU4X) and dom_smoothie (33 exclusive: the full HTML5 parser and CSS-selector stack, used only for article extraction in `fetch_url`).

### Vulnerability Scan

- **Tool:** cargo-audit **v0.22.2**, installed with `cargo install cargo-audit --locked --root <scratchpad>/cargo-tools` (the user's `~/.cargo/bin` is untouched). Run from the repo root as `cargo-audit audit --db <scratchpad>/advisory-db -D warnings`; JSON copy at `scratchpad/cargo-audit.json`. Advisory DB fetched fresh from `github.com/RustSec/advisory-db`: **1,352 advisories**, 217 lock entries scanned.
- **Result: 1 vulnerability, 1 warning.**
  - **RUSTSEC-2026-0384** (CVE-2026-115567, GHSA-5wjr-9356-4wc8), tokio 1.54.0: a race between `Interval::poll_tick` and the time driver gives a use-after-free. It affects the **multi-threaded runtime only**; `Sleep`/`Timeout` are unaffected. Patched in ≥1.54.1, dated 2026-10-21.
  - **Warning:** tokio 1.54.0 is **yanked**.
- **Exposure:** killi does use `Interval` (tui/mod.rs:411, update.rs:81 and :155, broker.rs:218). All of it runs on `new_current_thread()` (main.rs:65), where the poller and the driver share one thread, so the described race cannot happen. Practical risk is low.
  - It is still worth fixing. The crate is yanked, `--locked` builds (release.sh:12-14, ci.yml:29-30) keep using it indefinitely, and `cargo update --dry-run` shows 1.54.1 is available within MSRV 1.94. The fix is a patch bump.
  - Timeline: the first lock (31ad771, 2026-10-21 16:28 UTC) predates 1.54.1 (published 21:05 UTC the same day). Every release since then ships 1.54.0, because no `cargo update` has run and nothing audits.
- **TLS stack sits right at the patch edge.** The locked versions are exactly the minimum patched ones:
  - rustls **0.23.46**: required by RUSTSEC-2026-0346/0347/0348/0349 (2026-10-14: secrets in `Debug`, ECH data leak, a 1 GiB brotli cert allocation, ServerHello KEX downgrade).
  - rustls-webpki **0.103.16**: required by RUSTSEC-2026-0382 (2026-10-20: distrusted intermediates accepted).
  - h2 0.4.20 ≥ 0.4.16 (RUSTSEC-2026-0258). tokio's unsound-notice RUSTSEC-2026-0386 needs ≥1.53.3, which 1.54.0 meets. idna 1.1.0 ≥ 1.0.0 (RUSTSEC-2024-0421).
  - So the TLS stack is getting a steady stream of advisories. Because the binary is static and killi **never checks for updates on its own** (no caller of `update::` besides main.rs:79), installed copies stay on whatever TLS stack they were built with until the user runs `killi update`.
- **Yanked crates:** only tokio 1.54.0. **Unmaintained/unsound warnings affecting the lock:** none.

### Integration Points

Timeouts are cited at file:line. "Blocks runtime" means a synchronous call made on the single runtime thread: Esc, redraw, MCP reader tasks and peer-socket answers all stall while it runs.

| Service | Purpose | Auth | Timeouts / retries (file:line) | Failure mode |
|---|---|---|---|---|
| LLM, OpenAI Chat Completions (`{base_url}/chat/completions`; default `https://api.openai.com/v1`; MiMo, MiniMax, gateways, OpenCode Go) | Agent turns, SSE streaming | `Authorization: Bearer` from `api_key`/`api_key_env` (llm/openai.rs:63; config.rs:310-313). Per-provider custom headers and optional session header (llm/mod.rs:146-148,178-183) | connect 30 s and **read-idle 300 s, which also bounds time-to-headers**; no total timeout (llm/mod.rs:124-128). 10 attempts, 500 ms doubling to a 32 s cap plus ≤25% jitter (:336-341). Retries 408/409/425/429/5xx and any transport error. `Retry-After` honored only as integer seconds, up to 60 s (:187-192, 317, 324-333). Quota 429s fail fast (:320-326) | Loud error after retries; partial attempts are discarded. Worst case before the error: DNS failure or connection refused ≈2.1–2.7 min of backoff; black-holed host ≈7 min (10×30 s connect + ≈127 s backoff); accepted connection that never answers ≈52 min (10×300 s). Esc cancels in the TUI; `-p` has only signals. **TLS/certificate errors are classed transient** (:184) and retried 10 times. |
| LLM, Anthropic Messages (`{base_url}/v1/messages`; default `https://api.anthropic.com`) | Agent turns | `x-api-key`, pinned `anthropic-version`, `anthropic-beta: interleaved-thinking-2025-05-14` (llm/anthropic.rs:32-39) | Same client and retry loop | Same |
| LLM, OpenAI Responses (`{base_url}/responses`) | Agent turns | Bearer (llm/responses.rs:49) | Same | Same |
| ChatGPT plan (`https://api.openai.com/v1/responses`, `/v1/models`) | Requests on the user's ChatGPT plan | OAuth access token added per attempt, refreshed 5 min early and once after a 401 (llm/mod.rs:171-199; login/chatgpt.rs:146-165) | Refresh goes through the LLM client (30 s / 300 s). Refresh lock is a non-blocking `flock` polled 600×50 ms = 30 s (chatgpt.rs:226-236) | 5xx/429 on refresh is transient and retried by the LLM loop. `invalid_grant` and similar clear the tokens and tell the user to run `killi login chatgpt` (chatgpt.rs:174-201). |
| OpenAI auth (`auth.openai.com` authorize/token, `/.well-known/openid-configuration`, revocation endpoint) | `killi login/logout chatgpt` | PKCE S256, state and nonce from ring RNG. Dynamic client registration. Callback on 127.0.0.1:1455 or a random port. ID token claims are checked but its **signature is not verified** (documented, chatgpt.rs:472-501) | Login client: connect 30 s, total 60 s (login.rs:98-101). Browser wait 5 min (chatgpt.rs:44,270). Callback reads 5 s per chunk, capped at 16 KB (:417-421) | Loud, no retry. Logout clears local tokens even if revocation fails, and says so. |
| OpenCode Go (`opencode.ai/inference/go/v1/models`) | `killi login opencode-go`: list served models | None on the list call (opencode_go.rs:97-101). Key stored as `api_key` or `api_key_env=OPENCODE_API_KEY` | Login client 30 s / 60 s | Falls back to models.dev's list with a warning (opencode_go.rs:51-57) |
| models.dev (`https://models.dev/api.json`) | Each OpenCode Go model's API, URL, limits and inputs | None | Login client 30 s / 60 s; the whole catalog is read into memory (:114) | Login fails loudly. **Trust:** each model's `base_url` comes straight from the catalog's `provider.api` with no host check (:131-141), and is then used **with the provider's key** (config.rs:305-312). |
| Brave Search API (`api.search.brave.com/res/v1/web/search`) | `web_search` | `X-Subscription-Token` | Search client total 20 s (tools/web.rs:440-444). One retry on 429/5xx/transport (:450, 466-470) | Falls through the chain: configured service, then SearXNG if `url` is set, then the Brave public page (:402-431). When all fail, the error names every source. |
| Brave public results page (`search.brave.com/search`) | Keyless fallback; regex HTML scraping (:546-562) | None; **spoofs a Chrome 140 user agent** (:21) | 20 s total, 1 retry | A captcha or HTML change shows as "turned the search away" or no results (:500-502). Fragile and ToS-sensitive. |
| Tavily (`api.tavily.com/search`) | `web_search` | Bearer | 20 s, 1 retry | As above |
| SearXNG (`{url}/search?format=json`; default suggestion `http://localhost:8888`) | `web_search` on the user's instance | Optional Bearer, which **can go over plain http** (:505-509) | 20 s, 1 retry. Body read without a size cap (:523-531) | 403 points at `search.formats` (:513-515) |
| Arbitrary web (`fetch_url`) | Pages and PDFs; GitHub `blob` URLs rewritten to raw (:176-185) | None; spoofed Chrome user agent | Total 30 s (:42). **Body capped at 10 MB** (:14,163-173). Non-text types refused before download (:58-62). Redirects followed (reqwest default, up to 10) | Error with a hint (:103-123), no retry. No loopback or private-range filter. |
| Headless Chrome / Chromium / Edge / Brave (`/Applications/*` or `$KILLI_CHROME`) | `render` and `screenshot`; started automatically when static HTML has <200 readable characters (:85-93) | n/a | 45 s (:17,287,315). Own session; `killpg` and profile removal on drop (:245-265) | Error string. DOM size is unbounded within the 45 s. |
| `pdftotext` (poppler, via PATH) | PDF text | n/a | **None.** No `kill_on_drop`, no `own_session` (tools/web.rs:139) | Missing binary gives an install hint. A hung run holds the tool call until Esc, and the child is then orphaned. |
| git (PATH): undo snapshots in `~/.killi/snapshots/<tree>.git` | `/undo` | n/a | **None.** On `index.lock` it retries 30×100 ms using **`std::thread::sleep`** (snapshot.rs:49-72) | **Blocks runtime:** `git add -A` and `write-tree` run before every TUI turn (tui/mod.rs:632). At start it walks up to 50,001 files and runs `git --version` (tui/mod.rs:137; snapshot.rs:33-47,124-127). `/undo` also runs synchronously (tui/mod.rs:525). On error undo is turned off with a notice (degrades). A hung git hangs killi. |
| git (PATH): status line | Branch and dirty flag | n/a | None | **Blocks runtime:** two git calls (`rev-parse`, `status --porcelain`) per main-loop iteration (tui/mod.rs:2513-2527, called at :259 and :1009). On failure no branch is shown. |
| `/bin/bash` (bash tool) | Model-run commands | **Inherits killi's full environment**, plus NO_COLOR/TERM/PAGER/GIT_PAGER/KILLI_PID (tools/bash.rs:190-201) | 60 s default, 300 s max, then moved to the background; own session (bash.rs:22-23,168-186,375-400) | Well handled |
| `/bin/sh` hooks (user config only) | Claude-Code-protocol hooks | n/a | 60 s default; the process group is killed on timeout or drop (hooks.rs:61,103-151) | Timeout or failure: the hook is silently skipped |
| MCP stdio servers (`~/.killi/mcp.json`, project `.mcp.json`) | Tools behind the `mcp` gateway; eager servers get native tools | `env` from the config | Per request 120 s or `tool_timeout_ms` (mcp.rs:33,431). Broker initialize 60 s (broker.rs:29,169). Broker start 5 s (broker.rs:99). **Attach handshake has no timeout** (broker.rs:68-88) | When a server exits its clients are released and reconnect on the next call (broker.rs:153-158). **Server stderr goes to /dev/null** (broker.rs:49), so its errors are invisible. Eager servers start inside `Setup::new` and delay startup (setup.rs:64-75). |
| MCP broker (`killi mcp-serve`; `$TMPDIR/killi-mcp-<uid>-<ver>.sock`) | One copy of each stdio server shared across sessions | **None.** No peer-uid check and no request cap. The client-supplied `Launch` is spawned (broker.rs:249-253,42-54) | Exits after 15 min idle (broker.rs:28) | Without a broker, servers start directly. The `servers` lock is held while a server starts (up to 60 s), which stalls every attach (broker.rs:289-296). |
| MCP streamable HTTP | Remote MCP tools | `headers` plus a bearer from `bearer_token_env_var` (mcp.rs:399-403) | Connect 30 s (mcp.rs:404); per-request total = tool timeout (mcp.rs:465) | Loud error, no retry. JSON or SSE body read uncapped within the timeout (mcp.rs:484-500). No user agent sent. |
| Other killi sessions (`$TMPDIR/killi-live-<uid>/<pid>.sock`, `~/.killi/live/*.json`) | `killi sessions/ask/tell` | Peer uid checked (live.rs:303); 0700 directory (live.rs:285) | Ask 300 s, quick 10 s (live.rs:39-42). Requests capped at 16 KB (live.rs:36,308) | Times out with the session named (live.rs:334). Locking: a fresh staging file plus non-blocking `try_lock_shared` for readers (live.rs:133-142,214). Fine. |
| GitHub raw (`github.com/timwuhaotian/killi/raw/main/releases`, which redirects once to `raw.githubusercontent.com`) | `install.sh`, `killi update` | None (public repository) | **`killi update`: `reqwest::Client::new()` with no connect, read or total timeout** (update.rs:42). Ctrl-C is handled (update.rs:34-37) | Hangs until Ctrl-C or the OS TCP timeout. A checksum mismatch fails loudly and removes the staging file (update.rs:54-58). |
| macOS helpers | Notifications: OSC 9 or `osascript` (tui/mod.rs:3603-3623). Clipboard image: `osascript` (tui/paste.rs:38-60). Copy: OSC 52 plus `pbcopy` (tui/mod.rs:3568-3580). Image shrink: `sips` (tools/files.rs:113-130). Browser for login: `open` (chatgpt.rs:541-544). Update checksum: `/usr/bin/shasum` (update.rs:98) | n/a | **None.** paste osascript (about 0.5 s), sips (images >500 KB) and the pbcopy `wait()` **block the runtime**. Notification osascript is `.spawn()`ed and **never waited** (tui/mod.rs:3623) | Silent degrade. Probably one zombie process per notification, since SIGCHLD is not ignored (only SIGTTOU, tui/mod.rs:3911-3914). Not verified empirically. |
| Terminal (escape sequences) | OSC 10/11 colour query, OSC 52, OSC 9, `?2026` synchronized output | n/a | Colour query is bounded by a 200 ms `poll` (tui/mod.rs:304-313) | Falls back to fixed palette |

Local-file integrations only (no network): `killi import-kimi` / first-run import from Kimi Code, pi, omp and opencode configs; skills; session logs. `@` mentions walk up to 20,000 entries synchronously (tui/mention.rs:10-25, called at tui/mod.rs:1379), which blocks the runtime on big trees.

### Distribution & Update Chain

**Build and publish (RELEASING.md, scripts/release.sh)**
- Built locally on the maintainer's Mac with whatever toolchain is default (1.97.1 today). The script runs clippy and the tests (release.sh:12-13), then `cargo build --release --locked` (release.sh:14).
- It copies the binary to `releases/killi-arm64`, writes `version` and `sha256` into `releases/latest.toml` (release.sh:16-20), and the result is committed and pushed **into git**.
- CI never builds the artifact. There is no signature, no provenance attestation and no reproducibility check. The trust root is the maintainer's machine plus the GitHub account.
- **67 of 95 commits** carry the ~7 MB binary. The pack is 49.4 MiB and grows about 7 MB per release (`git count-objects -vH`). No LFS, no GitHub Releases.

**What users actually get (verified)**
- Local `releases/killi-arm64` SHA-256 = `latest.toml` = the GitHub-served file (downloaded into the scratchpad) = `0465f462…afee3`.
- `codesign -dv`: `Signature=adhoc`, `flags=adhoc,linker-signed`, `TeamIdentifier=not set`, so it is not notarized. That works only because curl and reqwest don't set the quarantine attribute (decision 27). The same bypass applies to a tampered binary.
- `strings` shows `killi/0.4.20`.

**Checksum origin.** `latest.toml` and the binary come from the same repository, branch and path over the same origin (install.sh:13-14; update.rs:43,52).
- It protects against truncated or corrupted downloads and against the ~5-minute raw-CDN skew mentioned in RELEASING.md (fails closed with "checksum mismatch").
- It does **not** protect against a compromised GitHub account or repository, or the maintainer's machine: an attacker who can push the binary can push `latest.toml` too.

**TLS**
- Default URLs are https. install.sh uses system curl; `killi update` uses rustls with compiled-in roots.
- Neither enforces https on redirects or on `KILLI_RELEASES_URL`. There is no `--proto '=https'` in install.sh, and no `https_only(true)` on the update client, so an `http://` mirror is accepted.

**install.sh**
- Uses `mktemp` plus a cleanup trap (:11-12) and `curl -fsSL` (:13).
- An empty checksum is rejected (`[ -n "$expected" ]`, :16), and the binary is verified **before** `chmod` and `mv` (:16-20).
- It runs the binary only after verification (`--version`, :22). `mv` from `$TMPDIR` to `~/.local/bin` is atomic on the usual single APFS data volume.
- The `curl | sh` script has no `main()` wrapper. Truncation risk is low because it contains no destructive commands.

**`killi update` (update.rs)**
- No timeout at all (:42).
- The whole binary is collected into a `Vec` with **no size cap and no Content-Length sanity check** (:79-87). A hostile mirror can exhaust memory before the checksum runs.
- Staging file `.killi-update-<pid>` is placed next to the canonicalized exe (:50-51) and written with `fs::write` without fsync (:53).
- Verification shells out to `/usr/bin/shasum` (Perl) (:98), even though `ring::digest::SHA256` is already linked (chatgpt.rs:563).
- Then `chmod 755` and an **atomic `rename` over the running binary** (:104-105); the running process keeps its inode. Staging is removed on failure (:55-57). The write, verify and rename (:53-58) form one synchronous stretch with no await inside it, and the `select!` on `ctrl_c()` (:34-37) can only cancel at an await. So a Ctrl-C arriving during that stretch is handled only after the rename completes, and no half-installed file is left.
- **No rollback:** the old binary is not kept, and there is no `--version` pin. Downgrades are refused (`parse(&version) <= parse(current)`, :45), which also blocks rolling back through `latest.toml`.
- Version parsing splits on `.` with non-numeric parts read as 0 (:108-110). `0.10.0 > 0.9.3` is handled correctly (test at :217-220). A pre-release suffix becomes 0 (`0.5.0-rc1` = `0.5.0`), and `0.4 < 0.4.0` because of Vec prefix ordering. Both are edge cases only, since releases are plain x.y.z.
- Corner case: if `latest.toml` had `sha256 = ""` and `shasum` printed nothing to stdout, `"".eq_ignore_ascii_case("")` would pass and install an **unverified** binary (:99-102). install.sh guards against the empty value; update.rs does not.
- No automatic update discovery or notification.

**CI does not gate releases.** 12 consecutive CI runs failed from 2026-10-24 08:44 to 2026-10-25 15:10 UTC (runs 40079233681 … 40205695057), and **releases 0.4.7–0.4.13 shipped during that streak** (`gh run list`). The real gate is release.sh's local test run.

### Dependency Health Flags (outdated, deprecated, CVEs, duplicates, MSRV)

**CVE / yanked**
- tokio 1.54.0: RUSTSEC-2026-0384 and yanked. Fix: `cargo update -p tokio --precise 1.54.1`.

**Outdated (major)**
- reqwest 0.12.28 → 0.13.5. Feature map from the crates.io API shows it is **not a drop-in**:
  1. `rustls-tls` no longer exists. `rustls` now means **aws-lc-rs** plus **`rustls-platform-verifier`**: trust moves from compiled-in webpki-roots to the macOS Keychain, which would fix corporate TLS-inspection CAs (relevant to the gateways in decisions 31/48). Keeping ring requires `rustls-no-provider` and installing a provider by hand.
  2. `query` and `form`, which killi uses (tools/web.rs:484,493,506; chatgpt.rs:185,286,359), became opt-in features.
  3. reqwest 0.13.5 depends on base64 ^0.23.

**Outdated (compatible, 15 packages)** per `cargo update --dry-run --verbose`:
- tokio 1.54.1, **webpki-roots 1.0.11** (the CA root store is compiled in, so new roots arrive only with a rebuild), cc 1.10.0, icu_* 2.4.0 (7 crates), smallvec 1.16.4, tinystr 0.8.5, yoke 0.8.4, zerovec 0.11.9, zerovec-derive 0.11.7.
- Held back by semver: base64 (0.23.1), reqwest (0.13.5).

**Deprecated / unmaintained:** none flagged. ring is security-maintenance-only (withdrawn RUSTSEC-2025-0007).

**Duplicates (compiled)**
- base64 0.22.1 (killi and reqwest 0.12) plus 0.23.1 (hyper-util 0.1.21). Bumping killi alone does **not** remove the duplicate, because reqwest 0.12 needs 0.22. Moving to reqwest 0.13 *and* base64 0.23 does.
- syn 2.0.119 (derive_more-impl, phf_macros) plus syn 3.0.9 (most proc-macros): build time only, nothing in the binary.
- Lock-only duplicates: getrandom 0.2.17/0.4.3, windows-sys 0.52/0.61.

**Small or redundant use**
- base64 is used in one file, while two hand-rolled encoders exist (files.rs:136; tui/mod.rs:3584).
- ring is linked anyway but not used for the update checksum, which shells out to `shasum` instead.
- dom_smoothie brings 33 exclusive crates for one feature (readability).
- futures-util, globset and ring add nothing at the margin.

**Features**
- tokio `fs` is used but undeclared (tools/web.rs:136). It is implied by reqwest's `stream`.
- crossterm's default `windows` and regex's full Unicode are harmless.

**Dev/prod boundary**
- No dev-dependencies.
- The tests make **live network calls**: tools/web.rs:874-881 hits `api.search.brave.com` with key `k`.
- The 12-run red streak came from tests reaching Brave's live public page. CI log: `panicked at crates/killi/src/tools/web.rs:878:58` and `:915:68`, "called `Result::unwrap_err()` on an `Ok` value: [Hit { title: "A Definition & Meaning - Merriam-Webster" …". release.sh runs `cargo test` under `set -eu` (release.sh:4, :13), and releases 0.4.7–0.4.13 were built and committed during the red streak. So either those tests passed on the maintainer's machine or the script was bypassed. The test outcome depends on the network environment (the Brave page answers a CI runner with results).

**MSRV**
- `rust-version = 1.94`. **Verified consistent:** with a scratchpad toolchain (`RUSTUP_HOME`/`CARGO_HOME`/target all under the scratchpad), `cargo check --workspace --all-targets --locked` on rustc **1.94.0** passes (26 s).
- `cargo metadata`: all 173 locked crates that declare a `rust-version` declare ≤1.88 (highest: globset, ignore, icu_*); 44 declare none.
- **Risk:** three compilers are in play. CI uses `dtolnay/rust-toolchain@stable` (1.99.1 today), releases build on local 1.97.1, and 1.94 is tested nowhere. A new stable can redden CI through new clippy lints under `-D warnings` with no code change. `clippy::incompatible_msrv` catches std APIs used beyond 1.94, but not syntax or compiler-feature regressions.

**Formatting**
- CI installs `rustfmt` (ci.yml:27) but **never runs it**. There is no `rustfmt.toml`. The local 1.97.1 toolchain has no rustfmt component (`cargo fmt` errors out).
- `cargo fmt --all --check` with rustfmt 1.8.0 (the 1.94 toolchain) reports **1,286 hunks across all 39 files**, and 1,551 lines exceed 100 columns. Adding a fmt gate would require codifying the wide style first.

**CI coverage (.github/workflows/ci.yml)**
- What it runs: on push to main and on PRs, `macos-15`, 20 min; `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked`; `permissions: contents: read`.
- What it does **not** run: fmt, an MSRV job, cargo-audit/cargo-deny, a release build or artifact check, `scripts/eval.py verify`, `scripts/live_check.py`, `scripts/frames.py`, Python linting.
- Action refs are mutable tags (`actions/checkout@v7`, `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain@stable`); all exist (GitHub API 200) but are not SHA-pinned.
- No Dependabot or Renovate configuration, which is how the yanked tokio went unnoticed.
- The last 7 runs are green.

### Observations for auditors (unverified leads, with file:line)

1. **A cloned project runs code at session start with no prompt.** `.mcp.json` (command/args/env, read at mcp.rs:86-100) plus `.killi/config.toml` `[mcp] eager` (setup.rs:224-235) start the server inside `Setup::new` (setup.rs:64-75; spawn at broker.rs:42-54). This is by design (decision 11), but it is a clone-and-run exposure. Non-eager project servers can be started by the model through `mcp` (mcp.rs:225-256), for example under prompt injection.
2. **A poisoned models.dev entry could exfiltrate the OpenCode Go key.** Each model's `base_url` comes from `provider.api` (login/opencode_go.rs:131-141) and is paired with the provider key (config.rs:305-312). Suggested fix: require the `https://opencode.ai/inference/go/` prefix.
3. **Broker socket has no peer-uid check**, unlike live.rs:303. It spawns any client-supplied `Launch` (broker.rs:249-253). Protection rests on a per-user 0700 `$TMPDIR` and the umask. If `TMPDIR` is unset, `std::env::temp_dir()` is `/tmp` and the socket name is predictable (broker.rs:60-64). A squatted socket would receive each `Launch`, including MCP `env` secrets (broker.rs:74), and could feed back fake tool results.
4. **The bash tool inherits killi's whole environment** (tools/bash.rs:190-201; no `env_clear`/`env_remove` anywhere in the tools). Every model-run command sees the secrets that `api_key_env` points at.
5. **fetch_url SSRF:** no loopback, private-range or link-local filtering. Redirects are followed, and `render()` receives `final_url` (tools/web.rs:23-99).
6. **`killi update` hardening:** no timeout (update.rs:42), unbounded body (:79-87), the empty-sha corner case (:99-102), `KILLI_RELEASES_URL` accepted over http (:41), no fsync before rename (:53).
7. **Blocking calls on the single runtime thread:**
   - snapshot.rs:49-72 (`std::process` plus `thread::sleep`, up to 3 s on `index.lock`), called at tui/mod.rs:632 every turn, :137 at start and :525 on undo.
   - `refresh_branch` at tui/mod.rs:2513-2527.
   - tui/paste.rs:40 osascript; tools/files.rs:117 sips (inside the synchronous `read` tool); tui/mod.rs:3574-3578 pbcopy; tui/mention.rs:15 (≤20k-entry walk).
   - None of the subprocesses has a timeout, so a hung `git` hangs killi with Esc unable to reach it.
8. **Possible zombie processes:** the notification osascript `.spawn()` (tui/mod.rs:3623) and the broker start (broker.rs:91-98) are never waited, and SIGCHLD is not ignored (tui/mod.rs:3911-3914 only covers SIGTTOU). Not verified empirically.
9. **pdftotext** has no timeout, no `kill_on_drop` and no `own_session` (tools/web.rs:139). That contradicts decision 39's "every child gets its own session".
10. **MCP robustness:**
    - `tools/list` loops forever if a server repeats `nextCursor` (mcp.rs:265-276).
    - A request that times out leaves its `pending` entry behind (mcp.rs:427-434).
    - Stdio `lines()` has no line-length cap (mcp.rs:381-382; broker.rs:136-137, 251-252).
    - The broker attach has no timeout (broker.rs:68-88), and the `servers` lock is held across a server start of up to 60 s (broker.rs:289-296). A wedged broker hangs `mcp` calls and eager startup.
11. **LLM transport:**
    - TLS/certificate failures are retried 10 times (llm/mod.rs:184).
    - The 300 s `read_timeout` also covers time-to-first-byte, so an accepted connection that never answers can take ~52 min to fail (llm/mod.rs:124-128).
    - A `Retry-After` given as an HTTP-date is ignored (:187-192).
    - Error bodies are read uncapped (:193), and the SSE buffer grows until a newline arrives (:209-215).
12. **Corporate networks:** TLS roots are compiled in (webpki-roots), so Keychain-installed inspection CAs are not trusted, and macOS proxy settings are ignored (only env vars count). Users behind a MITM proxy would see repeated TLS retries and then a failure.
13. **Secrets over plain http:** keys go out over `http://` without a warning whenever a provider `base_url` or SearXNG `url` is http (config.rs:305-309; tools/web.rs:505-509).
14. **Brave public-page scraping with a spoofed user agent** (tools/web.rs:21, 490-503) carries ToS and fragility risk. The test suite still calls Brave's real API (tools/web.rs:874-881).
15. **PATH resolution is inconsistent:** `open`, `git`, `pbcopy` and `pdftotext` are found via PATH (chatgpt.rs:542-543; tui/mod.rs:3574; snapshot.rs:52), while osascript, sips, shasum, bash and sh use absolute paths.
16. **Per-call regex compilation** in tools/web.rs (`plain` :566-567, `html_title` :127, `brave_page` :547-551, `page_links` :610). Minor performance cost.
17. **Repository growth** from committed binaries: 67 of 95 commits, a 49.4 MiB pack. decisions.md "Status" (6.0 MB binary) is stale against the 7.26 MB binary. Decision 14's "ring and base64 were already linked" holds: killi's base64 0.22.1 is the copy reqwest uses, and ring is rustls's provider.
