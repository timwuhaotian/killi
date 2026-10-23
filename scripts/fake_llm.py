#!/usr/bin/env python3
"""A scripted OpenAI-compatible /chat/completions server, so killi's screens can be drawn
without a model (see frames.py).

    scripts/fake_llm.py [PORT]        # 18741 by default; Ctrl+C stops it

The prompt picks the scenario: one containing "FIX" or "slugify" is a bug fix (a task list, a
read, a failing run, a thought, an edit with a diff, a passing run and a Markdown answer),
one containing "QUESTION" is an `ask_user_question`, anything else a one-line answer. The
step is the number of assistant messages since the last real user message, so the reminders
killi appends as user messages (`<system-reminder>`) don't restart the script.
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OLD = 'return title.lower().replace(" ", "-")'
NEW = 'slug = re.sub(r"[^a-z0-9]+", "-", title.lower())\n    return slug.strip("-")'


def call(i, name, args):
    return {"id": f"call_{i}", "name": name, "arguments": json.dumps(args)}


def todos(first, second, third):
    items = [("Reproduce the failing test", first), ("Rewrite slugify", second), ("Re-run the tests", third)]
    return {"todos": [{"content": content, "status": status} for content, status in items]}


FIX = [
    {"reason": "The user wants the failing slugify test fixed. Let me read the module and run the tests to see how it fails.", "pause": 0.9,
     "calls": [call(1, "todo_write", todos("in_progress", "pending", "pending")), call(2, "read", {"path": "slug.py"}),
               call(3, "bash", {"command": "python3 test_slug.py"})]},
    {"reason": "The test expects punctuation removed, trimmed, and multiple spaces collapsed to one hyphen. The current code only "
               "replaces single spaces. A regex that turns every run of non-alphanumerics into one hyphen and strips the ends covers all three.",
     "pause": 2.6,
     "text": "Now I'll implement a proper slugify. The current implementation only lowercases and replaces single spaces — it doesn't "
             "strip punctuation, collapse runs, or trim the edges.",
     "calls": [call(4, "edit", {"path": "slug.py", "edits": [{"old_string": "import os\n", "new_string": "import os\nimport re\n"},
                                                            {"old_string": OLD, "new_string": NEW}]}),
               call(5, "todo_write", todos("completed", "completed", "in_progress"))]},
    {"calls": [call(6, "bash", {"command": "python3 test_slug.py && python3 -m py_compile slug.py && echo compiled"}),
               call(7, "todo_write", todos("completed", "completed", "completed"))]},
    {"text": """Fixed and passing.

**Root cause:** `slugify` only did `title.lower().replace(" ", "-")`, so it left punctuation in place, didn't collapse runs of whitespace, and didn't trim the edges — `"  Rust: Fast & Safe!  "` became `"--rust:-fast-&-safe!--"`.

## What changed

`slug.py` now lowercases, turns every run of characters outside `a-z0-9` into one `-`, and strips leading and trailing hyphens:

```python
def slugify(title: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", title.lower())
    return slug.strip("-")
```

Verified:
- `python3 test_slug.py` — 4 passed
- `py_compile` — clean

One judgment call worth flagging: an apostrophe is treated as a separator, so `don't` becomes `don-t`. The tests don't cover it; say the word if you'd prefer `dont` and I'll add it.
"""},
]
QUESTION = [{"calls": [call(1, "ask_user_question", {
    "question": "slugify can drop apostrophes (don't → dont) or treat them as separators (don-t). Which do you want?",
    "options": ["Drop them: don't becomes dont", "Separator: don't becomes don-t", "Keep the current behavior"]})]}]
SHORT = [{"text": "Hello! Ask me to read, change or run something in this folder."}]


def text_of(message):
    content = message.get("content", "")
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def step_for(messages):
    real = [i for i, m in enumerate(messages) if m.get("role") == "user" and not text_of(m).lstrip().startswith("<system-reminder>")]
    start = real[-1] if real else 0
    prompt = text_of(messages[start]) if messages else ""
    steps = FIX if "FIX" in prompt or "slugify" in prompt else QUESTION if "QUESTION" in prompt else SHORT
    index = sum(1 for m in messages[start:] if m.get("role") == "assistant")
    return steps[min(index, len(steps) - 1)]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def chunk(self, data):
        self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
        self.wfile.flush()

    def event(self, payload):
        body = payload if isinstance(payload, str) else json.dumps(payload)
        self.chunk(b"data: " + body.encode() + b"\n\n")

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        messages = request.get("messages", [])
        step = step_for(messages)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()

        def delta(d, finish=None):
            return {"choices": [{"index": 0, "delta": d, "finish_reason": finish}]}

        words = step.get("reason", "").split(" ")
        for i in range(0, len(words), 4):
            self.event(delta({"reasoning_content": " ".join(words[i:i + 4]) + " "}))
            if step.get("pause"):
                time.sleep(step["pause"] / max(1, len(words) // 4))
        text = step.get("text", "")
        for i in range(0, len(text), 14):
            self.event(delta({"content": text[i:i + 14]}))
            time.sleep(0.004)
        for n, c in enumerate(step.get("calls", [])):
            self.event(delta({"tool_calls": [{"index": n, "id": c["id"], "type": "function",
                                              "function": {"name": c["name"], "arguments": c["arguments"]}}]}))
        self.event(delta({}, "tool_calls" if step.get("calls") else "stop"))
        prompt_tokens = 4200 + 1900 * len(messages)
        self.event({"choices": [], "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": 120 + len(text) // 4,
                                             "prompt_tokens_details": {"cached_tokens": int(prompt_tokens * 0.83)}}})
        self.event("[DONE]")
        self.chunk(b"")


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18741
    try:
        ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
