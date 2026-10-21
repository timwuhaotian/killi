# Capability tests

Ten coding tasks, each run through killi, Kimi Code and Claude Code with `scripts/eval.py`.
A task passes when its check (the project's tests, clippy, or an exact answer) succeeds after the
tool finishes; the tests are restored first, so editing them doesn't help.

| | killi | Kimi Code | Claude Code |
|---|---|---|---|
| Model | mimo-v2.6-pro | mimo-v2.6-pro | claude-opus-5-5 |
| Passed | 10 / 10 | 10 / 10 | 10 / 10 |
| Input tokens per task | 17,068 | 209,634 | 252,606 |
| … not from cache | 3,769 | 23,586 | 41,222 |
| Output tokens per task | 1,825 | 2,339 | 2,836 |
| Requests per task | 5.6 | 5.0 | 6.1 |
| Time per task | 46 s | 58 s | 41 s |
| Peak memory | 4 MB | 336 MB | 186 MB |

## Per task

Pass (✓) or fail (✗), input tokens, and time.

| Task | killi | Kimi Code | Claude Code |
|---|---|---|---|
| `rust-borrow-checker` | ✓ 13k · 24 s | ✓ 207k · 47 s | ✓ 247k · 35 s |
| `rust-clippy-clean` | ✓ 24k · 45 s | ✓ 396k · 104 s | ✓ 199k · 34 s |
| `rust-config-refactor` | ✓ 14k · 22 s | ✓ 246k · 41 s | ✓ 351k · 48 s |
| `rust-parse-duration` | ✓ 17k · 93 s | ✓ 174k · 142 s | ✓ 201k · 50 s |
| `rust-ring-buffer` | ✓ 11k · 20 s | ✓ 163k · 28 s | ✓ 197k · 32 s |
| `ts-csv-parser` | ✓ 41k · 151 s | ✓ 212k · 94 s | ✓ 246k · 64 s |
| `ts-lru-cache` | ✓ 17k · 31 s | ✓ 163k · 27 s | ✓ 300k · 41 s |
| `ts-retry-bugs` | ✓ 12k · 30 s | ✓ 164k · 37 s | ✓ 245k · 41 s |
| `ts-task-priority` | ✓ 11k · 25 s | ✓ 209k · 41 s | ✓ 298k · 35 s |
| `ts-trace-locale` | ✓ 10k · 14 s | ✓ 162k · 19 s | ✓ 242k · 24 s |

Peak memory is the tool's own process (macOS peak memory footprint), not the compilers or
test runners it starts. Claude Code's tokens include this machine's skills, plugins and MCP
servers, as configured; so do Kimi Code's.
