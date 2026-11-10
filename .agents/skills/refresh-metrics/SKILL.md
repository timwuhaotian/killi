---
name: refresh-metrics
description: Re-measure killi's installed size, startup, memory and first-request tokens against Kimi Code and Claude Code, regenerate the docs/bench-*.svg, docs/eval-*.svg and combined docs/benchmarks-*.svg charts, and update the numbers README.md quotes. Use after a release, when Kimi Code or Claude Code updates, or when asked to refresh the benchmark charts or the size, memory and token claims.
---

# Refresh the metrics and charts

Four things come from the scripts, in this order: `scripts/bench.py` writes `docs/bench.json`,
`scripts/chart.py` draws `docs/bench-{light,dark}.svg`, `docs/eval-{light,dark}.svg` and
`docs/benchmarks-{light,dark}.svg` (both charts stacked in one image — the one at the top of
the README, right under the screenshot), `scripts/eval.py` runs the capability tasks, and
`scripts/eval.py report` writes `docs/eval.{md,json}` and redraws the eval and combined charts.
The README quotes numbers from all of them, so it is the last thing to edit.

## 0. Before measuring

- Apple silicon Mac, `kimi` and `claude` installed and signed in, and `mimo/mimo-v2.6-pro`
  configured in `~/.killi/config.toml` (`BENCH_MODEL` and `EVAL_MODEL` change it).
- opencode 2 is optional for `bench.py`: `OPENCODE_BIN` is the compiled binary, e.g. `npm install
  --prefix <dir> --ignore-scripts @opencode/cli` and
  `<dir>/node_modules/@opencode/cli-darwin-arm64/bin/opencode` (the npm launcher would hide the
  real process's memory). `bench.py` builds its config from killi's provider, runs it with
  `--standalone` (one process, no background service) and reads tokens from its sqlite database,
  because `--format json` carries no usage.
- Measure the binary people install on your host: `releases/killi-arm64` (macOS arm64) or
  `releases/killi-linux-x64` / `releases/killi-linux-arm64` (Linux), built by
  `scripts/release.sh` (or `cargo build --release`). `~/.local/bin/killi` can be a release
  behind. Pass it as `KILLI_BIN="$PWD/releases/killi-arm64"` (the `killi-linux-*` file on Linux).
- Look at `uptime`. Other agents and test suites on the machine raise the load; startup time
  jitters with it (0.2.16 measured 38, 13 and 17 ms in three runs, and 8 ms when timed directly).

## 1. Benchmark

```sh
KILLI_BIN="$PWD/releases/killi-arm64" python3 scripts/bench.py  # or killi-linux-* on Linux
```

`--tools killi,opencode` measures only those and keeps the other rows (each row records the date
it was measured). It runs each tool in an empty folder: startup (`--version`, median of 5), peak memory (maximum
resident set size of a one-prompt run, median of 3) and the first request's input tokens. Run
it **at least twice**. Size, memory and tokens agree within a few percent between runs; if
they don't, something else was running. Quote startup as a round "under N ms" unless the load
was low, and never as a single-digit figure from a loaded machine. killi runs with
`KILLI_NO_HOOKS=1` (bench.py sets it): hooks are the user's own programs in other
processes, and `time -l` counts the whole tree's peak.

Things that change between runs for reasons that aren't killi:
- Claude Code runs its *default* model; `docs/bench.json` records which. If it changed
  (it went from Opus 5.5 to Sonnet 5.5 when the default did), add the display name to `MODELS`
  in `scripts/chart.py`, and say so in the README: its token count moved with the model.
- "Tokens sent before you type" include this machine's skills, `AGENTS.md` files and MCP
  servers, so they move when those do.

## 2. Charts

```sh
python3 scripts/chart.py
rsvg-convert -w 960 docs/benchmarks-light.svg -o "$TMPDIR/benchmarks.png"   # a PNG of the whole combined chart to look at (qlmanage crops to a square)
```

The footnote names the chip and macOS from `bench.json` (`machine`), so it is always the
machine that measured.

## 3. Capability tests (costs tokens: decide first)

- `python3 scripts/eval.py verify` checks every task fails as given and passes with its solution.
- killi alone, about 10 minutes and about 28k input tokens per task on MiMo:
  `KILLI_BIN="$PWD/releases/killi-arm64" python3 scripts/eval.py run --tools killi` (the `killi-linux-*` file on Linux)
- Kimi Code and Claude Code too: about 207k and 279k input tokens per task, so roughly 3M
  tokens of the MiMo plan for Kimi Code and a day's worth of Claude usage. Ask first.
- `python3 scripts/eval.py report` merges the **latest run of each tool**. After a killi-only run
  the other two rows are older (their versions are in `docs/eval.md`); say so when reporting.
  It also adds the run to the "killi across versions" table.

Memory in the eval is the tool's own process per task, not the compilers it starts.

## 4. README

Update by hand, from `docs/bench.json` and `docs/eval.json`:
- "Small and quick": size, startup, memory running a prompt, and the other two tools' memory.
- "Few tokens": the N× fewer input tokens, which is Kimi Code's `avg_input` over killi's.
- "Size and memory": which models and versions each tool ran.
- The image alt texts if the headline changed.

`docs/screenshot.svg`, `docs/menu.svg` and `docs/theme.svg` are drawn by `scripts/frames.py`
(the `readme`, `slash` and `skin` scenes): a scripted provider answers, so no model, account
or clean HOME is needed and the shots come out the same every time. They wear the
`iced-coffee` theme — the palette `docs/icon.svg` is drawn from; `docs/icon.png` is that SVG
rendered at 512 (`rsvg-convert -w 512`). Redraw the shots when the UI changes, against the
release binary for your host so the banner's version is the one being shipped (`killi-linux-*` on
Linux): the hero and the menu run inline at a tall size, the theme preview fullscreen where its
specimen fits whole:

```sh
scripts/frames.py /tmp/shots --killi releases/killi-arm64 --sizes 118x64 \
    --scenes readme slash --inline --ui-theme iced-coffee --frame
scripts/frames.py /tmp/shots --killi releases/killi-arm64 --sizes 118x36 \
    --scenes skin --ui-theme iced-coffee --frame
cp /tmp/shots/readme-118x64-night.svg docs/screenshot.svg
cp /tmp/shots/slash-118x64-night.svg docs/menu.svg
cp /tmp/shots/skin-118x36-night.svg docs/theme.svg
```

Look at PNGs of all three (`rsvg-convert -w 1000`) before committing. `scripts/screenshot.py`
remains for shooting a real model's run.

## Another number people ask for

Memory of the interactive UI at rest (default state, nothing typed): start each tool in a pty
in an empty folder, wait about 10 s, and read `ps -o rss=` for its pid and its children. On
0.2.16 that was killi 6.9 MB, Kimi Code 319 MB (471 MB with children) and Claude Code 200 MB;
it isn't in `bench.py` because the README's claim is memory running a prompt.

## Commit

`docs/bench.json`, `docs/bench-*.svg`, `docs/benchmarks-*.svg`, `docs/eval.*`, `evals/results.jsonl`, `README.md` and any
script change. `evals/runs/` is ignored. These aren't part of a release; for that see
`RELEASING.md`.
