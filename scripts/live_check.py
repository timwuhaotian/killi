#!/usr/bin/env python3
"""Checks live sessions end to end: two real killi sessions in one repository, each in a
pseudo-terminal under a scratch HOME, a scripted provider that logs every request, and the
`killi sessions`, `killi ask` and `killi tell` commands between them.

    scripts/live_check.py                       # target/release/killi
    scripts/live_check.py --killi target/debug/killi

Prints PASS or FAIL for each check and exits non-zero if any failed. What it covers: the list
and its liveness (a session that exits, crashes or loses its terminal drops out), the banner,
the reminder the model gets when the sessions in its tree change (again after a compaction, as
are plan and swarm mode),
a question answered from the other conversation without being kept, a message for an idle and
for a working session, the undo warning, and `/sessions`.

A session's terminal must be read while another process talks to it, as a real terminal is:
killi blocks writing its screen to a full pseudo-terminal, and a request then times out.
"""

import argparse
import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def text_of(message):
    content = message.get("content", "")
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def provider(log):
    """A /chat/completions server that appends each request's messages to `log` and answers:
    another session's question with a line about the work, a prompt with SLEEP with a slow
    command first, anything else with "Done: <prompt>"."""

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def chunk(self, data):
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            self.wfile.flush()

        def event(self, payload):
            self.chunk(b"data: " + (payload if isinstance(payload, str) else json.dumps(payload)).encode() + b"\n\n")

        def do_POST(self):
            messages = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}").get("messages", [])
            with open(log, "a") as f:
                f.write(json.dumps(messages) + "\n")
            last = text_of(messages[-1]) if messages else ""
            prompt, steps = "", 0
            for m in reversed(messages):
                if m.get("role") == "user" and not text_of(m).lstrip().startswith("<system-reminder>"):
                    prompt = text_of(m)
                    break
                steps += m.get("role") == "assistant"
            calls, text = [], ""
            if "A question about your work from" in last:
                text = "I'm fixing slugify in slug.py on master; nothing committed yet."
            elif "SLEEP" in prompt and steps == 0:
                calls = [{"index": 0, "id": "c1", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": "sleep 6; echo slept"})}}]
            else:
                text = "Done: " + prompt[:40]
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("transfer-encoding", "chunked")
            self.end_headers()
            delta = lambda d, finish=None: {"choices": [{"index": 0, "delta": d, "finish_reason": finish}]}
            if text:
                self.event(delta({"content": text}))
            if calls:
                self.event(delta({"tool_calls": calls}))
            self.event(delta({}, "tool_calls" if calls else "stop"))
            self.event({"choices": [], "usage": {"prompt_tokens": 5000, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 4500}}})
            self.event("[DONE]")
            self.chunk(b"")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class Session:
    """killi in a pseudo-terminal, in the scratch project."""

    def __init__(self, killi, env, cwd):
        self.out = b""
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 110, 0, 0))
            os.chdir(cwd)
            os.execve(killi, [killi], env)

    def pump(self, seconds):
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            if not select.select([self.fd], [], [], min(left, 0.05))[0]:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            self.out += data
            for _ in re.findall(rb"\x1b\[6n", data):
                os.write(self.fd, b"\x1b[1;1R")

    def type(self, text, wait=1.0):
        os.write(self.fd, text.encode())
        self.pump(wait)

    def text(self):
        plain = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[=>]", b"", self.out).decode("utf-8", "replace")
        return re.sub(r"\s+", " ", plain)

    def until(self, needle, seconds=15):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self.pump(0.2)
            if needle in self.text():
                return True
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--killi", default=str(ROOT / "target/release/killi"))
    args = parser.parse_args()
    killi = str(Path(args.killi).resolve())

    scratch = Path(tempfile.mkdtemp(prefix="killi-live-check-")).resolve()
    home, project, log = scratch / "home", scratch / "home" / "code" / "slug", scratch / "requests.jsonl"
    server = provider(log)
    (home / ".killi").mkdir(parents=True)
    (home / ".killi" / "command-guide.json").write_text('{"command":"killi"}\n')
    (home / ".killi" / "config.toml").write_text(f"""default_model = "fake/m"
[providers.fake]
type = "openai"
base_url = "http://127.0.0.1:{server.server_address[1]}/v1"
api_key = "x"
[models."fake/m"]
provider = "fake"
model = "m"
max_context_size = 200000
capabilities = ["tool_use"]
[agent]
notify_after = 9999
[ui]
recap = false
""")
    project.mkdir(parents=True)
    (project / "slug.py").write_text("x = 1\n")
    subprocess.run(["git", "init", "-q", "-b", "master"], cwd=project, check=True)
    subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "init"], cwd=project, check=True)
    env = {k: v for k, v in os.environ.items() if k not in ("TERM_PROGRAM", "KILLI_PID")}
    env.update(HOME=str(home), TERM="xterm-256color", TERM_PROGRAM="live-check")
    live = home / ".killi" / "live"
    sessions, failed = [], []

    def cli(*words, pid=None):
        """Runs the CLI while every session's terminal is read."""
        run_env = dict(env, **({"KILLI_PID": str(pid)} if pid else {}))
        p = subprocess.Popen([killi, *words], env=run_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        end = time.monotonic() + 60
        while p.poll() is None and time.monotonic() < end:
            for s in sessions:
                s.pump(0.05)
        if p.poll() is None:
            p.kill()
            return "TIMEOUT"
        return p.stdout.read().strip()

    def requests():
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def check(name, ok, detail=""):
        print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n     {str(detail)[:600]}"))
        if not ok:
            failed.append(name)

    def start():
        s = Session(killi, env, project)
        sessions.append(s)
        s.pump(2.5)
        return s

    def contents(messages, role="user"):
        return [text_of(m) for m in messages if m.get("role") == role]

    try:
        a = start()
        check("a session is listed", str(a.pid) in cli("sessions"), cli("sessions"))
        b = start()
        check("the banner names the other session", f"Also in this repository: {a.pid}" in b.text(), b.text()[-800:])
        listing = cli("sessions", pid=b.pid)
        check("the list marks this session", f"{b.pid} · idle" in listing and "this session" in listing, listing)
        status = cli("sessions", str(a.pid))
        check("a status needs no model", status.startswith(f"{a.pid} · idle") and "fake/m" in status, status)

        a.type("hello there\r", 0.5)
        a.until("Done: hello there")
        told = [u for u in contents(requests()[-1]) if f"Another killi session works in this repository too: {b.pid}" in u]
        check("the model hears about the other session", len(told) == 1, contents(requests()[-1]))
        check("the terminal says the agent was told", f"Session {b.pid} also works in this repository" in a.text(), a.text()[-600:])

        answer = cli("ask", str(a.pid), "which files are you changing?", pid=b.pid)
        check("ask gets the other model's answer", "fixing slugify" in answer, answer)
        side = requests()[-1]
        check("the question is a side request under the peer note", "A question about your work from killi session" in text_of(side[-1]), side[-1])
        a.pump(1)
        check("the asked session shows question and answer", f"session {b.pid} asks" in a.text() and "Answered from this conversation" in a.text(), a.text()[-800:])

        told = cli("tell", str(a.pid), "I pushed the base branch", pid=b.pid)
        check("a message to an idle session says so", told.startswith("It is idle"), told)
        a.pump(1)
        check("the idle session shows the message", f"session {b.pid} says" in a.text(), a.text()[-800:])
        a.type("next please\r", 0.5)
        a.until("Done: next please")
        users = contents(requests()[-1])
        check("the message reaches the model before the prompt, marked as not the user's",
              users[-1] == "next please" and any("I pushed the base branch" in u and "It isn't from the user" in u for u in users[:-1]), users[-3:])
        check("the question wasn't kept", not any("which files are you changing" in json.dumps(m) for m in requests()[-1]))
        check("a session can't ask itself", "is this session" in cli("ask", str(a.pid), "hi", pid=a.pid))

        # Plan mode on and announced, then a compaction summarizes every reminder away.
        a.type("\x1b[Z", 0.5)
        a.type("plan it\r", 0.5)
        a.until("Done: plan it")
        check("plan mode is announced", any(u.startswith("<system-reminder>Plan mode is on.") for u in contents(requests()[-1])))
        a.type("/compact\r", 0.5)
        a.until("Done:", 10)
        a.pump(1)
        a.type("after compact\r", 0.5)
        a.until("Done: after compact")
        users = contents(requests()[-1])
        compacted = next((i for i, u in enumerate(users) if "This conversation was compacted" in u), None)
        after = users[compacted + 1:] if compacted is not None else []
        check("after a compaction the model is told again about the other session",
              any(f"Another killi session works in this repository too: {b.pid}" in u for u in after), users)
        check("…and that plan mode is on", any(u.startswith("<system-reminder>Plan mode is on.") for u in after), users)
        check("…and that swarm mode is on", any(u.startswith("<system-reminder>Swarm mode is on.") for u in after), users)
        a.type("\x1b[Z", 0.5)

        cli("tell", str(a.pid), "stale news", pid=b.pid)
        a.type("/new\r", 1.0)
        a.type("fresh start\r", 0.5)
        a.until("Done: fresh start")
        check("/new drops messages meant for the conversation left", not any("stale news" in u for u in contents(requests()[-1])), contents(requests()[-1]))

        a.type("SLEEP now\r", 1.0)
        time.sleep(1)
        told = cli("tell", str(a.pid), "base is green", pid=b.pid)
        check("a message to a working session says so", told.startswith("It is working"), told)
        busy = cli("sessions", str(a.pid))
        check("a status shows the running call", "Waiting on:" in busy and "sleep 6" in busy, busy)
        a.until("Done: SLEEP now", 20)
        after = requests()[-1]
        check("the message comes at the next step, after the tool result", after[-1]["role"] == "user" and "base is green" in text_of(after[-1]), after[-3:])

        b.type("hello from b\r", 0.5)
        b.until("Done: hello from b")
        a.type("again\r", 0.5)
        a.until("Done: again")
        b.type("/undo\r", 1.0)
        check("undo warns about the other session's work", f"session {a.pid} also worked in this repository" in b.text(), b.text()[-600:])
        b.type("/undo\r", 1.0)
        check("a second undo goes ahead", "Undone" in b.text(), b.text()[-400:])
        b.type("/sessions\r", 1.0)
        check("/sessions lists the other session", f"● {a.pid} · idle" in b.text(), b.text()[-600:])

        a.type("/exit\r", 1.5)
        sessions.remove(a)
        check("a session that exits leaves the list", str(a.pid) not in cli("sessions"))
        b.type("one more\r", 0.5)
        b.until("Done: one more")
        check("the model hears the other session left", any("No other killi session works in this repository now" in u for u in contents(requests()[-1])))

        os.kill(b.pid, signal.SIGKILL)
        time.sleep(0.5)
        sessions.remove(b)
        left = sorted(p.name for p in live.iterdir())
        gone = cli("sessions")
        check("a crashed session's file is removed by the next reader", left == [f"{b.pid}.json"] and gone == "No killi session is running." and not any(live.iterdir()), (left, gone))

        c = start()
        sock = Path(tempfile.gettempdir()) / f"killi-live-{os.getuid()}" / f"{c.pid}.sock"
        had = (live / f"{c.pid}.json").exists() and sock.exists()
        sessions.remove(c)
        os.close(c.fd)
        time.sleep(1.5)
        check("a closed terminal (SIGHUP) removes its file and socket", had and not (live / f"{c.pid}.json").exists() and not sock.exists())
    finally:
        for s in sessions:
            os.kill(s.pid, signal.SIGKILL)
        server.shutdown()
        shutil.rmtree(scratch, ignore_errors=True)
    print(f"\n{'All checks passed.' if not failed else f'{len(failed)} failed.'}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
