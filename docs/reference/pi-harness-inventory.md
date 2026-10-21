# pi harness inventory (for killi)

Source: github.com/earendil-works/pi (MIT), shallow clone read 2026-10-21. Read-only review; nothing executed.

Path legend: `ca/` = `packages/coding-agent/src/`, `ai/` = `packages/ai/src/`, `ag/` = `packages/agent/src/`, `mcp/` = `packages/mcp/src/`, `ev/` = `packages/evals/`.

Char counts below were computed by reconstructing the exact strings/schemas from source in a separate script (pi code not run). Tokens ≈ chars/4.

---

## 1. System prompt and tool declarations

### Default system prompt
Built in `ca/core/system-prompt.ts:128-193` as ordered *named sections*; every section except `preamble` is wrapped `<name>…</name>` (`:188-191`), joined with `\n\n` (`ai/utils/text.ts:15-21`).
Order: `preamble` → `tools` → `rules` → `docs` → [`addendum` (APPEND_SYSTEM.md)] → [`project_context` (AGENTS.md…)] → [`skills`] → `cwd` → [extension sections, e.g. `mcp_servers`] (`:155-186`).
No date, no git status, no OS/env block — the prompt is deterministic for a given cwd + install.
Default tools = `read, bash, edit, write` (`ca/core/system-prompt.ts:64`, `ca/core/settings-manager.ts:85`).

Full text with the default tools, minus the `<docs>` section (this is exactly the eval "without_docs" variant):

```
You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

<tools>
- read: Read file contents
- bash: Execute bash commands (ls, grep, find, etc.)
- edit: Make precise file edits with exact text replacement, including multiple disjoint edits in one call
- write: Create or overwrite files

In addition to the tools above, you may have access to other custom tools depending on the project.
</tools>

<rules>
- Use bash for file operations like ls, rg, find
- Use read to examine files instead of cat or sed.
- You can inspect PI_* environment variables for current model and session details.
- Use edit for precise changes (edits[].oldText must match exactly)
- When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls
- Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.
- Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.
- Use write only for new files or complete rewrites.
- Be concise in your responses
- Show file paths clearly when working with files
</rules>

<cwd>
/home/user/project
</cwd>
```

The `<docs>` section (`ca/core/system-prompt.ts:162-169`) is pi-self-documentation (paths to README/docs/examples + a topic→file map). Not relevant to killi except as a pattern ("read docs only when asked about the harness itself").

Rules are assembled per active tool (`buildRules`, `:88-125`): tool `promptGuidelines` are only emitted when that tool is active; "use bash for ls/rg/find" only when grep/find/ls are absent (`:108-116`); rules are de-duplicated (`:95-100`).

| piece (default 4 tools, cwd=`/home/user/project`, install path 59 chars) | chars | ~tok |
|---|---|---|
| preamble | 169 | 42 |
| `<tools>` body | 322 | 81 |
| `<rules>` body | 822 | 206 |
| `<docs>` body (incl. 3×59-char paths; 1127 without paths) | 1304 | 326 |
| **total prompt** | **2705** | **~676** |
| total without `<docs>` | 1384 | ~346 |
| prompt + 4 default tool declarations | 5249 | ~1312 |

### Tool declarations (description + params as compact JSON `{type,properties,required}`)

| tool | desc | schema | name+desc+schema | ~tok | source |
|---|---|---|---|---|---|
| read | 303 | 304 | 611 | 153 | `ca/core/tools/read.ts:14-18,96` |
| bash | 248 | 218 | 470 | 118 | `ca/core/tools/bash.ts:40-43,255` |
| edit | 326 | 776 | 1106 | 276 | `ca/core/tools/edit.ts:21-41,151-152` |
| write | 127 | 225 | 357 | 89 | `ca/core/tools/write.ts:11-14,52-53` |
| grep | 221 | 741 | 966 | 242 | `ca/core/tools/grep.ts:21-33,78` |
| find | 186 | 353 | 543 | 136 | `ca/core/tools/find.ts:26-32,78` |
| ls | 184 | 227 | 413 | 103 | `ca/core/tools/ls.ts:11-14,62` |
| tool_search (inactive by default) | 338 | 220 | 569 | 142 | `ca/extensions/tool-search/tool.ts:159-164,215` |
| **default 4 (read/bash/edit/write)** | | | **2544** | **~636** | |
| **all 7 built-ins** | | | **4466** | **~1116** | |

Notes:
- Each tool has three texts: model `description`, one-line `promptSnippet` (system prompt `<tools>` list), and `promptGuidelines` bullets (system prompt `<rules>`) (`ca/core/extensions/types.ts:584-589`). Guidelines are duplicated knowledge vs. descriptions (edit says the same thing 3×: description, param descriptions, 4 rules).
- Descriptions embed the truncation limits so the model knows to page (`read.ts:96`, `bash.ts:255`).
- `constrainedSampling: { type: "json_schema", strict: "prefer" }` on read/bash/edit/write (`read.ts:101`): strict tool schemas where provider supports it, with automatic downgrade on "grammar too large" 400s (`ai/api/anthropic-messages.ts:664-679`).
- `prepareArguments` shim for edit repairs common model mistakes before validation (`edit.ts:103-134`): `edits` sent as JSON string, a single object instead of array, legacy top-level `oldText/newText`.

---

## 2. Prompt caching

### Prefix stability: transcript-embedded system state
- Core idea: the system prompt and tool set are **state carried by `system` messages in the transcript**, not request headers. `SystemMessage { content, sections?: Record<string,string|null>, toolsAdded?, toolsRemoved? }` (`ai/types.ts:542-568`). Leading message = prompt; later ones are *patches*.
- When anything changes (skills reloaded, AGENTS.md edited, tools toggled), pi diffs sections (`ca/core/system-prompt.ts:217-229`, called from `ca/core/agent-session.ts:1743-1755`) and appends a mid-conversation system message carrying only changed sections (`null` = removed). The head of the prompt is never rewritten → cached prefix survives.
- Rendering of a patch to the model: `Updated system prompt section "<name>":\n\n<text>` / `Removed system prompt section "<name>".` (`ai/utils/text.ts:28-40`).
- Providers that cannot take mid-convo system messages get the transcript collapsed into one replayed head (`ai/utils/transcript.ts:73-120`) — correct but cache-busting. Capability is per model (`compat.supportsMidConvoSystemMessages`, default false); enabled in the generated catalog for Anthropic first-party, Kimi K3, some OpenAI Responses models (`packages/ai/scripts/generate-models.ts:930-972,1243-1260`).
- On Anthropic, mid-convo system messages are buffered and emitted just before the next assistant message, because a system message between `tool_use` and `tool_result` is rejected (`ai/api/anthropic-messages.ts:1364-1373`).
- After compaction, the whole replayed prompt/tool state is snapshotted into the compaction entry and becomes the new single leading system message (`ca/core/session-manager.ts:102-103,461-464,1270-1283`) — patches collapse at a point where the cache is being rebuilt anyway.

### Tool changes mid-session
- Agent loop computes `toolsAdded`/`toolsRemoved` between transcript-declared tools and the executable set before each request and inserts/merges a system message (`ag/agent-loop.ts:332-372`). A changed definition = remove + add (`ai/utils/transcript.ts:150-167`); comparison via canonical JSON (`:123-142`).
- Transport capability levels (`ai/utils/transcript.ts:221-263`):
  - `none`: top-level `tools` = current set (cache bust on any change).
  - `additions`: top-level list frozen at the last removal/redefinition; later additions declared in place → cache invalidated once per removal, not per addition.
  - `changes`: everything declared in place.
- Anthropic native path: top-level `tools` = initial tools + a `defer_loading` placeholder `__pi_deferred_placeholder__` ("Reserved placeholder. Never available. Never call this.") so the list never changes; later tools go in `tool_addition {tool_definition}` blocks and removals in `tool_removal {tool_reference}` blocks inside system messages (`ai/api/anthropic-messages.ts:208-213,1202-1204,1265-1276,1384-1398`).
- Kimi K3 quirk recorded: a removed tool that is later re-declared is ignored → any redeclaration counts as non-additive (`ai/utils/transcript.ts:206-207`).

### "Loadout" / hidden declarations (`prepareLoadout`)
- Tool exposure enum (`ca/core/extensions/types.ts:501-516`): `direct` (declared + callable), `model-only` (declared, not callable from scripts), `codemode` (callable from codemode scripts only), `deferred` (loadable by `tool_search`), `hidden`.
- A tool may define `prepareLoadout(loadout) → { descriptions?, hiddenDeclarations? }` (`types.ts:551-574,633-638`), called whenever active tools change (`ca/core/agent-session.ts:1574-1625`). It can rewrite other tools' model-facing descriptions and hide declarations from requests while keeping them active/callable (and still recorded in the transcript so `/tree`/resume work).
- Hidden declarations are filtered from every request by a `transformContext` projection over the *whole* transcript with the *current* hidden set, so projected declarations are consistent across requests (`agent-session.ts:1767-1790`). System prompt tool list/rules also drop hidden tools (`system-prompt.ts:150`).
- Used by codemode: mode `on` appends "Codemode: `tools.x(args)` resolves to …" to declared tools' descriptions; mode `only` hides all direct tool declarations so the model sees only `codemode` (`ca/extensions/codemode/tool.ts:308-356`). Codemode description lists non-direct tools within an inline token budget (default 3000 est. tokens, `ca/core/settings-defaults.ts:57-60`; round-robin cheapest-first per namespace `codemode/tool.ts:197-220`). Listing is by exposure, not active set, so `tool_search` loads don't redeclare codemode (`:317-318`).
- `tool_search` and codemode descriptions are deliberately independent of which tools are registered, so they stay byte-stable as MCP servers connect (`tool-search/tool.ts:211-215`, `codemode/tool.ts:222-227`).

### Anthropic `cache_control` placement (`ai/api/anthropic-messages.ts`)
- Retention: `short` default (5 min ephemeral); `long` → `ttl: "1h"` if model supports it; `none` → no markers. Env `PI_CACHE_RETENTION=long` (`:65-93`).
- Breakpoints: (1) system text block (`:1243-1252`; OAuth adds a separate "You are Claude Code…" block, also marked, `:1228-1242`), (2) last tool in `tools` (`:1264`, `:1659`), (3) last block of the last user/system message (tool_result/text/image/tool_addition/tool_removal) (`:1529-1555`). Max 4 used.
- Summarization calls force `cacheRetention: "none"` to avoid paying cache-write premium on one-off requests (`ca/core/compaction/compaction.ts:626-632`).
- OpenAI(-compatible): `prompt_cache_key = sessionId` (≤64 chars) and `prompt_cache_retention: "24h"` for long retention (`ai/api/openai-completions.ts:817-837`, `ai/api/openai-prompt-cache.ts:1-8`); `cache_control` on last tool / last message for compat endpoints (`openai-completions.ts:1105-1197`); session-affinity headers `x-session-affinity` / OpenRouter `x-session-id` (`:781-788`).
- Codex Responses WebSocket: if the new request equals the last one except `input`, and old input+output is a prefix, send only the delta with `previous_response_id` (`ai/api/openai-codex-responses.ts:1447-1499`).

### `core/cache-warmer.ts`
Keeps one prompt-cache entry alive by re-sending the exact last request with `maxTokens: 1, maxRetries: 0` before TTL expiry (`:332-338`).
- Delay = `min(0.9·TTL, TTL−10s)` (`:29-32`); TTL from model `promptCache[retention]`.
- Economics gate: `expectedSavings = P(continue)·(missCost − hitCost) − warmCost ≥ $0.05` (`:20,378-400`). P=1 while the agent run is active ("streaming" phase), 0.15 when idle (measured constant, `:21-26`).
- Hard limits: 60 min after the real request while streaming, 30 min idle (`:15-18`); refresh deadline keeps half the margin for timer lateness (`:286-298`).
- Not used when replay would change the cache key (Anthropic budget-thinking derives budget from max_tokens) (`:48-58`).
- Modes `off|streaming|idle`, default `streaming` (`ca/core/settings-defaults.ts:61`) — i.e. it mainly covers long tool executions (>5 min builds/tests) mid-run. Stops when model/messages change (`ca/core/sdk.ts:361-372,420`). Warm usage is persisted as `usage` entries (`cache-warmer.ts:342-349`).

### `core/cache-stats.ts`
Cache-miss detector/accounting: per assistant message, `missed = min(prevPrompt, curPrompt) − cacheRead`; ignored below 1024 tokens (breakpoint granularity noise) (`:10-11,56-90`). Only counts once the provider has ever reported cache activity (handles read-only-reporting providers) (`:63-68`). Resets at compaction/branch summary; model switches are counted as misses (`:112-119`). Reports idle time (vs 5-min TTL, `:8`) and model change as likely causes; cost = missed × (paid rate − read rate). Off by default in UI (`showCacheMissNotices: false`, `settings-defaults.ts:28`).

---

## 3. Edit tool

- Schema: `{ path, edits: [{ oldText, newText }] }` — multi-edit in one call is the only shape (`ca/core/tools/edit.ts:21-41`).
- Semantics: all `oldText` matched against the **original** file, not incrementally; must be unique and non-overlapping; applied in reverse offset order (`ca/core/tools/edit-diff.ts:291-362`).
- Matching (`edit-diff.ts:207-245`): exact `indexOf` first; fallback "fuzzy" = both sides normalized with NFKC, per-line `trimEnd`, smart quotes → ASCII, Unicode dashes → `-`, special spaces → space (`:34-55`). Uniqueness is counted in fuzzy space (`:247-251`). If any edit needed fuzzy, replacements are computed in normalized space and only the touched *lines* are written back; untouched lines keep original bytes (`:122-173,352-355`). No whitespace-indent-insensitive or line-trimmed-start matching, no Levenshtein.
- Pre-processing: BOM stripped and restored, CRLF detected → normalized to LF → restored (`edit.ts:190-197`, `edit-diff.ts:11-25`).
- Error messages (`edit-diff.ts:253-289,346-348`, single vs multi variant):
  - not found: `Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.` / `Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines.`
  - ambiguous: `Found {n} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.` / `Found {n} occurrences of edits[{i}] in {path}. Each oldText must be unique. …`
  - empty: `oldText must not be empty in {path}.`
  - no-op: `No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.`
  - overlap: `edits[{a}] and edits[{b}] overlap in {path}. Merge them into one edit or target disjoint regions.`
  - access: `Could not edit file: {path}. Error code: {code}.` (`edit.ts:177-182`)
  - empty array: `Edit tool input is invalid. edits must contain at least one replacement.` (`edit.ts:136-141`)
- Success result to model is tiny: `Successfully replaced {n} block(s) in {path}.` (`edit.ts:207`); diff/patch go to `details` (UI only), not to the model.
- Errors carry no nearest-match hint / snippet of the file.

---

## 4. Truncation

Constants (`ca/core/tools/truncate.ts:11-13`): `DEFAULT_MAX_LINES = 2000`, `DEFAULT_MAX_BYTES = 50KB`, `GREP_MAX_LINE_LENGTH = 500`. Whichever limit hits first; never partial lines except tail edge case.
- `truncateHead` (read, grep, find, ls) `:78-160`; `truncateTail` (bash) `:168-241`; `truncateLine` → `…... [truncated]` `:268-276`; `truncateMiddle` (MCP, Codex-style `…N chars truncated…`) `:292-314`.
- read (`read.ts:177-203`), no line-number prefixes in output:
  - `[Showing lines {a}-{b} of {total}. Use offset={next} to continue.]`
  - `[Showing lines {a}-{b} of {total} (50.0KB limit). Use offset={next} to continue.]`
  - `[{n} more lines in file. Use offset={next} to continue.]` (user limit)
  - giant first line: `[Line {n} is {size}, exceeds 50.0KB limit. Use bash: sed -n '{n}p' {path} | head -c 51200]`
  - `Offset {o} is beyond end of file ({n} lines total)` (error, `:164`)
- bash (tail; `bash.ts:338-356`), full output spilled to temp file:
  - `[Showing lines {a}-{b} of {total}. Full output: {tmp}]`, `… ({50.0KB} limit). Full output: {tmp}]`, `[Showing last {size} of line {n} (line is {size}). Full output: {tmp}]`
  - empty → `(no output)`; non-zero exit → append `Command exited with code {n}` and `isError: true` (`:400-407`); `Command timed out after {s} seconds`, `Command aborted` (`:374-380`). No default timeout (`:42`).
- grep (`grep.ts:280-303`): default 100 matches; notices joined in one bracket: `[100 matches limit reached. Use limit=200 for more, or refine pattern. 50.0KB limit reached. Some lines truncated to 500 chars. Use read tool to see full lines]`; `No matches found`. Output `path:line: text`, context lines `path-line- text`. Uses `rg --json --hidden`.
- find (`find.ts:280-292`): default 1000, `[1000 results limit reached. Use limit=2000 for more, or refine pattern]`, uses `fd --glob --hidden`. ls (`ls.ts:145-155`): default 500, `[500 entries limit reached. Use limit=1000 for more]`, `(empty directory)`.
- MCP results: 20KB middle truncation (`ca/extensions/mcp/tools.ts:51,180-199`): `Warning: truncated output (original token count: {n})\nTotal output lines: {n}\n\n{head…N chars truncated…tail}\n\n[Full output: {path} (read it with offset/limit)]`.
- `OutputAccumulator` (`ca/core/tools/output-accumulator.ts`): bounded-memory streaming of bash output. Streaming UTF-8 decode, keeps only a rolling decoded tail (≤ 2×maxBytes, trimmed at 4×) for snapshots (`:57-61,182-228`); buffers raw chunks in memory until limits exceeded, then opens a temp file and streams everything there (`:64-78,239-256`); line count tracked incrementally; `readFullOutput(max)` returns head+tail with `[... N bytes omitted ...]` for programmatic (codemode) callers, 1MB cap (`:149-176`, `bash.ts:24`).
- Compaction serializer truncates each tool result to 2000 chars (`ca/core/compaction/utils.ts:93-104`).

---

## 5. `file-mutation-queue.ts`

`ca/core/tools/file-mutation-queue.ts:28-61`. Problem: the agent loop executes tool calls **in parallel by default** (`ag/agent.ts:261`; sequential only if a tool sets `executionMode: "sequential"`, `ag/agent-loop.ts:527-529`). Two `edit`/`write` calls on the same file in one assistant turn would race (read-modify-write lost update). Fix: per-file promise chain keyed by `realpath` (falls back to resolved path for not-yet-existing files, `:16-26`); different files still run concurrently. Registration itself is serialized (`registrationQueue`) so realpath resolution can't reorder enqueues. Used by edit (`edit.ts:163`) and write (`write.ts:67`); abort is checked after each await rather than rejecting from a listener, so the lock is held until in-flight fs ops settle (`edit.ts:164-170`).

---

## 6. Compaction (`ca/core/compaction/`)

- Settings (`ca/core/settings-defaults.ts:9-13`, `compaction.ts:126-130`): `enabled: true, reserveTokens: 16384, keepRecentTokens: 20000`.
- Trigger (`ca/core/agent-session.ts:2977-3136`):
  1. threshold: `contextTokens > contextWindow − reserveTokens` (`compaction.ts:267-270`), using provider-reported usage of last assistant message; estimate (chars/4, images 4800 chars) if errored/zero usage (`compaction.ts:276-349`). Checked after each run and before prompt submission.
  2. overflow error (regex catalog, §8) or a `length` stop below the model's max output → drop the failed attempt from context (persistent `context_edit` omission), compact, **retry once** (`agent-session.ts:3054-3097`); second failure surfaces `Context overflow recovery failed after one compact-and-retry attempt…`.
  3. silent overflow (successful response with `usage.input > contextWindow`) → compact, no retry.
- Cut point: walk back from newest accumulating estimated tokens until ≥ `keepRecentTokens`; cut at a user or assistant message, never at a tool result (`compaction.ts:388-501`). If the cut splits a turn, the turn prefix gets its own summary (`TURN_PREFIX_SUMMARIZATION_PROMPT`, `:929-942`) and is merged: `{history}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}` (`:1017-1018`).
- Summary request: separate system prompt "You are a context summarization assistant… Do NOT continue the conversation… ONLY output the structured summary." (`utils.ts:161-163`); conversation serialized as plain text `[User]: … [Assistant]: … [Assistant tool calls]: read(path="…") … [Tool result]: …` so the model doesn't continue it; **thinking omitted**; tool results capped at 2000 chars (`utils.ts:93-155`). Wrapped `<conversation>…</conversation>`, plus `<previous-summary>` for iterative updates (`compaction.ts:713-729`).
- Instruction (`compaction.ts:507-538`), exact sections: `## Goal`, `## Constraints & Preferences`, `## Progress` (`### Done` / `### In Progress` / `### Blocked`), `## Key Decisions`, `## Next Steps`, `## Critical Context`; "Keep each section concise. Preserve exact file paths, function names, and error messages." Update variant (`:540-579`) says PRESERVE/ADD/UPDATE (move In Progress→Done) and allows removing irrelevant items. Optional `Additional focus: {customInstructions}` (`/compact <text>`).
- Rejected summaries: `length` stop (partial) or tool calls → error, not persisted (`compaction.ts:585-593,742-748`). Retries via same policy as agent turns (`:618-638`).
- Preserved: last ~20k tokens verbatim; file lists appended as `<read-files>…</read-files>` / `<modified-files>…</modified-files>` accumulated across compactions (`utils.ts:63-87`, `compaction.ts:57-88,1042-1044`); current system prompt + tool state snapshot (§2). Summary injected as a user message: `The conversation history before this point was compacted into the following summary:\n\n<summary>\n…\n</summary>` (`ca/core/messages.ts:11-17,176-183`).
- Branch summarization (`/tree` navigation) uses a similar structured prompt (`branch-summarization.ts:253-290`).

---

## 7. MCP

- Client library `mcp/`: JSON-RPC, stdio + streamable-HTTP + in-memory transports, OAuth (discovery, PKCE flow), cursor pagination with duplicate-cursor guard (`mcp/client.ts:347-373`), default request timeout 30s (`mcp/client.ts:38`; server config default 60s, progress resets it, `ca/core/mcp-servers.ts:75-77`). `notifications/tools/list_changed` triggers re-list (`ca/extensions/mcp/runtime.ts:380-381,430`).
- Tools registered as `mcp__<server>__<tool>`, names >64 chars hashed (`ca/extensions/mcp/tools.ts:49,90-92`).
- **Exposure per server/tool** (`ca/core/mcp-servers.ts:43-73`, default `codemode`, `ca/extensions/mcp/index.ts:135-137`):
  - `codemode` (default): NOT declared to the model at all. The built-in `codemode` tool (JS in QuickJS sandbox) is auto-activated; scripts discover tools with `searchTools()` / `describeTool()` / `describeNamespace()` (`ca/extensions/codemode/tool.ts:118-134`). → MCP adds 1 tool (`codemode`) to the declared list regardless of server count.
  - `deferred`: hidden until the model calls `tool_search` (BM25 over name, description, schema text, namespace + server instructions; limit 8; `ca/extensions/tool-search/tool.ts:21,103-157,195-209`). Matches are activated → declared from the next call via `toolsAdded` system message (cache-friendly on capable providers, §2). Result: `Loaded {n} tools. They are available from your next call:\n- name: first line of description`.
  - `direct`: declared upfront like built-ins; first prompt waits ≤10s for these servers (`index.ts:86-93`).
  - `hidden`: unreachable. Per-tool overrides via `toolExposure` with `*` patterns.
- Activation logic: config-driven, before servers connect, so the declared set is stable from turn 1 (`index.ts:494-533`).
- System prompt `mcp_servers` section lists only servers whose tools are not declared: `MCP servers whose tools are not declared to you. Call the tools of \`codemode\` servers from codemode scripts. …` then `- mcp__server (codemode): <first line of description/instructions>`; ≤250 chars per description, ≤4096 chars total, overflow summarized as `- … N more servers; find their tools with searchTools()` (`index.ts:155-215`).
- Resource tools (`list_mcp_resources`, `list_mcp_resource_templates`, `read_mcp_resource`) take the widest exposure of servers with resources (`index.ts:476-490`).
- Output cap 20KB middle-truncated (§4). All MCP calls run through the normal tool pipeline (hooks/permissions).

---

## 8. LLM layer (`ai/`)

- **Retries, two layers**:
  - HTTP layer `retryProviderRequest` (`ai/utils/provider-retry.ts:99-128`): SDK clients run with `maxRetries: 0` (`ai/api/anthropic-messages.ts:638-642`) and pi re-implements SDK policy so backoff is abortable: retry on `x-should-retry`, 408/409/429/5xx, network (`:25-37`); honors `retry-after-ms`/`retry-after`, fails fast if server asks > 60s (`:39-66`); else `min(0.5·2^i, 8)s` with 25% jitter (`:67-68`). Provider-layer `maxRetries` unset by default → 0.
  - Agent layer (`ca/core/agent-session.ts:3759-3858`, defaults `settings-defaults.ts:18-26`): `maxRetries 3`, `baseDelayMs 2000` doubling, cap 60s. Classification by regex on error text (`ai/utils/retry.ts:33-121`: overloaded, rate limit, 5xx codes, network/socket/fetch, "ended without", "stream ended before message_stop", etc.) with explicit non-retryable quota/billing patterns (`:7-31`). Failed attempt is persistently omitted from model context before retry (`agent-session.ts:1241-1257,3838`). Context overflow is excluded from retry (handled by compaction) (`:3759-3764`).
  - Stream idle timeout default 300s (`ca/core/http-dispatcher.ts:4`).
- **Overflow detection** (`ai/utils/overflow.ts:38-67,142-176`): ~28 provider regexes (Anthropic "prompt is too long", OpenAI "exceeds the context window", Kimi "exceeded model token limit", …) with exclusions for throttling (`:80-84`), HTTP 413, plus silent overflow (`usage.input > window`) and MiMo-style `length` with 0 output at ≥99% window. `isRecoverableLength` (`:184-186`).
- **Tool-call argument streaming**: accumulate raw deltas, re-parse each delta with `parseStreamingJson` = `JSON.parse` → repair → `partial-json` → repair+partial → `{}` (`ai/utils/json-parse.ts:129-149`; used `openai-completions.ts:669-672`, `anthropic-messages.ts:795-800`). On block end, strict parse `parseToolCallArguments`: repairs raw control chars and invalid escapes (`:33-96`), must be an object, else `arguments: {}` + `argumentsError` including the raw text (`:106-120`) → surfaced to model as `Invalid arguments for tool "x": … Received arguments: …` (`ai/utils/validation.ts:317-320`). Schema validation coerces types (`Value.Convert`, optional-null normalization) and reports `Validation failed for tool "x":\n  - path: msg\n\nReceived arguments:\n{json}` (`validation.ts:321-352`). Anthropic `eager_input_streaming` or `fine-grained-tool-streaming-2025-05-14` beta (`anthropic-messages.ts:194,1580-1585,1656`).
- **Reasoning round-trip, OpenAI-compatible** (`ai/api/openai-completions.ts`):
  - Read: first non-empty of `reasoning_content`, `reasoning`, `reasoning_text` per delta (avoids double count when providers send two) and the field name is stored as the thinking block's `thinkingSignature` (`:593-626`). Structured `reasoning_details` (OpenRouter, encrypted) accumulated and stored JSON in the signature slot (`:628-639,331-336`). Reasoning processed before content in the same delta (`:593-594`).
  - Write back: same field name it arrived in (`assistantMsg[signature] = thinking`), or `reasoning_details` verbatim (`:1320-1360,1393-1395`); `requiresReasoningContentOnAssistantMessages` (DeepSeek) forces `reasoning_content: ""` on every assistant msg (`:1396-1402,1621,1665`); `requiresThinkingAsText` mode inlines thinking as plain text without tags "to avoid model mimicking them" (`:1333-1338`). Assistant content sent as a plain string, not parts array (some models mirror the array structure) (`:1340-1347`).
  - Cross-model: thinking from a different provider/api/model is **dropped** (not converted to text); signed/redacted blocks kept for same model; `thoughtSignature` stripped; tool-call IDs normalized (`ai/api/transform-messages.ts:119-166`). Orphaned tool calls get synthetic `No result provided` error results (`:179-199`).
  - Anthropic: thinking signatures replayed; unsigned (aborted) thinking → text unless `allowEmptySignature` (`anthropic-messages.ts:1442-1476`); managed-effort models use `thinking: adaptive` + `block_binding.prefix_mismatch_behavior: "drop_block"` so prefix mismatches don't 400 (`:1282-1290`). Codex Responses: `store: false` + `include: ["reasoning.encrypted_content"]` (`openai-codex-responses.ts:577-582`).

---

## 9. Skills

- Discovery: `~/.pi/agent/skills/` and `<cwd>/.pi/skills/` + configured paths (`ca/core/skills.ts:458-463`); a dir containing `SKILL.md` is one skill (stop recursing), else recurse; root-level `*.md` also allowed; honors `.gitignore/.ignore/.fdignore`, skips dotdirs and `node_modules` (`:16,173-275`). Frontmatter `name` ≤64, `description` ≤1024 chars, `disable-model-invocation` (`:11-14,70,341`).
- Prompt: only name + description + file path, in the `skills` section (progressive disclosure) (`skills.ts:358-388`):
  ```
  The following skills provide specialized instructions for specific tasks.
  Use the read tool to load a skill's file when the task matches its description.
  When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.

  <available_skills>
    <skill>
      <name>…</name>
      <description>…</description>
      <location>/abs/path/SKILL.md</location>
    </skill>
  </available_skills>
  ```
  Reader verb adapts to the active tools (`read`, else `bash`, else neutral) and the section is omitted if no reader is reachable (`ca/core/system-prompt.ts:174-182`).
- Loading: no dedicated skill tool — the model uses `read`. User can force-inject with `/skill:name args`, which inlines `<skill name="…" location="…">\nReferences are relative to {baseDir}.\n\n{body}\n</skill>\n\n{args}` into the user message (`ca/core/agent-session.ts:2193-2222`). Skills with `disable-model-invocation: true` are only reachable this way.
- Context files: `AGENTS.override.md`, `AGENTS.md`, `CLAUDE.md` (first match per dir), global agent dir + every ancestor of cwd, root-first, worktree shadow de-dup (`ca/core/resource-loader.ts:184-274`); rendered `<project_instructions path="…">` (`system-prompt.ts:79-86`). `SYSTEM.md` replaces the preamble; `APPEND_SYSTEM.md` → `addendum` section (`resource-loader.ts:1214-1233`).

---

## 10. Evals (`ev/`)

- Framework: vitest + `vitest-evals` (`describeEval`, judges) + `autoevals` (`ev/package.json`).
- Harness (`ev/src/harness.ts:272-469`): per run, fresh temp root with isolated `HOME`, agent dir, workspace (seeded files), session dir; only the selected model's credential; real `AgentSession` with `thinkingLevel: "off"` and an explicit tool list; refuses runs with unexpected extensions loaded (`:357-363`). Input = string or steps `[{type:"prompt"}, {type:"reload"}]` (`:43,367-380`). Output = last assistant text, or a custom `output()` that inspects resulting runtime state (`:414-415`). Fails on non-`stop`/`toolUse` stop reason or empty text (`:238-255`).
- Task/grading styles:
  - exact assertion (`ev/evals/smoke.eval.ts:7-17`: "capital of France" → `Paris`, no tools).
  - outcome-state check with `StructuredOutputJudge({ match: "strict", allowExtras: false })` on runtime state after the agent configured something, e.g. model added to catalog, custom provider works against a local fake server that validates requests (`ev/evals/models.docs.eval.ts:25-39`, `custom-provider.docs.eval.ts:14-71`).
  - forced structured final answer: a custom `submit_*` tool with strict schema and `terminate: true` ends the run; grade via `toolCalls(result)` (`ev/evals/documentation-audit.eval.ts:9-41,59-64`).
- Metrics per run (`harness.ts:396-412`): input/output/cacheRead/cacheWrite/total tokens, tool calls, wall ms, estimated USD, `systemPromptSha256`; session JSONL attached as artifact (`:420-431`).
- A/B prompt experiments: "docs" evals run every case under `without_docs` (control: `<docs>` section stripped, `harness.ts:494-506`) and `with_docs` (treatment), N runs per variant, alternating order per run number to cancel order effects (`ev/src/plan.ts:39-59`), each in a hardened Docker container (`--read-only`, tmpfs, uid/gid drop, secrets mounted read-only; `ev/src/docker.ts:100-121`; tools limited to read/write/edit/grep/find/ls, `harness.ts:485`).
- Report (`ev/src/report.ts:21-83,283-357`): per eval set pass rates control vs treatment, `lift`, paired mean deltas of tokens/tool calls/time/cost, totals per variant, flags `no-lift`, `negative-delta`, `control-saturated`, `treatment-saturated`, `flaky` (same case flips across runs); blocked pairs listed with reasons.
- Scope: smoke + pi-self-knowledge tasks. No SWE-bench-style coding benchmark in-repo.

---

## Other notable details
- Tool-result images split for Anthropic error results (only text allowed) (`anthropic-messages.ts:1503-1523`).
- `read` path resolution retries macOS screenshot variants (narrow NBSP before AM/PM, NFD, curly apostrophe), strips leading `@`, expands `~` (`ca/core/tools/path-utils.ts:5-60`).
- `grep`/`find` auto-download `rg`/`fd` if missing (`ensureTool`, `grep.ts:119`).
- bash exposes `PI_SESSION_ID/PI_SESSION_FILE/PI_PROVIDER/PI_MODEL/PI_REASONING_LEVEL` env to commands (`bash.ts:186-212`).
- Anthropic OAuth "stealth mode" mimics Claude Code identity and tool names (`anthropic-messages.ts:95-99,1228-1235`) — do not copy.

---

## Ideas worth adopting in killi (ranked)

1. Stable, sectioned system prompt with no date/env noise; send later changes as section patches instead of rewriting the head — `ca/core/system-prompt.ts:128-229`, `ai/utils/text.ts:28-40`.
2. Tool-set deltas as transcript events + frozen top-level tool list (Anthropic `tool_addition`/`tool_removal` + deferred placeholder; "additions" fallback) to keep cache across tool changes — `ai/utils/transcript.ts:221-263`, `ai/api/anthropic-messages.ts:1265-1276`.
3. Three cache breakpoints (system, last tool, last message) with summaries sent uncached — `ai/api/anthropic-messages.ts:1243-1252,1529-1555,1659`, `ca/core/compaction/compaction.ts:626-632`.
4. MCP tools hidden by default behind one discovery/proxy tool (BM25 `tool_search` or code-mode) plus a ≤4KB server list in the prompt — `ca/core/mcp-servers.ts:43-52`, `ca/extensions/tool-search/tool.ts:195-215`, `ca/extensions/mcp/index.ts:155-215`.
5. Multi-edit `edits[]` against the original file, exact→normalized fallback that rewrites only touched lines, precise not-found/duplicate/overlap/no-op errors, terse success text — `ca/core/tools/edit-diff.ts:207-362`, `ca/core/tools/edit.ts:207`.
6. Argument-repair shims: `prepareArguments` for common shape mistakes, JSON control-char/escape repair, raw args echoed in validation errors — `ca/core/tools/edit.ts:103-134`, `ai/utils/json-parse.ts:33-120`, `ai/utils/validation.ts:317-352`.
7. Dual-limit truncation (2000 lines / 50KB) with actionable continuation hints (`offset=`, `limit=2×`, full-output temp file) and bounded-memory streaming — `ca/core/tools/truncate.ts:11-13`, `ca/core/tools/read.ts:177-203`, `ca/core/tools/output-accumulator.ts:35-256`.
8. Compaction: reserve 16k / keep 20k recent, cut only at user/assistant boundaries, structured Goal/Progress/Decisions/Next-Steps summary, iterative update, thinking dropped, tool results capped at 2000 chars, read/modified file lists — `ca/core/compaction/compaction.ts:126-130,446-579`, `ca/core/compaction/utils.ts:63-155`.
9. Error recovery: SDK retries off, own abortable backoff honoring retry-after; regex retry/overflow classification; failed attempt omitted from context; overflow → compact and retry once — `ai/utils/provider-retry.ts:25-128`, `ai/utils/retry.ts:7-121`, `ai/utils/overflow.ts:38-186`, `ca/core/agent-session.ts:3054-3097`.
10. Reasoning round-trip keyed by the arriving field name (`reasoning_content`/`reasoning`/`reasoning_details`), DeepSeek empty `reasoning_content`, cross-model thinking dropped, synthetic results for orphaned tool calls — `ai/api/openai-completions.ts:593-639,1320-1402`, `ai/api/transform-messages.ts:119-199`.

Runners-up: per-file mutation queue for parallel tool calls (`ca/core/tools/file-mutation-queue.ts:32-61`); skills as name+description+path only (`ca/core/skills.ts:358-388`); cache-miss accounting with 1024-token noise floor (`ca/core/cache-stats.ts:56-90`); cost-gated 1-token cache warming during long tool runs (`ca/core/cache-warmer.ts:15-32,378-400`); paired A/B prompt evals reporting lift + token/cost deltas (`ev/src/plan.ts:39-59`, `ev/src/report.ts:283-357`).
