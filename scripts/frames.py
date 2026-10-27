#!/usr/bin/env python3
"""Draws killi's screens at any size and on any terminal theme, with no model and none of your
config: a scripted provider (fake_llm.py) answers, killi runs in a pseudo-terminal under a
scratch HOME, and each screen is written as an SVG painted on the theme's own palette.

    scripts/frames.py out/
    scripts/frames.py out/ --sizes 118x40 80x24 --themes night paper --scenes conv menu --png
    scripts/frames.py out/ --killi target/debug/killi --inline --colors 256

The themes are Kil's five (crates/kil/src/theme.rs in the Kil repo): killi asks the terminal
for its background and foreground, and the pseudo-terminal answers with the theme's. SGR dim is
drawn as Kil draws it, each channel scaled to two thirds. Scenes: start, menu, files (the `@`
menu), picker, conv (a bug fix with a diff), question, plan, busy (mid-turn), swarm and swarm-big (an
`agent_swarm` of 5 and of 40 subagents, mid-run), swarm-done (the same five, finished), peers (the banner
with another session in the repository), sessions (`/sessions`) and peer (that session's question and
message arriving through `killi ask` and `killi tell`). `--inline` runs killi as it runs outside
Kil, in the terminal's scrollback; `--colors 256` hides COLORTERM; `--no-osc` never answers the
color queries, like a terminal that doesn't know them; `--no-color` sets NO_COLOR.

Needs `pyte` (pip install pyte) and a built killi (`cargo build`, or --killi); --png also needs
Google Chrome.
"""

import argparse
import fcntl
import json
import os
import pty
import re
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

import pyte

ROOT = Path(__file__).resolve().parent.parent

# Kil's themes: background, foreground, then the 16 ANSI colors.
THEMES = {
    "night": ("111316", "d7dae0", "1d1f24 e06c75 98c379 e5c07b 61afef c678dd 56b6c2 abb2bf 5c6370 ef7b85 a8d38a f0cd8a 74bdf7 d48ae8 67c7d2 e6e9ef"),
    "paper": ("fbfaf7", "2e3138", "24292f c4242f 1a7f37 845600 0550ae 8250df 1b7c83 6e7781 57606a a40e26 116329 633c01 1a71d0 835dc7 297b8f 8c959f"),
    "fjord": ("1c212b", "d4dae6", "2a303c d0707a a3c48c e4c98a 7fa6d6 b896c8 82c3c9 d4dae6 637089 dd8890 b4d29e edd7a0 95b8e3 c7a9d6 98d1d6 eef1f6"),
    "ember": ("1d1915", "e8dcc6", "2b2520 e06c50 a9b665 e3a64f 7daea3 d3869b 89b482 d4c4a8 766859 f08a6e bfcc7c f0bd6b 95c4b8 e29db0 a2c99a f2e8d5"),
    "dusk": ("17141f", "ddd8ea", "262233 e4708e 8fd1a0 e8c88a 8aa8f0 c09af0 7cc8d8 cbc5dc 6b628a f088a4 a6deb4 f0d6a2 a2baf5 d0b2f6 96d6e3 f1eefa"),
}
ANSI_NAMES = "black red green brown blue magenta cyan white".split()

FIX = "FIX the slugify test\r"
# What each scene types, and what it waits for before the screen is taken.
SCENES = {
    "start": dict(keys=[]),
    "menu": dict(keys=["/"]),
    "files": dict(keys=["fix @sl"]),
    "picker": dict(keys=["/model\r"]),
    "conv": dict(keys=[FIX], until="ask, or /"),
    "question": dict(keys=["QUESTION please\r"], until="Esc dismisses"),
    "plan": dict(keys=["\x1b[Z", "add a flag"]),
    "busy": dict(keys=[FIX, "WAIT:1.5", "also keep it small"], settle=0.2),
    "drop": dict(keys=["look at", "DROP:slug.py", "DROP:shot.png", "together"]),
    "swarm": dict(keys=["SWARM review the open PRs\r", "WAIT:4.5"], settle=0.2),
    "swarm-big": dict(keys=["SWARMBIG review every service\r", "WAIT:7"], settle=0.2),
    "swarm-done": dict(keys=["SWARM review the open PRs\r"], until="nothing blocks"),
    "peers": dict(keys=[], peer=True),
    "sessions": dict(keys=["/sessions\r"], peer=True),
    "peer": dict(keys=["ASK:Which files are you changing, and is anything committed?", "TELL:The base branch is pushed and green; rebase on it when you get there."], peer=True),
}

# The other session the peer scenes show: a file in ~/.killi/live that frames.py keeps locked,
# which is all a live session is to the others.
PEER_PID = 48213

# A 1x1 PNG, the file the drop scene drags in.
PNG = bytes.fromhex("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da63f8ffff3f0005fe02fea7369a5e0000000049454e44ae426082")

CELL, ROW, PAD, FONT = 7.8, 20, 14, 13

CONFIG = """default_model = "minimax/MiniMax-M3.1-Flash"

[thinking]
effort = "high"

[providers.minimax]
type = "openai"
base_url = "http://127.0.0.1:{port}/v1"
api_key = "demo"

[models."minimax/MiniMax-M3.1-Flash"]
provider = "minimax"
model = "MiniMax-M3.1-Flash"
max_context_size = 1000000
max_output_size = 128000
capabilities = ["image_in", "thinking", "tool_use"]

[models."zai/glm-5.3"]
provider = "minimax"
model = "glm-5.3"
max_context_size = 200000
max_output_size = 128000
capabilities = ["thinking", "tool_use"]

[agent]
notify_after = 9999

[ui]
recap = false
"""

SKILLS = {
    "brainstorming": "You MUST use this before any creative work - creating features, building components, adding functionality, or modifying behavior. Explores user intent, requirements and design before implementation.",
    "refactor": "Surgical code refactoring to improve maintainability without changing behavior. Covers extracting functions, renaming variables, breaking down god functions, improving type safety, eliminating code smells.",
    "code-review": "Review the diff for bugs",
    "pdf": "Read and write PDF files",
    "tdd": "Test-driven development with a red-green-refactor loop. Use when building features or fixing bugs test-first.",
}

SLUG = '''import os


def slugify(title: str) -> str:
    """Turns a title into a URL slug."""
    return title.lower().replace(" ", "-")
'''
TEST = '''from slug import slugify

assert slugify("Hello World") == "hello-world"
assert slugify("  Rust: Fast & Safe!  ") == "rust-fast-safe", slugify("  Rust: Fast & Safe!  ")
assert slugify("a   b") == "a-b"
assert slugify("") == ""
print("4 passed")
'''


def newest_killi():
    """Whichever of the debug and release binaries is newer; a fixed default goes stale."""
    built = [p for p in (ROOT / "target/debug/killi", ROOT / "target/release/killi") if p.exists()]
    if not built:
        sys.exit("no killi binary yet; run cargo build first")
    return max(built, key=lambda p: p.stat().st_mtime)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Scratch:
    """A HOME with a config that points at the scripted provider, skills, and a git project."""

    def __init__(self, port):
        # Resolved, so the folder killi reports (the real path) starts with HOME and shows as `~/…`.
        self.dir = Path(tempfile.mkdtemp(prefix="killi-frames-")).resolve()
        self.home = self.dir / "home"
        self.project = self.home / "code" / "slug"
        (self.home / ".killi").mkdir(parents=True)
        (self.home / ".killi" / "command-guide.json").write_text('{"command":"killi"}\n')
        (self.home / ".killi" / "config.toml").write_text(CONFIG.format(port=port))
        for name, description in SKILLS.items():
            folder = self.home / ".agents" / "skills" / name
            folder.mkdir(parents=True)
            (folder / "SKILL.md").write_text(f"---\nname: {name}\ndescription: {description}\n---\nBody.\n")
        self.project.mkdir(parents=True)
        (self.project / "slug.py").write_text(SLUG)
        (self.project / "test_slug.py").write_text(TEST)
        (self.project / "shot.png").write_bytes(PNG)
        git = ["git", "-c", "user.email=a@b", "-c", "user.name=a"]
        subprocess.run(["git", "init", "-q", "-b", "master"], cwd=self.project, check=True)
        subprocess.run([*git, "add", "-A"], cwd=self.project, check=True)
        subprocess.run([*git, "commit", "-q", "-m", "init"], cwd=self.project, check=True)

    def reset(self):
        """The project as committed, and no sessions: every scene starts the same."""
        subprocess.run(["git", "checkout", "-q", "--", "."], cwd=self.project, check=True)
        subprocess.run(["git", "clean", "-qfdx"], cwd=self.project, check=True)
        for name in ("sessions", "snapshots", "live"):
            shutil.rmtree(self.home / ".killi" / name, ignore_errors=True)
        (self.home / ".killi" / "last-model.json").unlink(missing_ok=True)

    def close(self):
        shutil.rmtree(self.dir, ignore_errors=True)


def osc(code, hex_color):
    r, g, b = (int(hex_color[i:i + 2], 16) for i in (0, 2, 4))
    return f"\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\".encode()


def hold_peer(scratch):
    """Registers PEER_PID as a session working in the scratch project, locked until closed."""
    live = scratch.home / ".killi" / "live"
    live.mkdir(parents=True, exist_ok=True)
    now = int(time.time() * 1000)
    entry = {"pid": PEER_PID, "version": "0.4.16", "cwd": str(scratch.project), "tree": str(scratch.project), "branch": "fix-slug",
             "model": "zai/glm-5.3", "started": now - 600_000, "working": True, "since": now - 252_000,
             "prompt": "review the open PRs", "socket": str(scratch.dir / f"{PEER_PID}.sock")}
    held = open(live / f"{PEER_PID}.json", "w+")
    held.write(json.dumps(entry))
    held.flush()
    fcntl.flock(held, fcntl.LOCK_EX)
    return held


def run_scene(args, scratch, scene, cols, rows, theme):
    """Runs killi through `scene` and returns the screen as rows of (char, fg, bg, bold, dim)."""
    background, foreground, _ = THEMES[theme]
    peer = hold_peer(scratch) if scene.get("peer") else None
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        env = {k: v for k, v in os.environ.items() if k not in ("COLORFGBG", "COLORTERM", "NO_COLOR", "TERM_PROGRAM")}
        env.update(TERM="xterm-256color", HOME=str(scratch.home), TERM_PROGRAM="frames" if args.inline else "kil")
        if args.colors == "truecolor":
            env["COLORTERM"] = "truecolor"
        if args.no_color:
            env["NO_COLOR"] = "1"
        os.chdir(scratch.project)
        os.execve(args.killi, [args.killi], env)

    pending = b""

    def pump(seconds):
        nonlocal pending
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            ready, _, _ = select.select([fd], [], [], min(left, 0.05))
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            # pyte has no dim: it rides on blink, which killi never uses.
            stream.feed(data.replace(b"\x1b[2m", b"\x1b[5m"))
            pending = (pending + data)[-256:]
            # A terminal answers once the bytes before a query are on its screen. Like Kil, it
            # writes the answers to cursor position and device attributes at once and the colors
            # after them.
            colors = []
            while match := re.search(rb"\x1b\[6n|\x1b\[c|\x1b\]1[01];\?(?:\x1b\\|\x07)|\x1b\[\?u", pending):
                query = match.group(0)
                if query == b"\x1b[6n":
                    os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())
                elif query == b"\x1b[c":
                    os.write(fd, b"\x1b[?62;c")
                elif query.startswith(b"\x1b]1") and not args.no_osc:
                    background_asked = query.startswith(b"\x1b]11")
                    colors.append(osc(11 if background_asked else 10, background if background_asked else foreground))
                pending = pending[:match.start()] + pending[match.end():]
            for answer in colors:
                os.write(fd, answer)

    shown = lambda: "\n".join(screen.display)
    pump(1.5)
    for key in scene["keys"]:
        if key.startswith("WAIT:"):
            pump(float(key[5:]))
        elif key.startswith(("ASK:", "TELL:")):
            # Another session's `killi ask` or `killi tell`, sent while this one is drawn.
            verb, text = key.split(":", 1)
            env = {**os.environ, "HOME": str(scratch.home), "KILLI_PID": str(PEER_PID)}
            cli = subprocess.Popen([args.killi, verb.lower(), str(pid), text], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            while cli.poll() is None:
                pump(0.1)
            pump(0.6)
        elif key.startswith("DROP:"):
            # A file dropped on the terminal arrives as a bracketed paste of its escaped path.
            path = str(scratch.project / key[5:]).replace(" ", "\\ ")
            os.write(fd, f"\x1b[200~{path} \x1b[201~".encode())
            pump(0.6)
        else:
            os.write(fd, key.encode())
            pump(0.6)
    if until := scene.get("until"):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            pump(0.3)
            if until in shown() and "Esc to interrupt" not in shown():
                break
    pump(scene.get("settle", 1.5))
    cells = [[(c.data, c.fg, c.bg, c.bold, c.blink) for c in (screen.buffer[y][x] for x in range(cols))] for y in range(rows)]
    try:
        os.write(fd, b"\x1b")
        pump(0.2)
        os.write(fd, b"/exit\r")
        pump(1.0)
        os.close(fd)
        os.waitpid(pid, os.WNOHANG)
    except OSError:
        pass
    if peer:
        peer.close()
    return cells


def hex_rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def svg(cells, theme):
    """The screen as an SVG on the theme's palette: box-drawing lines and blocks as rectangles,
    so they join like a terminal's own, and dim as Kil dims, to two thirds of each channel."""
    background, foreground, ansi = THEMES[theme]
    bg, fg = hex_rgb(background), hex_rgb(foreground)
    palette = [hex_rgb(c) for c in ansi.split()]

    def color(name, default):
        if name == "default":
            return default
        if name in ANSI_NAMES:
            return palette[ANSI_NAMES.index(name)]
        if name.startswith("bright") and name[6:] in ANSI_NAMES:
            return palette[8 + ANSI_NAMES.index(name[6:])]
        return hex_rgb(name) if len(name) == 6 else default

    def hexed(c):
        return "#%02x%02x%02x" % tuple(int(v) for v in c)

    cols = len(cells[0])
    while cells and all(c[0] in (" ", "") and c[2] == "default" for c in cells[-1]):
        cells = cells[:-1]
    width, height = round(cols * CELL + 2 * PAD), len(cells) * ROW + 2 * PAD
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" '
           f'font-family="JetBrains Mono, SF Mono, Menlo, Consolas, monospace" font-size="{FONT}">',
           f'<rect width="{width}" height="{height}" fill="{hexed(bg)}"/>']
    for y, row in enumerate(cells):
        top = PAD + y * ROW
        x = 0
        while x < cols:
            ch, f, b, bold, dim = row[x]
            if ch == "":
                x += 1
                continue
            end = x + 1
            while end < cols and row[end][1:] == (f, b, bold, dim) and row[end][0] != "":
                end += 1
            start = x
            run = "".join(c[0] for c in row[start:end])
            x = end
            ink = color(f, fg)
            if dim:
                ink = tuple(int(v) * 2 // 3 for v in ink)
            x0, w = PAD + start * CELL, (end - start) * CELL
            if b != "default":
                out.append(f'<rect x="{x0:.1f}" y="{top}" width="{w:.1f}" height="{ROW}" fill="{hexed(color(b, bg))}"/>')
            if not run.strip():
                continue
            fill = hexed(ink)
            if set(run) <= set("─━╌"):
                thick = 2 if "━" in run else 1
                mid = top + ROW / 2 + 0.5
                if "╌" in run:
                    out += [f'<rect x="{x0 + (i + k) * CELL:.1f}" y="{mid - 0.5:.1f}" width="{CELL * 0.3:.1f}" height="{thick}" fill="{fill}"/>'
                            for i in range(len(run)) for k in (0.1, 0.6)]
                else:
                    out.append(f'<rect x="{x0:.1f}" y="{mid - thick / 2:.1f}" width="{w:.1f}" height="{thick}" fill="{fill}"/>')
            elif set(run) <= set("│▏"):
                out += [f'<rect x="{x0 + i * CELL + (CELL / 2 if run[i] == "│" else 1) - 0.5:.1f}" y="{top}" width="1" height="{ROW}" fill="{fill}"/>'
                        for i in range(len(run))]
            elif set(run) == {"█"}:
                out.append(f'<rect x="{x0:.1f}" y="{top}" width="{w:.1f}" height="{ROW}" fill="{fill}"/>')
            else:
                text = run.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
                weight = ' font-weight="700"' if bold else ""
                out.append(f'<text x="{x0:.1f}" y="{top + 14.5}" textLength="{w:.1f}" lengthAdjust="spacingAndGlyphs" fill="{fill}"{weight} xml:space="preserve">{text}</text>')
    out.append("</svg>")
    return "\n".join(out), width, height


def png(svg_path, width, height):
    chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    if not os.path.exists(chrome):
        sys.exit("--png needs Google Chrome in /Applications")
    target = svg_path.with_suffix(".png")
    subprocess.run([chrome, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=2",
                    f"--window-size={width},{height}", f"--screenshot={target}", f"file://{svg_path}"],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path, help="the folder to write the SVGs to")
    parser.add_argument("--killi", default=None, help="killi binary (default: whichever of target/debug/killi and target/release/killi is newer)")
    parser.add_argument("--sizes", nargs="+", default=["118x40", "80x24", "60x24"], metavar="COLSxROWS")
    parser.add_argument("--themes", nargs="+", default=["night"], choices=sorted(THEMES))
    parser.add_argument("--scenes", nargs="+", default=list(SCENES), choices=list(SCENES))
    parser.add_argument("--inline", action="store_true", help="run killi as outside Kil, in the scrollback")
    parser.add_argument("--colors", choices=["truecolor", "256"], default="truecolor")
    parser.add_argument("--no-osc", action="store_true")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--png", action="store_true")
    args = parser.parse_args()
    if args.killi is None:
        args.killi = str(newest_killi())
        print(f"using {args.killi}")
    args.killi = str(Path(args.killi).resolve())
    args.out.mkdir(parents=True, exist_ok=True)
    port = free_port()
    server = subprocess.Popen([sys.executable, str(ROOT / "scripts" / "fake_llm.py"), str(port)])
    scratch = Scratch(port)
    try:
        time.sleep(0.5)
        for size in args.sizes:
            cols, rows = (int(n) for n in size.split("x"))
            for theme in args.themes:
                for name in args.scenes:
                    scratch.reset()
                    cells = run_scene(args, scratch, SCENES[name], cols, rows, theme)
                    text, width, height = svg(cells, theme)
                    path = (args.out / f"{name}-{size}-{theme}.svg").resolve()
                    path.write_text(text)
                    if args.png:
                        png(path, width, height)
                    print(path)
    finally:
        server.terminate()
        scratch.close()


if __name__ == "__main__":
    main()
