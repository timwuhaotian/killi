#!/usr/bin/env python3
"""Draws the README's screenshots by running killi in a pseudo-terminal.

    scripts/screenshot.py --project ~/code/slug --model minimax/MiniMax-M3.1-Flash \
        --prompt "The slugify test fails. Fix it and run the tests." docs/screenshot.svg
    scripts/screenshot.py --project ~/code/slug --type /re docs/menu.svg

The first runs a whole turn and draws the conversation; the second types into the prompt and
draws the screen as it stands. Both need `pyte` (pip install pyte) and the release binary
(`cargo build --release -p killi`, or `--killi`). The drawing is an SVG in the design system's
colors (docs/killi-design-system.html) inside a window frame; it keeps every cell's color,
bold and dim, so it shows exactly what the terminal showed.
"""

import argparse
import fcntl
import os
import pty
import select
import struct
import sys
import termios
import time
from pathlib import Path

import pyte

COLS, ROWS = 118, 160
FONT_SIZE, ROW_HEIGHT = 13, 20
CELL = 7.8
PAD, BAR = 16, 36
# The dark theme's tokens.
BG, FG, MUTED = "#0b0f14", "#e8eef2", "#8e9cab"
ANSI = {
    "black": "#0b0f14", "red": "#ff6f7d", "green": "#4fd1a0", "brown": "#f2c14e", "yellow": "#f2c14e",
    "blue": "#5fafff", "magenta": "#d787d7", "cyan": "#4fd1c5", "white": "#e8eef2",
    "brightblack": "#5c6b7a", "brightred": "#ff6f7d", "brightgreen": "#4fd1a0", "brightyellow": "#f2c14e",
    "brightblue": "#5fafff", "brightmagenta": "#d787d7", "brightcyan": "#4fd1c5", "brightwhite": "#ffffff",
}


def run(args):
    """Runs killi in a pty, feeds everything it prints to a terminal emulator, and returns the
    screen once the turn has ended or the typed text is in the prompt."""
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    command = [args.killi, "-m", args.model] if args.model else [args.killi]
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        env = {k: v for k, v in os.environ.items() if k != "COLORFGBG"}
        env.update(COLORTERM="truecolor", TERM="xterm-256color")
        if args.home:
            env["HOME"] = args.home
        os.chdir(args.project)
        os.execvpe(command[0], command, env)

    def pump(seconds):
        """Feeds output to the screen for `seconds`; true when anything arrived."""
        got = False
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            ready, _, _ = select.select([fd], [], [], min(left, 0.1))
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            # pyte has no dim; killi only uses blink's code for nothing, so dim rides on it.
            stream.feed(data.replace(b"\x1b[2m", b"\x1b[5m"))
            got = True
        return got

    for _ in range(100):
        pump(0.2)
        if "ask, or /" in text(screen):
            break
    else:
        sys.exit("killi showed no prompt within 20 s:\n" + text(screen).rstrip())
    pump(0.5)
    if args.prompt:
        os.write(fd, args.prompt.encode() + b"\r")
        start = time.monotonic()
        while time.monotonic() - start < args.timeout:
            if not pump(3) and "Esc to interrupt" not in text(screen):
                break
    if args.type:
        os.write(fd, args.type.encode())
        pump(1)
    rows = [row for row in cells(screen)]
    os.write(fd, b"\x1b")
    pump(0.2)
    os.write(fd, b"/exit\r")
    pump(1)
    os.close(fd)
    return rows


def text(screen):
    return "\n".join(screen.display)


def cells(screen):
    """The screen's rows as lists of (char, fg, bg, bold, dim), trailing blank rows dropped."""
    rows = []
    for y in range(screen.lines):
        line = screen.buffer[y]
        rows.append([(c.data, c.fg, c.bg, c.bold, c.blink) for c in (line[x] for x in range(screen.columns))])
    while rows and all(ch in (" ", "") and bg == "default" for ch, _, bg, _, _ in rows[-1]):
        rows.pop()
    return rows


def color(name, default):
    if name == "default":
        return default
    if name in ANSI:
        return ANSI[name]
    return f"#{name}" if len(name) == 6 else default


def dimmed(hex_color):
    """A color at two thirds of the way from the background: what dim does to it."""
    r, g, b = (int(hex_color[i : i + 2], 16) for i in (1, 3, 5))
    br, bg_, bb = (int(BG[i : i + 2], 16) for i in (1, 3, 5))
    mix = lambda a, b: round(a * 0.6 + b * 0.4)
    return f"#{mix(r, br):02x}{mix(g, bg_):02x}{mix(b, bb):02x}"


def escape(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def svg(rows, title):
    width = round(COLS * CELL + 2 * PAD)
    height = BAR + len(rows) * ROW_HEIGHT + 2 * PAD
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="JetBrains Mono, SF Mono, Menlo, Consolas, Liberation Mono, monospace" font-size="{FONT_SIZE}">',
        f'<rect width="{width}" height="{height}" rx="10" fill="{BG}"/>',
        f'<rect width="{width}" height="{BAR}" rx="10" fill="#1a2129"/><rect y="{BAR - 10}" width="{width}" height="10" fill="#1a2129"/>',
        f'<circle cx="20" cy="{BAR / 2}" r="6" fill="#ff5f57"/><circle cx="40" cy="{BAR / 2}" r="6" fill="#febc2e"/><circle cx="60" cy="{BAR / 2}" r="6" fill="#28c840"/>',
        f'<text x="{width / 2}" y="{BAR / 2 + 4.5}" text-anchor="middle" fill="{MUTED}" font-family="-apple-system, Helvetica Neue, Arial, sans-serif" font-size="13">{escape(title)}</text>',
    ]
    for y, row in enumerate(rows):
        top = BAR + PAD + y * ROW_HEIGHT
        x = 0
        while x < COLS:
            ch, fg, bg, bold, dim = row[x]
            if ch == "":  # the second cell of a wide character
                x += 1
                continue
            run = [ch]
            end = x + 1
            while end < COLS and row[end][1:] == (fg, bg, bold, dim) and row[end][0] != "":
                run.append(row[end][0])
                end += 1
            start, x = x, end
            txt = "".join(run)
            if txt.strip() == "" and bg == "default":
                continue
            if bg != "default":
                out.append(f'<rect x="{PAD + start * CELL:.1f}" y="{top}" width="{(end - start) * CELL:.1f}" height="{ROW_HEIGHT}" fill="{color(bg, BG)}"/>')
            if txt.strip() == "":
                continue
            fill = color(fg, FG)
            if dim:
                fill = MUTED if fg == "default" else dimmed(fill)
            if set(txt) == {"█"}:
                # A full block as a rect: as text, fonts leave hairlines between cells.
                out.append(f'<rect x="{PAD + start * CELL:.1f}" y="{top}" width="{(end - start) * CELL:.1f}" height="{ROW_HEIGHT}" fill="{fill}"/>')
                continue
            weight = ' font-weight="700"' if bold else ""
            out.append(
                f'<text x="{PAD + start * CELL:.1f}" y="{top + 14.5}" textLength="{(end - start) * CELL:.1f}" lengthAdjust="spacingAndGlyphs" fill="{fill}"{weight} xml:space="preserve">{escape(txt)}</text>'
            )
    out.append("</svg>")
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path, help="the SVG to write")
    parser.add_argument("--project", required=True, help="the folder killi runs in")
    parser.add_argument("--model", help="the model alias to run on")
    parser.add_argument("--prompt", help="a message to send, drawing the conversation once the turn ends")
    parser.add_argument("--type", help="text to type into the prompt, drawing the screen as it stands")
    parser.add_argument("--timeout", type=float, default=300, help="seconds to wait for the turn")
    parser.add_argument("--killi", default=str(Path(__file__).resolve().parent.parent / "target/release/killi"))
    parser.add_argument("--home", help="a HOME for killi, to show a clean config instead of this machine's")
    parser.add_argument("--title", help="the window title; the folder and killi by default")
    args = parser.parse_args()
    project = Path(args.project).expanduser().resolve()
    args.project = str(project)
    home = Path(args.home).expanduser().resolve() if args.home else Path.home()
    shown = f"~/{project.relative_to(home)}" if project.is_relative_to(home) else str(project)
    rows = run(args)
    args.out.write_text(svg(rows, args.title or f"{shown} — killi"))
    print(f"{args.out}: {len(rows)} rows", file=sys.stderr)


if __name__ == "__main__":
    main()
