# Capability tests

14 coding tasks, each run through killi, Kimi Code and Claude Code with `scripts/eval.py`.
A task passes when its check (the project's tests, clippy, or an exact answer) succeeds after the
tool finishes; the tests are restored first, so editing them doesn't help.

| | killi | Kimi Code | Claude Code |
|---|---|---|---|
| Version | 0.2.16 | 2.2.0 | 2.1.306 |
| Model | mimo-v2.6-pro | mimo-v2.6-pro | claude-opus-5-5 |
| Passed | 14 / 14 | 14 / 14 | 14 / 14 |
| Input tokens per task | 36,768 | 206,767 | 278,879 |
| … not from cache | 6,614 | 24,038 | 41,849 |
| Output tokens per task | 2,879 | 2,552 | 3,115 |
| Requests per task | 6.4 | 4.9 | 6.4 |
| Time per task | 80 s | 60 s | 48 s |
| Peak memory | 5 MB | 335 MB | 191 MB |

## Per task

Pass (✓) or fail (✗), input tokens, and time.

| Task | killi | Kimi Code | Claude Code |
|---|---|---|---|
| `rust-borrow-checker` | ✓ 24k · 50 s | ✓ 207k · 47 s | ✓ 247k · 35 s |
| `rust-clippy-clean` | ✓ 27k · 36 s | ✓ 396k · 104 s | ✓ 199k · 34 s |
| `rust-config-refactor` | ✓ 34k · 41 s | ✓ 246k · 41 s | ✓ 351k · 48 s |
| `rust-json-no-deps` | ✓ 60k · 152 s | ✓ 205k · 59 s | ✓ 364k · 73 s |
| `rust-long-file` | ✓ 70k · 50 s | ✓ 211k · 29 s | ✓ 248k · 25 s |
| `rust-parse-duration` | ✓ 29k · 109 s | ✓ 174k · 142 s | ✓ 201k · 50 s |
| `rust-ring-buffer` | ✓ 21k · 21 s | ✓ 163k · 28 s | ✓ 197k · 32 s |
| `ts-async-queue` | ✓ 33k · 114 s | ✓ 208k · 47 s | ✓ 457k · 104 s |
| `ts-csv-parser` | ✓ 73k · 257 s | ✓ 212k · 94 s | ✓ 246k · 64 s |
| `ts-lru-cache` | ✓ 21k · 30 s | ✓ 163k · 27 s | ✓ 300k · 41 s |
| `ts-retry-bugs` | ✓ 22k · 44 s | ✓ 164k · 37 s | ✓ 245k · 41 s |
| `ts-scoped-fix` | ✓ 29k · 106 s | ✓ 174k · 120 s | ✓ 309k · 59 s |
| `ts-task-priority` | ✓ 42k · 78 s | ✓ 209k · 41 s | ✓ 298k · 35 s |
| `ts-trace-locale` | ✓ 30k · 39 s | ✓ 162k · 19 s | ✓ 242k · 24 s |

## killi across versions

Every run of killi, by version, on the tasks that version ran.

| Version | Passed | Input tokens per task | … not from cache | Requests per task | Time per task |
|---|---|---|---|---|---|
| 0.1.1 | 10 / 10 | 17,068 | 3,769 | 5.6 | 46 s |
| 0.2.0 | 14 / 14 | 32,856 | 6,259 | 5.7 | 52 s |
| 0.2.1 | 14 / 14 | 27,579 | 4,914 | 5.6 | 48 s |
| 0.2.16 | 14 / 14 | 36,768 | 6,614 | 6.4 | 80 s |

Peak memory is the tool's own process (macOS peak memory footprint), not the compilers or
test runners it starts. Claude Code's tokens include this machine's skills, plugins and MCP
servers, as configured; so do Kimi Code's.
