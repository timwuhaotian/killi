#!/usr/bin/env python3
"""Asks a configured provider small questions about how it behaves, to confirm what a session log suggests.

  python3 scripts/probe.py stream    <model> [--effort E]   which fields a chat/completions reply streams, and its raw usage
  python3 scripts/probe.py responses <model>                does the Responses API return reasoning items to replay
  python3 scripts/probe.py cache     <model> [--cache-control]   two identical requests: does the second read from the cache

<model> is an alias from ~/.killi/config.toml (--config changes the file), such as meta/muse-wonder-1.0-contributor.
Each probe sends two or three small synthetic requests (`cache` sends about 6k tokens twice) with the
provider's own key, which is never printed. Providers of type `anthropic` aren't probed: killi already
speaks their cache protocol.
"""

import argparse
import json
import os
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

REASONING_KEYS = ("reasoning_content", "reasoning_details", "reasoning")  # the fields killi reads
BASH = {"name": "bash", "description": "Run a shell command.",
        "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}
ASK = "Think carefully, then use the bash tool to print the current date."


def provider_of(config, alias):
    model = config["models"].get(alias)
    if not model:
        raise SystemExit(f"unknown model {alias}; the aliases are under [models] in the config")
    provider = config["providers"][model["provider"]]
    kind = model.get("protocol") or provider["type"]
    if kind == "anthropic":
        raise SystemExit(f"{alias} uses the Anthropic API; killi already handles its caching and thinking")
    key = os.environ[provider["api_key_env"]] if provider.get("api_key_env") else provider["api_key"]
    base = provider.get("base_url", "https://api.openai.com/v1").rstrip("/")
    headers = {"authorization": "Bearer " + key, "content-type": "application/json", **provider.get("custom_headers", {})}
    return model["model"], kind, base, headers


def post(url, headers, body):
    """Yields each server-sent event's JSON of a streamed request."""
    request = urllib.request.Request(url, data=json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=180) as reply:
            for raw in reply:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                data = line[5:].strip()
                if data == "[DONE]":
                    return
                yield json.loads(data)
    except urllib.error.HTTPError as e:
        raise SystemExit(f"HTTP {e.code}: {e.read()[:300].decode('utf-8', 'replace')}")


def stream(model, kind, base, headers, effort):
    body = {"model": model, "stream": True, "stream_options": {"include_usage": True}, "max_tokens": 2000,
            "messages": [{"role": "user", "content": ASK}], "tools": [{"type": "function", "function": BASH}]}
    if effort:
        body["reasoning_effort"] = effort
    keys, samples, usage = {}, {}, None
    for event in post(base + "/chat/completions", headers, body):
        usage = event.get("usage") or usage
        for choice in event.get("choices") or []:
            for key, value in (choice.get("delta") or {}).items():
                keys[key] = keys.get(key, 0) + 1
                if value not in (None, "", []) and key not in samples:
                    samples[key] = json.dumps(value, ensure_ascii=False)[:140]
    print("fields streamed:", keys)
    for key, value in samples.items():
        print(f"  {key} = {value}")
    print("usage:", json.dumps(usage))
    billed = ((usage or {}).get("completion_tokens_details") or {}).get("reasoning_tokens", 0)
    sent = [k for k in REASONING_KEYS if k in keys]
    if billed and not sent:
        print(f"VERDICT: {billed} reasoning tokens billed but none streamed, so killi can't keep them on this API. Try `responses`.")
    elif sent:
        print(f"VERDICT: reasoning arrives in {sent}; killi keeps and replays it.")
    else:
        print("VERDICT: no reasoning billed or streamed.")


def responses(model, kind, base, headers):
    body = {"model": model, "stream": True, "store": False, "max_output_tokens": 2000,
            "input": [{"role": "user", "content": ASK}], "tools": [{"type": "function", **BASH}],
            "reasoning": {"effort": "high", "summary": "auto"}, "include": ["reasoning.encrypted_content"]}
    seen, items, usage = {}, [], None
    for event in post(base + "/responses", headers, body):
        seen[event.get("type")] = seen.get(event.get("type"), 0) + 1
        if event.get("type") == "response.output_item.done":
            items.append(event["item"])
        if event.get("type") == "response.completed":
            usage = event["response"].get("usage")
    print("events:", seen)
    for item in items:
        shown = {k: (v if len(str(v)) < 50 else str(v)[:47] + "…") for k, v in item.items() if k != "id"}
        print("  item:", json.dumps(shown, ensure_ascii=False))
    print("usage:", json.dumps(usage))
    reasoning = [i for i in items if i.get("type") == "reasoning"]
    if reasoning and any(i.get("encrypted_content") or i.get("summary") for i in reasoning):
        print("VERDICT: reasoning items come back. Set `type = \"openai_responses\"` on the provider (or `protocol` on the model) to keep them.")
    else:
        print("VERDICT: no reasoning items returned on the Responses API.")


def cache(model, kind, base, headers, cache_control):
    if kind != "openai":
        raise SystemExit("the cache probe speaks chat/completions; use it on a provider of type `openai`")
    rules = " ".join(f"Rule {i}: keep answers short and precise, and cite the rule number when it applies." for i in range(1, 330))
    tag = f"[probe {int(time.time())}] "
    system = tag + rules
    if cache_control:
        system = [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]
    body = {"model": model, "stream": True, "stream_options": {"include_usage": True}, "max_tokens": 20,
            "prompt_cache_key": "probe-" + tag, "messages": [{"role": "system", "content": system}, {"role": "user", "content": "Say ok."}]}
    reads = []
    for attempt in (1, 2):
        usage = None
        for event in post(base + "/chat/completions", headers, body):
            usage = event.get("usage") or usage
        print(f"request {attempt}:", json.dumps(usage))
        details = (usage or {}).get("prompt_tokens_details") or {}
        reads.append(max(details.get("cached_tokens", 0), (usage or {}).get("cache_read_input_tokens", 0),
                         (usage or {}).get("prompt_cache_hit_tokens", 0)))
        time.sleep(2)
    if reads[1]:
        print(f"VERDICT: the second request read {reads[1]} tokens from the cache.")
    else:
        print("VERDICT: no cache read on the second request. Compare the two `cost` fields if the gateway reports one; "
              "if the first request wrote a cache (cache_creation > 0) and the second billed in full, the gateway doesn't serve reads.")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("probe", choices=["stream", "responses", "cache"])
    parser.add_argument("model")
    parser.add_argument("--config", default=str(Path.home() / ".killi/config.toml"))
    parser.add_argument("--effort")
    parser.add_argument("--cache-control", action="store_true", help="cache: mark the system message with cache_control")
    args = parser.parse_args()
    config = tomllib.loads(Path(args.config).read_text())
    model, kind, base, headers = provider_of(config, args.model)
    print(f"{args.probe} probe of {args.model} ({kind}, {base})")
    if args.probe == "stream":
        stream(model, kind, base, headers, args.effort)
    elif args.probe == "responses":
        responses(model, kind, base, headers)
    else:
        cache(model, kind, base, headers, args.cache_control)


if __name__ == "__main__":
    main()
