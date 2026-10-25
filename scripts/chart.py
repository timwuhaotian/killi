#!/usr/bin/env python3
"""Draws docs/bench-{light,dark}.svg from docs/bench.json, docs/eval-{light,dark}.svg from

docs/eval.json, and docs/benchmarks-{light,dark}.svg with both charts in one image."""

import json
from html import escape
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
THEMES = {
    "light": dict(bg="#ffffff", border="#e5e7eb", text="#111827", sub="#4b5563", muted="#6b7280",
                  accent="#2563eb", highlight="#eff4ff", bar="#d1d5db"),
    "dark": dict(bg="#0d1117", border="#30363d", text="#f0f6fc", sub="#9ea7b3", muted="#7d8590",
                 accent="#5b9bff", highlight="#13213a", bar="#3a414b"),
}
MODELS = {"mimo-v2.6-pro": "MiMo-V2.6-Pro", "claude-opus-5-5": "Claude Opus 5.5", "claude-sonnet-5-5": "Claude Sonnet 5.5"}
WIDTH, LABEL_X, BAR_X, BAR_MAX, ROW = 960, 182, 194, 560, 40


def mb(value):
    return "N/A" if value is None else f"{value:.1f} MB"


def tokens(value):
    return f"{value:,} tokens"


def times(value, base):
    ratio = value / base
    return f"{ratio:.0f}×" if ratio >= 10 else f"{ratio:.1f}×"


PANELS = [
    ("size_mb", "Installed size", mb, "runtime"),
    ("memory_mb", "Peak memory running one prompt", mb, "runtime"),
    ("tokens", "Tokens sent before you type", tokens, "model"),
]


def wrap(lines, height, c, label):
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" '
            f'font-family="-apple-system, BlinkMacSystemFont, \'Segoe UI\', Helvetica, Arial, sans-serif" role="img" aria-label="{escape(label)}">\n'
            f'<rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="12" fill="{c["bg"]}" stroke="{c["border"]}"/>')
    return "\n".join([head, *lines, "</svg>\n"])


def bench_body(data, c):
    tools = data["tools"]
    killi, kimi = tools[0], tools[1]
    out = []
    y = 58
    out.append(f'<text x="32" y="{y}" font-size="26" font-weight="700" fill="{c["text"]}">killi is '
               f'<tspan fill="{c["accent"]}">{mb(killi["size_mb"])}</tspan> and runs in '
               f'<tspan fill="{c["accent"]}">{mb(killi["memory_mb"])}</tspan>.</text>')
    out.append(f'<text x="32" y="{y + 28}" font-size="15" fill="{c["sub"]}">Against Kimi Code on the same model: '
               f'{times(kimi["size_mb"], killi["size_mb"])} smaller, {times(kimi["memory_mb"], killi["memory_mb"])} less memory, '
               f'{times(kimi["tokens"], killi["tokens"])} fewer tokens before you type.</text>')
    y = 124
    for key, title, fmt, sublabel in PANELS:
        out.append(f'<text x="32" y="{y}" font-size="13" font-weight="700" fill="{c["muted"]}" letter-spacing="0.4">{escape(title.upper())}</text>')
        y += 10
        top = max(t[key] for t in tools)
        for tool in tools:
            first = tool is killi
            if first:
                out.append(f'<rect x="20" y="{y}" width="920" height="{ROW - 2}" rx="8" fill="{c["highlight"]}"/>')
            weight = 700 if first else 500
            note = MODELS.get(tool["model"], tool["model"]) if sublabel == "model" else tool["runtime"]
            out.append(f'<text x="{LABEL_X}" y="{y + 17}" font-size="15" font-weight="{weight}" fill="{c["text"]}" text-anchor="end">{escape(tool["name"])}</text>')
            out.append(f'<text x="{LABEL_X}" y="{y + 32}" font-size="11" fill="{c["muted"]}" text-anchor="end">{escape(note)}</text>')
            width = max(6.0, tool[key] / top * BAR_MAX)
            out.append(f'<rect x="{BAR_X}" y="{y + 12}" width="{width:.1f}" height="14" rx="4" fill="{c["accent"] if first else c["bar"]}"/>')
            extra = "" if first else f'<tspan dx="8" font-size="12" font-weight="400" fill="{c["muted"]}">· {times(tool[key], killi[key])} killi</tspan>'
            out.append(f'<text x="{BAR_X + width + 10:.1f}" y="{y + 24}" font-size="14" font-weight="{weight}" fill="{c["text"]}">{escape(fmt(tool[key]))}{extra}</text>')
            y += ROW
        y += 34
    when = " and ".join(sorted({t.get("measured", data["measured"]) for t in tools}))
    notes = [
        f'Measured {when} on {data["machine"]} with scripts/bench.py, prompt “{data["prompt"]}” in an empty folder.',
        "Size is the installed binary (Kimi Code’s includes its bundled rg and fd). Memory is the median maximum resident set size of 3 runs.",
        "Tokens are the first request’s input as each tool reports it, with this machine’s skills, instructions and MCP servers loaded;",
        "opencode 2 ran with a fresh config holding only the model, so it loaded this machine’s skills but none of its MCP servers.",
        "By default opencode 2 uses a background service; it was measured with --standalone, so everything is in one process.",
    ]
    y -= 8
    for note in notes:
        out.append(f'<text x="32" y="{y}" font-size="12" fill="{c["muted"]}">{escape(note)}</text>')
        y += 18
    label = "; ".join(f'{t["name"]} {mb(t["size_mb"])} installed, {mb(t["memory_mb"])} memory, {tokens(t["tokens"])}' for t in tools)
    return out, y + 6, label


def render(data, c):
    lines, height, label = bench_body(data, c)
    return wrap(lines, height, c, label)


def kilo(value):
    if value is None:
        return "N/A"
    return f"{value / 1000:,.1f}k" if value >= 1000 else f"{value:.0f}"


EVAL_PANELS = [
    ("passed", "Runs passed", lambda s: f'{s["passed"]} of {s["runs"]}', False),
    ("avg_input", "Input tokens per run", lambda s: kilo(s["avg_input"]), True),
    ("avg_wall_s", "Time per run", lambda s: "N/A" if s["avg_wall_s"] is None else f'{s["avg_wall_s"]:.0f} s', False),
    ("avg_memory_mb", "Peak memory", lambda s: mb(s["avg_memory_mb"]), True),
]


def eval_body(summary, c):
    tools = [summary[k] for k in ("killi", "kimi", "claude") if k in summary]
    killi = tools[0]
    others = " · ".join(f'{t["name"]}: {t["passed"]} of {t["runs"]}, {kilo(t["avg_input"])} tokens' for t in tools[1:])
    out = [
        f'<text x="32" y="58" font-size="26" font-weight="700" fill="{c["text"]}">killi passed '
        f'<tspan fill="{c["accent"]}">{killi["passed"]} of {killi["runs"]}</tspan> runs with '
        f'<tspan fill="{c["accent"]}">{kilo(killi["avg_input"])}</tspan> input tokens per run.</text>',
        f'<text x="32" y="86" font-size="15" fill="{c["sub"]}">{escape(others)}.</text>',
    ]
    y = 124
    for key, title, fmt, show_ratio in EVAL_PANELS:
        out.append(f'<text x="32" y="{y}" font-size="13" font-weight="700" fill="{c["muted"]}" letter-spacing="0.4">{escape(title.upper())}</text>')
        y += 10
        top = max((t[key] or 0 for t in tools), default=0) or 1
        for tool in tools:
            first = tool is killi
            if first:
                out.append(f'<rect x="20" y="{y}" width="920" height="{ROW - 2}" rx="8" fill="{c["highlight"]}"/>')
            weight = 700 if first else 500
            model = MODELS.get(tool["model"], tool["model"])
            out.append(f'<text x="{LABEL_X}" y="{y + 17}" font-size="15" font-weight="{weight}" fill="{c["text"]}" text-anchor="end">{escape(tool["name"])}</text>')
            out.append(f'<text x="{LABEL_X}" y="{y + 32}" font-size="11" fill="{c["muted"]}" text-anchor="end">{escape(model)}</text>')
            width = max(6.0, tool[key] / top * BAR_MAX) if tool[key] is not None else 0
            if tool[key] is not None:
                out.append(f'<rect x="{BAR_X}" y="{y + 12}" width="{width:.1f}" height="14" rx="4" fill="{c["accent"] if first else c["bar"]}"/>')
            extra = ""
            if show_ratio and not first and killi[key] and tool[key] is not None:
                extra = f'<tspan dx="8" font-size="12" font-weight="400" fill="{c["muted"]}">· {times(tool[key], killi[key])} killi</tspan>'
            out.append(f'<text x="{BAR_X + width + 10:.1f}" y="{y + 24}" font-size="14" font-weight="{weight}" fill="{c["text"]}">{escape(fmt(tool))}{extra}</text>')
            y += ROW
        y += 34
    notes = [
        f"Every repeat of each tool's latest version and model is included ({killi['runs']} killi runs); performance means use valid records.",
        "A task passes when its check succeeds after the tool finishes, with tests restored first. Peak memory excludes child processes.",
    ]
    y -= 8
    for note in notes:
        out.append(f'<text x="32" y="{y}" font-size="12" fill="{c["muted"]}">{escape(note)}</text>')
        y += 18
    label = "; ".join(f'{t["name"]}: {t["passed"]} of {t["runs"]} passed, {kilo(t["avg_input"])} input tokens per run' for t in tools)
    return out, y + 6, label


def render_eval_svg(summary, c):
    lines, height, label = eval_body(summary, c)
    return wrap(lines, height, c, label)


def render_combined(data, summary, c):
    """The size/memory chart and the capability chart stacked in one image, for the README's top."""
    bench, bench_h, bench_label = bench_body(data, c)
    ev, eval_h, eval_label = eval_body(summary, c)
    lines = [
        "<g>", *bench, "</g>",
        f'<line x1="20" y1="{bench_h + 8}" x2="940" y2="{bench_h + 8}" stroke="{c["border"]}"/>',
        f'<g transform="translate(0,{bench_h + 16})">', *ev, "</g>",
    ]
    return wrap(lines, bench_h + 16 + eval_h, c, f"{bench_label}. Capability tests: {eval_label}")


def render_eval(summary):
    data = json.loads((ROOT / "docs/bench.json").read_text())
    for name, colors in THEMES.items():
        (ROOT / f"docs/eval-{name}.svg").write_text(render_eval_svg(summary, colors))
        (ROOT / f"docs/benchmarks-{name}.svg").write_text(render_combined(data, summary, colors))


def main():
    data = json.loads((ROOT / "docs/bench.json").read_text())
    summary = json.loads((ROOT / "docs/eval.json").read_text())["summary"]
    for name, colors in THEMES.items():
        (ROOT / f"docs/bench-{name}.svg").write_text(render(data, colors))
    render_eval(summary)
    print("wrote docs/bench-{light,dark}.svg, docs/eval-{light,dark}.svg and docs/benchmarks-{light,dark}.svg")


if __name__ == "__main__":
    main()
