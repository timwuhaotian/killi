## LLM Provider Adapters & Wire Formats - Audit Results
**Score**: 3/4 (split: the conversion logic would score 4, since it is deterministic and correct across all three dialects; the failure paths would score 2, since that is where every P1/P2 clusters)
**Files reviewed**: 14 (llm/mod.rs, llm/openai.rs, llm/anthropic.rs, llm/responses.rs, config.rs [resolve, importers], login/chatgpt.rs, login/opencode_go.rs, agent.rs [request/record_reply/account/compact_with/recover/subagent/is_overflow], tui/mod.rs [retry row, busy commands], main.rs [Printer retry], session.rs [record/stamped], tools/files.rs [image sizing], docs/decisions.md 9/14/26/31/45/46/48, docs/reference/kimi-harness-inventory.md)
**Issues found**: 18 (P0 0, P1 1, P2 7, P3 10)

Evidence base. Scratch crate at `<scratchpad>/F` with tests in `F/crates/killi/src/llm/audit_f.rs`, plus one test added to `F/crates/killi/src/config.rs`. Run them with
`cd <scratchpad>/F && CARGO_TARGET_DIR=<scratchpad>/F/target cargo test --locked --offline -p killi audit_f -- --nocapture --test-threads=1`
(F10 needs `-- --ignored`; the config test is `audit_f_opencode`). All 18 scratch tests pass, which means each one reproduces the behavior described.

Real-log statistics come from read-only scripts that print aggregates only: `python3 -I <scratchpad>/F/{logstats,errstats,errstats2,errstats3,eststats,wsstats,idonly}.py`, run over `~/.killi/sessions` (175 logs, 4,062 timed requests, 8,835 assistant messages). No provider was called and killi was never run.

Owned leads:
- **intel-0a obs 22, Responses `include` vs replay: CONFIRMED (P1-1).** One nuance: gateways tolerate the replay, OpenAI's `store:false` doesn't.
- **obs 22, OpenAI `resize_with(index+1)`: CONFIRMED (P2-7).**
- **[V7]: EXTENDED (P2-5).** All three protocols plus an HTML body, with the full loop measured at 142 s.

### P0 - Critical (must fix before merge / blocks production)
None.

### P1 - High
- **[llm/responses.rs:43-45, :59-63, :137]** With `/effort off` (or `[thinking] enabled = false`) on a Responses model that reasons by default, killi stores reasoning items that carry no `encrypted_content` and replays them under `store: false`. Turning thinking back on doesn't help.
  Scenario: the user picks `off`, which is the first entry `/effort` offers (`config.rs:373-377`), on a Responses model: the template's default `openai/gpt-5.5`, or any ChatGPT-plan model. `Thinking::Off` sends no `reasoning` and no `include`, but a reasoning model still reasons at its default effort. It returns `{"type":"reasoning","id":"rs_…","summary":[]}`, which `:137` stores. Every later request replays it (`:60-61`) with `store:false`. Tested (F1) request body:
  `{"input":[{"content":"hi","role":"user"},{"id":"rs_abc","summary":[],"type":"reasoning"},…],"store":false,…}` with no `include`.
  OpenAI resolves an id-only reasoning item server-side, and under `store:false` it was never persisted. Its documented stateless rule is to pass `encrypted_content` back, and the request fails `invalid_request_error` ("Item with id 'rs_…' not found. Items are not persisted when `store` is set to false…").
  That error is fatal (400) and repeats on every request. `/compact` sends the same history (`agent.rs:1270`) and fails too. `is_overflow` doesn't match, so the reduced path never runs. Only `/new`, or a `/model` to a chat or Anthropic model (whose converters filter `type: reasoning`), gets out.
  Impact: a Responses session is wedged by a menu choice, with no message pointing at the cause.
  Side effect: `Reply::reasoning_kept()` counts these empty items as kept, so decision 46's `reasoning_kept` rate over-reports on this path. The scratch test asserts it.
  Log evidence for the fix: 17 id-only items exist in the logs, from `opencode-go/deepseek-v4.1-flash` (15) and `xfyun/spark-x2.6` (2), at effort `high`. They carried a **non-empty `summary`**, and the 32 requests after them succeeded. Gateways tolerate id-only items and use their summaries; OpenAI does not.
  Verified: tested (request shape, F1). The OpenAI rejection is from its store:false contract, not run live (no key).
  Suggested fix: in `responses::convert_input`, skip a `reasoning` item that has neither `encrypted_content` nor any summary text, since it carries nothing to replay. Apply it on every request so the result stays deterministic. This drops exactly what OpenAI returns under `off` and keeps the gateway items. Make `reasoning_kept()` apply the same test, but only to `type: reasoning` items. Anthropic `thinking` blocks and chat `reasoning_details` have neither field and must still count as kept.

### P2 - Medium
- **[llm/responses.rs:59-66 with agent.rs:575-581]** A Responses reply cut at `max_output_tokens` while still reasoning is replayed as a `reasoning` item followed directly by a user message.
  Scenario: the stream gives `reasoning` (with `encrypted_content`) and then `response.incomplete` with reason `max_output_tokens`, so the finish reason is `length` with no text and no calls. `validate_reply` accepts it. `record_reply` stores `content:""` plus the item, and the agent pushes the "cut off… Continue" reminder. `convert_input` then emits `[…, reasoning, {"role":"user",…}]`, because the empty text is skipped at `:64`. Tested (F2), printed input:
  `[{"content":"hi","role":"user"},{"encrypted_content":"gAAAA","id":"rs_1",…,"type":"reasoning"},{"content":"<system-reminder>Your reply was cut off…","role":"user"}]`.
  OpenAI requires a reasoning item to be followed by the output item it produced ("Item 'rs_…' of type 'reasoning' was provided without its required following item"). If it enforces that here, every later request fails, `/compact` included.
  When it triggers: reasoning outruns a configured `max_output_size`, for example a models.dev `limit.output` from the importers or a small hand-set value. The ChatGPT plan sends no cap, so it isn't exposed.
  Verified: tested (shape). The API rejection is unverified.
  Suggested fix: in `convert_input`, drop a message's `reasoning` items when that message yields no assistant text and no `function_call`. This is a different seam from P1-1.

- **[llm/mod.rs:149-164; shared Client at agent.rs:1224]** Concurrent subagents retry 429s independently, with no shared cooldown. On a provider with a concurrency cap, some swarm members are starved for the whole retry budget and fail.
  Scenario, from real logs: three `agent_swarm` runs on `zai/glm-5.3` (8–9 parallel subagents). The provider answered `HTTP 429 {"error":{"code":"1302","message":"Rate limit reached for requests"}}` with no `Retry-After`. Six subagents each spent 10 attempts and **139.5–152.2 s** of backoff, then failed. Four of them (`1792982039-84145/agent-5`, `1793001919-15241/agent-5` and `agent-9`, `1792981991-35556/agent-9`) completed 0 or 2 requests while their siblings ran 7–67 requests each.
  Every sibling's jittered backoff is computed alone. Nothing tells a starved request that the cap is a concurrency limit, or that a sibling just freed a slot.
  Impact: lost subagent work (the parent model gets failure results and may redo them), plus 2.5 minutes of dead time per starved member.
  Verified: reproduced from logs (`errstats2.py`, `errstats3.py`); traced.
  Suggested fix: every subagent already shares one `Rc<Client>`. Add a cooldown in `Client`, for example `Rc<Cell<Option<Instant>>>`: a non-quota 429 sets it, every clone waits for it before its next attempt, and attempts spent waiting while siblings succeed don't count against the budget. Alternatively, lower swarm parallelism after a 429. This overlaps with G (swarm).

- **[llm/mod.rs:184, :212]** Every transport error is retried as transient, including ones that can't heal, and the message the user sees drops the cause.
  Scenario: `send()` errors all become `Failure::transient(format!("request failed: {e}"))`. That covers builder errors (a `base_url` without a scheme, a key or header value containing a newline), TLS handshake or unknown-CA errors (a corporate inspecting proxy, given the compiled-in webpki roots in intel-0b), DNS failures and refused connections. Each takes 10 attempts. The backoff alone sums to 127.5–159 s (F6, computed; one sample came to 150.5 s). F10 measured 142.2 s end to end for a 200 JSON body.
  `{e}` prints reqwest's top-level text only. Tested (F8, F12):
  - shown `request failed: builder error`; the chain says `relative URL without a base`;
  - DNS and refused both show `request failed: error sending request for url (http://…/chat/completions)`; the chains say `dns error <- failed to lookup address information` and `tcp connect error <- Connection refused`;
  - an idle mid-stream timeout shows `stream interrupted: error decoding response body`, with nothing about a timeout.
  `complete` also has no total deadline. intel-0b's "accepted but never answers" case is 10 × 300 s, about 52 min, broken only by Esc in the TUI or a signal in `-p`.
  Impact: a typo in config costs about 2.5 min and then shows a message that doesn't name the problem. A proxy CA problem looks the same as an outage.
  Verified: tested (F8, F12, F6).
  Suggested fix: format the source chain (walk `source()` or use anyhow's `{:#}`). Treat `e.is_builder()` as fatal, and TLS certificate errors as fatal after one retry. Consider an overall deadline per `complete` call.

- **[llm/mod.rs:203-228] [V7 extended]** A 200 response that isn't SSE fails differently per protocol, is always transient, and the body is never shown.
  Tested (F7):
  - OpenAI JSON with a trailing newline: `OpenAI stream ended without finish_reason or [DONE]`;
  - the same body without a trailing newline: `stream ended with unconsumed data`;
  - Anthropic with a MiniMax-style `{"base_resp":{"status_code":1004,"status_msg":"login fail…"}}`: `Anthropic stream ended without message_stop`;
  - Responses with an HTML login page: `Responses stream ended without a terminal response event`.

  Full loop (F10): **142.2 s, 9 retries**, ending in `OpenAI stream ended without finish_reason or [DONE] (after 10 attempt(s))`. The model-not-found body never appears.
  Verified: tested.
  Suggested fix: in `attempt`, check `content-type`, or count the `data:` lines seen. When the body isn't an event stream, read it (capped) and send it through `classify`, so a 4xx-like body is fatal and shown.

- **[llm/anthropic.rs:225, :232-237; agent.rs:1339-1347]** With thinking enabled, the final assistant `tool_use` message can lack a thinking block, which Anthropic rejects.
  Scenario: a turn runs with `/effort off`, or on another model (killi drops unsigned or scalar reasoning for Claude at `:225`/`:233`), and the user presses Esc during a tool call. `recover()` adds placeholder results, and the next prompt merges into that `tool_result` turn. Then `/effort high`, or `/model` to a budget-thinking Claude, sends `thinking: enabled` while the final assistant message starts with `tool_use`. Tested (F4b), sent messages:
  `…{"content":[{"type":"tool_use",…}],"role":"assistant"},{"content":[{"type":"tool_result",…},{"type":"text","text":"<system-reminder>The user interrupted…now fix it"}]…`.
  Anthropic documents a 400 for this: "a final `assistant` message must start with a thinking block". It repeats on every request, including the `/compact` request.
  Verified: tested (shape). The API rule is from Anthropic's docs and unverified live. I did not assert the inverse case (thinking blocks replayed after `/effort off`).
  Suggested fix: in `anthropic::prepare`, when thinking is on and the last assistant message with `tool_use` has no thinking block, send `thinking: disabled` for that request only. Alternatively, apply a pending `/effort` or `/model` change only once the history doesn't end in an exchange left open by an interrupt.

- **[llm/openai.rs:168-173] (obs 22)** The tool-call `index` from the provider is trusted.
  - A gap or a non-zero first index creates nameless calls, which are executed and then replayed. Tested (F3): `index: 2` alone gives three calls, the first two `{"name":"","arguments":""}` with ids `call_1` and `call_2`. They run as unknown tools and are replayed as `"function":{"name":""}` (chat) or `"type":"tool_use","name":""` (Anthropic). Both APIs reject an empty function name, so every later request fails. A continuation chunk sent without `index` (`unwrap_or(len)`) opens a new nameless slot the same way.
  - `index: u64::MAX` panics. F3b gives `attempt to add with overflow` at `openai.rs:171` in debug. In release `index+1` wraps to 0 and `tool_calls[usize::MAX]` is out of bounds. Either way `panic = "abort"` kills killi. A large but valid index (1e9) asks for about 72 GB and aborts on allocation failure.

  No provider in the logs did any of this; this is about robustness.
  Verified: tested (F3, F3b).
  Suggested fix: cap the index (for example below 128, else fail the attempt as transient). Merge a chunk with no index and no id into the last slot. In `finish`, drop slots whose name and id are both empty, or fail the attempt.

- **[llm/mod.rs:184-200 with agent.rs:1666-1671; tools/files.rs:94, :113-134]** Provider media limits aren't handled. A media rejection isn't overflow-shaped, so it wedges the session. Decision 26's "reduce, then strip, older media" exists only inside overflow compaction.
  Scenario: images stay in history as base64. Only images over 500 KB are shrunk to 1568 px, so a PNG under 500 KB, such as a simple Retina screenshot at 2880×1800, keeps its pixel size. Known provider limits:
  - Bedrock Converse (the user's SPH gateway): 20 images per request.
  - Anthropic: at most 100 images per request, at most 2000 px per side once a request holds more than 20 (so such a full-size PNG fails only after the history crosses 20 images), and 32 MB per request (413 `request_too_large`).

  A 21st screenshot in one live history (MCP Godot/Unity/Blender captures, `fetch_url screenshot`, `read` of PNGs) gets a 400 or 413. None of `is_overflow`'s needles match, so it is fatal on every request. `/compact` sends the same images and fails too; only `/new` recovers.
  Verified: traced. The limits come from provider docs and are unverified live. No current log held more than one live image (`logstats.py`), because decision 43's storm compacted after each screenshot. Since 0.4.9 images accumulate.
  Suggested fix: in the adapters' `prepare`, or one shared pre-pass, keep the newest N images (Kimi's projection keeps 2 when degrading) and replace older ones with a text placeholder. That is deterministic for a given history, but it moves a cache boundary only when the count crosses N. As a fallback, route 413 and media-shaped 400s (`image`, `request_too_large`) to `compact_with(Reduced)` the way overflow is routed. This overlaps with G (`tools/files.rs` sizing, `is_overflow`).

### P3 - Low / Nits
- **[llm/mod.rs:178-183]** `custom_headers` are appended beside the built-in headers instead of replacing them. Tested (F9): overriding `Authorization` sends `authorization: Bearer k` and `authorization: Bearer gateway-key`. On Anthropic, `x-api-key` and `anthropic-beta` are each sent twice. Which one wins depends on the server. Fix: skip a built-in header whose name, case-insensitively, is in `model.headers`.
- **[config.rs:1312-1313 vs login/opencode_go.rs:139; llm/anthropic.rs:39]** The opencode importer keeps an `@ai-sdk/anthropic` `baseURL` as written. The AI SDK's convention includes `/v1`, so killi posts to `…/anthropic/v1/v1/messages` and gets a 404. Tested (`audit_f_opencode`). OpenCode Go's login already strips it. Fix: strip a trailing `/v1` in the importer, or have the Anthropic adapter skip adding `/v1` when `base_url` already ends in it, which also covers hand-written configs.
- **[llm/responses.rs:107-189 vs openai.rs:150-152]** On Responses, `response.refusal.delta` is ignored, so a refusal commits an empty assistant turn and shows nothing. Tested (F2b). On Chat Completions the same refusal is fatal. Fix: collect refusal text into `text`, or fail fatally as chat does.
- **[llm/mod.rs:251-256; llm/anthropic.rs:357-366]** Some finish reasons are treated as fatal when they shouldn't be. DeepSeek's documented `insufficient_system_resource` (a transient overload) discards the partial reply. Anthropic's `model_context_window_exceeded` (output stopped at the window) is fatal, and `is_overflow` doesn't catch it, because its needle is `context window` with a space. Tested (F5 and the F14 needle check). It is reachable only with compaction raised or off. Fix: map `model_context_window_exceeded` to `length` and treat `insufficient_system_resource` as transient.
- **[llm/mod.rs:320, :187-192]** The quota words misfire on Gemini's per-minute 429 ("Resource has been exhausted (e.g. check quota)."), which is then not retried. Tested (F6). Separately, `Retry-After` is parsed only as integer seconds, so an HTTP-date, `retry-after-ms` or a fractional value falls back to the backoff. That fallback is acceptable. In the field the quota detector works: 19 of 19 quota 429s in the logs failed at attempt 1.
- **[llm/anthropic.rs:117]** On budget-thinking Claude models (4.5 and older), `budget_tokens = min(effort budget, max_tokens - 1)` follows the per-request `cap_output`. Tested (F14), on a 200k window:
  - estimate around 170k tokens: budget 29,727;
  - around 175k: 24,727;
  - around 180k: 19,727;
  - around 199k: `{"type":"disabled"}`.

  Anthropic invalidates message cache breakpoints whenever thinking parameters change, so in that zone every request misses the conversation cache, and thinking switches off near the end. It is unreachable with default compaction, which fires at about 90k billed tokens on a 200k window; it needs `compact_at` raised or set to 0. Fix: clamp the budget to a few fixed steps.
- **[llm/anthropic.rs:59-61]** `ClaudeVersion::parse` requires the name to start with `claude-` after the last `/`. Bedrock-style ids (`us.anthropic.claude-sonnet-4-5-…-v1:0`, `anthropic.claude-3-7-sonnet-…`) are therefore treated as non-Claude: they get adaptive thinking instead of a budget, plus the non-Claude thinking-replay rules. Tested (F13). This matters only on the Anthropic protocol, since the user's Bedrock path is OpenAI-format. Fix: locate `claude-` anywhere in the name.
- **[llm/anthropic.rs:238]** Whitespace-only assistant text (for example `"\n\n"` before a `tool_use`) is sent as a text block. Anthropic rejects "text content blocks must contain non-whitespace text" (Kimi inventory :230/:391). Tested (F4, shape). No such message appears among the 8,835 logged assistant messages (`wsstats.py`). Fix: use `trim().is_empty()` in `assistant_blocks` and `user_blocks`.
- **[llm/mod.rs:296-310]** The comment calls the `cap_output` estimate "conservative", but it under-counts on most models. Billed/estimated prompt-token medians from `eststats.py`:
  - Claude via the SPH gateway: 1.48 (max 1.82);
  - MiniMax: 1.12;
  - DeepSeek: 1.06;
  - GLM: 1.03;
  - 12 of 16 models are above 1.0.

  So the 256-token margin doesn't cover the error once the cap binds, and input plus `max_tokens` can exceed the window. That happens only when `max_output` is more than about 40% of the window, or compaction is raised or off. Every logged window is 1M, so the user isn't affected today. Fix: correct the doc comment and reserve a proportional margin (for example 15% of the estimate).
- **[llm/openai.rs:43-47]** `Thinking::On` (the default when no effort is configured) sends no `reasoning_effort` on the first request, then `"medium"` from the first reply that carries reasoning, then nothing again after a compaction. Tested (F3c). That silently changes the effort mid-session. The parameter sits outside the cached prefix on OpenAI-style caches. Related: `off` sends nothing on chat or Responses, so a model that reasons by default keeps reasoning and billing while the status line says `off`.

### Checked and fine
- Determinism: serde_json's `preserve_order` is off (`cargo tree -e features -i serde_json`), so every `Map` is a BTreeMap and keys serialize sorted. The chat body is a struct with fixed field order. F11: bodies are byte-identical across repeated `prepare` calls and across a log round trip, for all three protocols, so resume matches. The next request's prefix is unchanged apart from the moved Anthropic mark. Tool arguments are stored raw and parsed only into Anthropic's `input`.
- A `max_tokens` or `max_output_tokens` that changes on every request is outside every provider's cached prefix. OpenAI-style automatic caching and Anthropic's cache key both cover tools, system and messages only. The one exception is the budget clamp in the P3 above.
- Anthropic cache marks: 3 of the 4 allowed (system, last tool, last non-thinking block). The moving last mark is Anthropic's documented pattern. The 20-block look-back is not a problem in practice: 0 of 4,062 logged requests followed more than 20 new blocks, and the most parallel calls in one message was 9.
- Anthropic conversion: tool results come first in their user turn; consecutive user turns merge; an assistant message with no blocks is dropped; an empty tool result becomes `(empty)`; data-URL images become base64 sources; signed thinking replays verbatim and unsigned thinking is dropped for Claude; `redacted_thinking` is sent in the real API shape (Kimi's thinking-plus-signature quirk is avoided); there is a scalar thinking fallback for compatible providers.
- Usage: Anthropic `prompt = input + cache_creation + cache_read`, `cached = cache_read`, with MiniMax's late values taking over. OpenAI cached variants are read (`prompt_tokens_details.cached_tokens`, `prompt_cache_hit_tokens`, `cached_tokens`). Responses `input_tokens` is the total.
- SSE: CRLF is handled by `trim`; `event:` and `:` comment lines are skipped; `[DONE]` is handled; a non-whitespace tail is an error.
- Partial output never reaches history: each attempt builds a fresh `Reply`, and `record_reply` runs only on `Ok` (V2 is the TUI side of this).
- Retries are visible: a `⚠ retry n in m:ss: reason` row in the TUI and a stderr line in `-p`.
- ChatGPT plan: the token is fetched per attempt, refreshed 5 minutes early and once after a 401 (that retry counts as an attempt), under an flock with a re-read of the file. No `max_output_tokens` is sent, the system prompt goes in `instructions`, and there are no system-role items.
- `cache_control` on Chat Completions marks only the copy, as the repo's tests and decision 48's live run show. `prompt_cache_key` is the session id, and `session_header` carries it.
- `base_url` trailing slashes are trimmed (`config.rs:318`), and OpenCode Go's Anthropic URL drops `/v1`. Subagents share the parent's `Rc<Client>`, and with it the connection pool.
- Tool-result images go in a separate user message after the results, which works on all three protocols.
