# Capability tests

14 coding tasks, each run through killi, Kimi Code and Claude Code with `scripts/eval.py`.
A task passes when its check (the project's tests, clippy, or an exact answer) succeeds after the
tool finishes; the tests are restored first, so editing them doesn't help.
The main report includes every repeat of each tool's latest version and model configuration.
Pass rates include all runs; performance means use only records with complete, valid metrics.

| | killi | Kimi Code | Claude Code |
|---|---|---|---|
| Version | 0.6.55 | 2.2.0 | 2.1.306 |
| Model | mimo-v2.6-pro | mimo-v2.6-pro | claude-opus-5-5 |
| Passed | 14 / 14 | 14 / 14 | 14 / 14 |
| Valid metrics | 14 / 14 | 14 / 14 | 14 / 14 |
| Input tokens per run | 36,628 | 206,767 | 278,879 |
| … not from cache | 5,835 | 24,038 | 41,849 |
| Output tokens per run | 1,903 | 2,552 | 3,115 |
| Requests per run | 6.1 | 4.9 | 6.4 |
| Time per run | 47 s | 60 s | 48 s |
| Peak memory | 7 MB | 335 MB | 191 MB |

## Per task

Passed runs, mean input tokens, and mean time over records with valid metrics.

| Task | killi | Kimi Code | Claude Code |
|---|---|---|---|
| `rust-borrow-checker` | 1 / 1 passed · 22k · 18 s | 1 / 1 passed · 207k · 47 s | 1 / 1 passed · 247k · 35 s |
| `rust-clippy-clean` | 1 / 1 passed · 40k · 45 s | 1 / 1 passed · 396k · 104 s | 1 / 1 passed · 199k · 34 s |
| `rust-config-refactor` | 1 / 1 passed · 33k · 18 s | 1 / 1 passed · 246k · 41 s | 1 / 1 passed · 351k · 48 s |
| `rust-json-no-deps` | 1 / 1 passed · 40k · 53 s | 1 / 1 passed · 205k · 59 s | 1 / 1 passed · 364k · 73 s |
| `rust-long-file` | 1 / 1 passed · 34k · 43 s | 1 / 1 passed · 211k · 29 s | 1 / 1 passed · 248k · 25 s |
| `rust-parse-duration` | 1 / 1 passed · 29k · 54 s | 1 / 1 passed · 174k · 142 s | 1 / 1 passed · 201k · 50 s |
| `rust-ring-buffer` | 1 / 1 passed · 26k · 15 s | 1 / 1 passed · 163k · 28 s | 1 / 1 passed · 197k · 32 s |
| `ts-async-queue` | 1 / 1 passed · 35k · 98 s | 1 / 1 passed · 208k · 47 s | 1 / 1 passed · 457k · 104 s |
| `ts-csv-parser` | 1 / 1 passed · 26k · 35 s | 1 / 1 passed · 212k · 94 s | 1 / 1 passed · 246k · 64 s |
| `ts-lru-cache` | 1 / 1 passed · 27k · 28 s | 1 / 1 passed · 163k · 27 s | 1 / 1 passed · 300k · 41 s |
| `ts-retry-bugs` | 1 / 1 passed · 45k · 25 s | 1 / 1 passed · 164k · 37 s | 1 / 1 passed · 245k · 41 s |
| `ts-scoped-fix` | 1 / 1 passed · 60k · 139 s | 1 / 1 passed · 174k · 120 s | 1 / 1 passed · 309k · 59 s |
| `ts-task-priority` | 1 / 1 passed · 36k · 44 s | 1 / 1 passed · 209k · 41 s | 1 / 1 passed · 298k · 35 s |
| `ts-trace-locale` | 1 / 1 passed · 58k · 42 s | 1 / 1 passed · 162k · 19 s | 1 / 1 passed · 242k · 24 s |

## killi across versions

Every run of killi, by version, on the tasks that version ran.

| Version | Passed | Input tokens per task | … not from cache | Requests per task | Time per task |
|---|---|---|---|---|---|
| 0.1.1 | 10 / 10 | 17,068 | 3,769 | 5.6 | 46 s |
| 0.2.0 | 14 / 14 | 32,856 | 6,259 | 5.7 | 52 s |
| 0.2.1 | 14 / 14 | 27,579 | 4,914 | 5.6 | 48 s |
| 0.2.16 | 14 / 14 | 36,768 | 6,614 | 6.4 | 80 s |
| 0.6.55 | 14 / 14 | 36,628 | 5,835 | 6.1 | 47 s |

Peak memory is the tool's own process (macOS peak memory footprint), not the compilers or
test runners it starts. Claude Code's tokens include this machine's skills, plugins and MCP
servers, as configured; so do Kimi Code's.
