# killi 的 agent-to-agent 能力审查与 A2A 对齐计划

日期：2026-11-10 · 基线：0.6.71（`main`，git clean）· 参考：A2A v1.0 规范
（a2a-protocol.org/latest/specification）、a2aproject/a2a-rs

## 1. 审查结论：现状是「同一台机器、同一个用户」的三层点对点协议

### 1.1 已有的三层

| 层 | 实现 | 证据 |
| --- | --- | --- |
| 发现/在场 | 每个顶层会话写 `~/.killi/live/<pid>.json`（pid、cwd、git work tree、branch、model、log、working、since、prompt、socket），staging 改名 + `flock` 独占即活性；`killi sessions [<pid>] [--json]` 列出或查状态 | `live.rs:141-300`、`live.rs:434-518` |
| 交互 | 三种请求走 `$TMPDIR/killi-live-<uid>/<pid>.sock` 一行 JSON：`Status`（不调模型）、`Ask`（侧请求，答完不留历史，300s 上限，并发 2）、`Tell`（排队成下一条 reminder，空闲时给用户看，每 prompt 上限 3 条） | `live.rs:302-431`、`tui/peers.rs:7-83`、`tui/mod.rs:136,140` |
| 模型面协同 | `tree_note` reminder 告诉模型同一 work tree 里还有谁、别动它的改动、怎么 ask/tell；`peer_guard` 在 peers 工作时拒绝 `checkout/switch/restore/reset/clean/stash` | `live/note.rs:5-22`、`peer_guard.rs:10` |

进程内委派（`agent` / `agent_swarm`，最多 128 个 subagent）不是 agent-to-agent：同进程、同模型、同缓存前缀。

killi 只是 MCP **客户端**（`broker.rs` 代管 stdio server），自身不对外提供任何服务端点。

### 1.2 与 A2A v1.0 的差距

| A2A 标准能力 | killi 现状 | 差距 |
| --- | --- | --- |
| Agent Card（`/.well-known/agent-card.json`：name/description/version/capabilities/skills/defaultInputModes） | 只有本地 JSON 文件，不可拉取、无能力声明、无 skills | 无自描述文档 |
| Task 模型（`taskId`/`contextId`、`TaskStatus.state`、`artifacts`、`history`） | 无任务概念：`ask` 是无状态侧请求，`tell` 是一条 reminder | 无法指认、跟踪一个工作单元 |
| 任务生命周期（submitted/working/input-required/completed/failed/canceled/rejected） | 只有 working/idle 两种会话态 | 无状态机 |
| 异步长任务（`SendMessage` 阻塞或 `returnImmediately`、`GetTask`、`ListTasks` 过滤分页） | `ask` 阻塞 300s；没有「派活 + 轮询」 | **最大缺口**：只能问问题和留言，不能委派任务 |
| `CancelTask` / `SubscribeToTask`（SSE 事件流） | 无取消、无流 | 派了活停不下来、看不到过程 |
| 标准传输（HTTP JSON-RPC + SSE；另规定 gRPC、HTTP+JSON） | unix socket + 文件注册表，仅本机同用户（`peer_cred().uid()` 校验） | 不能跨机、跨容器、跨用户 |
| 安全（card 里声明 securitySchemes、extended card、tenant） | 「同 uid 即信任」 | 远程场景无鉴权 |
| push notifications / webhook | 无 | 长任务断线即失联 |
| 客户端角色（作为 A2A Client 调远程 agent） | 完全不能：只能 ask/tell 另一个 killi | 无法编排外部 agent |
| 版本协商（`A2A-Version`） | `Envelope.v` 写了不读，靠「未知字段忽略」 | 无兼容性契约 |

规范允许自定义绑定（§12：自定义绑定必须自己定义类型、服务参数、错误与流式映射，且操作等价）。killi 的 unix socket 就是一个合法自定义绑定——本地场景它比 HTTP 更好（无端口、无 TLS、uid 即鉴权）。**结论：保留本地绑定，把数据模型和操作向 A2A 对齐，远程场景另加标准 HTTP 绑定。**

## 2. 约束（决定不能照搬的部分）

1. **提示词缓存**：系统提示与工具列表在会话内固定。新增模型面能力只能改「追加的 reminder」或用 bash 调 CLI，**不加新工具**（`main.rs` 的测试把提示词压在约 3k token 内）。
2. **体积与依赖**：单个约 7 MB 二进制、约 18 MB 内存是项目核心卖点（README）。reqwest 0.12（rustls、http2、stream）已在依赖里，客户端侧零新增依赖。
3. **历史只追加**：所有给模型的说明都以 `<system-reminder>` 用户消息追加在末尾（`live/note.rs` 即此风格）。
4. **单线程 tokio + TUI select 循环**：并发原语要沿用现有形态（`peer_asks` 那种在 select 里 poll 的 future、`oneshot`/`mpsc` 通道）。

## 3. 目标 CLI 面（A2A 操作 → killi 命令）

```
killi sessions [<pid>] [--json]      # 发现 + 摘要（≈ Agent Card 摘要 + ListTasks）
killi card <pid> [--json]            # 会话的 Agent Card
killi send <pid|url> <task> [--async] [--timeout <s>] [--after <id>]
                                     # ≈ SendMessage；默认阻塞到终态并打印报告，--async 只回 task id
killi tasks [<pid>] [--json]         # ≈ ListTasks（逐会话询问后合并）
killi task <id> [--json] [--follow]  # ≈ GetTask；--follow ≈ SubscribeToTask（事件流到终态）
killi cancel <id>                    # ≈ CancelTask
killi ask <pid> <question>           # 不变（≈ SendMessage 不落任务，返回 Message）
killi tell <pid> <message>           # 不变（≈ 同一 context 的后续留言）
```

任务 id 由**服务端**生成，形如 `<pid>-<8hex>`：前缀即属主，`killi task`/`cancel` 无需全局注册表即可路由。远程目标（Phase 4）没有 pid 前缀，写作 `killi task <url> <id>` / `killi cancel <url> <id>`。任务态映射到 A2A 枚举：submitted（已受理未起跑）、working（任务 agent 在跑）、input-required（报 `Status: blocked`）、completed（`Status: ready`）、failed（`Status: failed` 或出错）、canceled（`killi cancel`）、rejected（同级编辑守卫/预算/`peers = false`）。auth-required 本地无意义，不实现。

**副产物直接复用现有约定**：`agent/subagent.rs:413` 的 `Status: ready|blocked|failed|waiting` 就是状态机来源；artifact = 报告正文（text part）+ `{log, requests, tool_calls, ms}`（data part），失败时像 `swarm.rs:77` 那样给出 `Partial log:` 指针；`--after <id>` 把上一个任务的 blocker 与日志路径写进新 prompt（A2A 的「同一 context 新任务」，不保留子会话历史）。

## 4. 分阶段实施

### Phase 1 — Agent Card（只读，不动协议）✅ 已完成（0.6.72 起）

- 改动：`live.rs` 加 `card(entry, status)`，从 `Entry` + `Request::Status` 生成 card JSON：`name`（`killi <pid>`）、`description`（当前 prompt/goal）、`version`（killi 版本 + 协议版本 `killi-live/1`，Phase 2 再升到 2）、`capabilities`（ask/tell）、`skills`（可被问/可被派的事：改代码、跑测试、查日志）、`state`（idle/working）、`defaultInputModes/OutputModes`（text/plain、text/markdown）；`main.rs` 的 `USAGE` 与 dispatch 加 `card`。
- 复用缝：`live::describe`、`status_text` 已有全部字段；`Entry` 加一个 `tasks: usize`（在跑的 peer 任务数，旧版忽略未知字段的既有兼容规则继续成立）。
- 验收：`killi card <pid>` 与 `--json` 输出可被脚本解析；`live/tests.rs` 单测覆盖字段与缺失 socket 的 headless 会话；`scripts/live_check.py` 加一条检查。
- 非目标：签名（JWS）、extended card、skills 的自动推导。

### Phase 2 — 任务模型与委派（核心缺口）✅ 已完成

- 改动：
  - **先拆 `live.rs`**：它已有 635 行（AGENTS.md 的上限约 500），把 `command()` 与列表/状态文本移到 `live/command.rs`，从 `live` 根再导出，旧路径保持不变；否则新增五种请求会越线。
  - `live.rs`：`Request` 加 `Send{from,text,timeout,after}`、`Get{id}`、`List`、`Cancel{id}`、`Follow{id}`；`Envelope` 升到 `v: 2`，`v: 1` 的 status/ask/tell 照旧服务，未知 `type` 回明确错误（沿用现有错误形状）。流式应答：`Incoming` 增加一种流式形态（连接任务持有 `mpsc::UnboundedSender<String>`，把事件一行一条写回，终态后关闭），`Follow` 用它而不是 `oneshot`。
  - 服务端（被派活的一方）：**不能直接调 `Agent::subagent`**——它在 `agent/subagent.rs:321` 是 `pub(super)` 且要一个在跑的 `&mut Agent`，而 `App::on_peer`（`tui/peers.rs:7`）拿不到 Agent（TUI 的 agent 在 `tui/run.rs` 的 select 里单独持有，两侧只通过 `Controls` 解耦，`ask` 走的就是这条解耦路）。正确缝：`Aside`（`agent/types.rs:112`）已有 client/tools/ctx/历史的 Weak，补一个 `Rc<Settings>`（在 `Agent::share()` 里填），再加一个 agent 模块根的 `pub(crate)` 入口来组装任务 agent（`Agent::assemble` 是 `pub(super)`，同模块可直接用）：系统提示取历史 `messages[0]`、`Session::create(cwd)` 独立成日志、`interactive: false`、与普通会话同权（可用 swarm，由并发上限和超时兜底），跑完按 `subagent_prompt` 的 Status 约定回报；`controls.here` 必须一并带上，否则 `peer_block`（`agent/dispatch.rs:292`）对任务 agent 不生效。`App` 侧只加任务注册表 + 在 select 里 poll 的 future，形态同 `peer_asks`；单会话并发上限 2（新常量，与 `MAX_PEER_ASKS` 并列），默认超时 600s。
  - 客户端：`main.rs` dispatch 加 `send/tasks/task/cancel`；`live::command` 里目标解析为「pid 或 URL」（Phase 4 前只接受 pid）；`--after <id>` 把上个任务的 blocker 与其日志路径写进新 prompt。
- 安全与边界：headless（`-p`）会话仍然无 socket，一律拒绝（错误信息沿用现有文案）；同 work tree 的编辑冲突沿用现有机制（task note 带 `tree_note` 信息 + `peer_guard`），不新增守卫。
- 验收（`scripts/live_check.py`，脚本 provider 已在跑两个真实会话）：A 派任务给 B → B 以任务 agent 跑完 → `--follow` 看到 submitted→working→completed 与 artifact；`--async` 只回 id，随后 `killi task <id>` 拿到终态；运行中 `killi cancel` 生效；`killi tasks --json` 合并两个会话的任务态；`--after` 续跑一个 blocked 任务；派给 headless 会话被拒；peer 的 tree-moving git 在任务期间仍被拒。单测：id 解析与路由、状态机映射、`v:1` 兼容、超时与并发上限。
- 非目标：保留子会话历史以恢复对话（用 `--after` 代替）、contextId、artifact 里的变更文件清单、跨机。

### Phase 3 — 模型面与 TUI（未开始）

- 改动：`live/note.rs` 的 `tree_note` 增加一句（约 20 token）说明 `killi send`/`killi tasks`/`killi cancel`；`tui/peers.rs` 给入站任务加行（像 `tell` 那样显示，含取消提示），任务终态在 TUI 留一行结果。
- 验收：`live_check.py` 检查 reminder 里出现新命令、B 的终端显示入站任务与其结果；`scripts/frames.py` 按默认三档（118x40 / 80x24 / 60x24）重画（TUI 行变了就必须画）；clippy + 全量测试。
- 非目标：新工具、`ask_user_question` 式的任务审批流（自动受理，理由同 `tell`：另一会话本就能指示本会话干活；`peers = false` 是总开关）。

### Phase 4 — 作为 A2A Client 调远程 agent（零新依赖）

- 改动：新模块 `a2a.rs`（`main.rs` 声明，测试按惯例进 `a2a/tests.rs`），用现有 reqwest 实现：`GET <url>/.well-known/agent-card.json`（解析 `supportedInterfaces`、`capabilities`）、`POST` JSON-RPC `SendMessage` / `SendStreamingMessage` / `GetTask` / `ListTasks` / `CancelTask` / `SubscribeToTask`，`A2A-Version: 1` 头，SSE 帧解析（`data: {"jsonrpc":…,"result":{StreamResponse}}`），错误码 -32001/-32002/-32004 等映射成可读错误；CLI 侧 `killi card <url>`、`killi send <url> …`、`killi task <url> <id> [--follow]`、`killi cancel <url> <id>`，目标解析从 `live::command` 移到共用的「pid 或 URL」一处。
- 复用缝：`tools/web/fetch.rs:22` 的 reqwest 客户端构造与超时/限长处理、`llm::truncate`、`tools::spill`。
- 验收：`live_check.py`（或 `scripts/a2a_check.py`）内置一个脚本化的 A2A server（Python `http.server`：card + JSON-RPC + SSE），跑通 send 阻塞/异步、follow 事件流、get、cancel，以及一个标准错误路径；对真实远程 agent 未验证（本机无网络）。
- 非目标：gRPC 绑定、HTTP+JSON/REST 绑定、push notifications/webhook、OAuth（只支持 `Authorization: Bearer` 与 apiKey 头）、tenant、mTLS。

### Phase 5 —（待决策，不在本次范围）killi 作为 A2A Server 对外提供 HTTP

两条路：(a) 引入 `a2a-server-lf`（axum 0.8 + tower + hyper + reqwest 0.13 + rustls），二进制大约 +1~2 MB，并带入 0.x SDK（111 star、69 open issue、无文档站、只认 v1.0）；(b) 在 tokio 上手写 HTTP/1.1（只支持 `Content-Length` 与 SSE，约 200 行），零新增依赖但与 broker.rs/live.rs 一样要自己维护。**建议：等 Phase 4 证明外部互调确有需要再做**，届时按「是否接受 axum」重新决策。

## 5. 依赖决策：Phase 1–4 不引入 a2a-rs

- 类型面很小（Task/Message/Part/TaskStatus/AgentCard 中我们只用一部分），`a2a-lf` 虽只依赖 serde，但 `a2a-server-lf` 牵 axum/tower/hyper，`a2a-pb` 带 `protoc-bin-vendored` 构建依赖，`a2a-grpc` 牵 tonic——与「单个小二进制」直接冲突。
- SDK 处于 0.x、API 仍在动（A2A 1.1、`SendLiveMessage` 未实现；stdio/WebSocket 绑定未实现；OAuth 未实现）。
- 自有映射代码约 300~400 行，控制在 `live/` 拆出的模块与 `a2a.rs` 里，随规范演进手工对齐。
- 代价（明示）：没有 gRPC/REST 绑定，规范升级要手工跟。

## 6. 明确不做

gRPC / REST 绑定、push notifications、OAuth2/mTLS/tenant、Agent Card 签名与 extended card、跨用户与跨机（Phase 4 之前）、任务历史恢复对话、变更文件作为 artifact、`peers = false` 之外的新配置键、新工具。

## 7. 验证方式（每阶段）

- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- Phase 1–3：`scripts/live_check.py`（两个真实会话 + 脚本 provider）加检查项；动了 TUI 行就 `scripts/frames.py <out>/` 重画（默认即 118x40、80x24、60x24 三档，`--sizes` 可覆盖）。
- Phase 4：脚本化 A2A server 的端到端检查；对真实远程 agent 标注「未验证」。
- 文档同步：`main.rs` 的 `USAGE`、README 的 peers 段、`docs/decisions.md` 新增编号决策（记录本地绑定为何保留、为何不引 a2a-rs）、`CHANGELOG.md` 的 Unreleased。

## 8. 顺序与工作量（粗估）

1. Phase 1（0.5 天，只读，风险最低，先交付）
2. Phase 2（2~3 天，核心；协议 + 服务端 + CLI + 端到端检查）
3. Phase 3（0.5~1 天，提醒文案 + TUI 行 + frames）
4. Phase 4（1.5~2 天，reqwest JSON-RPC/SSE + 脚本 server）
5. Phase 5：决策点，本次不做。

## 9. Phase 1–2 实施记录（2026-11-10）

实现落在：`live/card.rs`（card 文本与 JSON）、`live.rs`（`TaskState`/`TaskView`/`Request` 新变体/
`Reply`、`serve` 的流式分支、`follow()` 客户端、四个新子命令）、`agent/task.rs`（任务 agent 的
组装）、`agent/types.rs` + `agent/lifecycle.rs`（`Aside` 增加 `backup` 与 `settings`）、
`tui/tasks.rs`（任务注册表、runner、sink、四个处理函数）、`tui/peers.rs`（分发）、
`tui/state.rs`（`PeerTask`/`PendingPeerTask`）、`tui/input.rs` + `tui/run.rs`（轮询）。

与计划的偏差，都是有意的：

1. **`Entry` 没有加 `tasks: usize`**：Phase 1 时没有任务可数，是死字段；`killi sessions` 显示
   任务数留到 Phase 3。
2. **card 放进新模块 `live/card.rs`**，没有往 635 行的 `live.rs` 里堆（AGENTS.md 的约 500 行
   规则），这也顺手完成了 Phase 2 计划里的「先拆 live.rs」前置工作。
3. **任务 id 是 `<pid>-<n>`（会话内计数器）**，不是 `<pid>-<8hex>`：路由只需要属主前缀，计数器
   更短也更好读。
4. **任务 prompt 不再自带 `tree_note`**：`Agent::peers_note`（`agent/turn.rs:285`）本就会在
   depth 0 宣布同树会话，手工再贴一遍会让同一条 reminder 在历史里出现两次（脚本 provider 还会把
   以 reminder 开头的消息当成没有 prompt，回答退化成 `Done: `）。`peer_guard` 仍靠
   `controls.here` 生效。
5. **任务 agent 的 `Aside` 还带了 `backup`**：与 subagent 一致（决策 59），被限流的任务落到备用
   模型而不是失败。
6. **TUI 只加了 notice（任务到达/终态各一行）**，没有做 Phase 3 的行渲染；模型面的
   `tree_note` 也还没提 `killi send`/`killi tasks`，所以**模型还不知道能派任务**——那是 Phase 3。
7. **`view` 是 `Rc<RefCell<TaskView>>`，与 runner 共享**：状态和「正在做什么」在运行中就要能被
   `Get`/`Follow` 读到；取消用 `PeerTask.stopped` 标志区分「已取消」与「已跑完」，因为 runner 会在
   返回前把终态写进同一条记录。
8. **`live.rs` 的 `command()` 已约 900 行**：Phase 3 应把四个新子命令的文本渲染（`tasks_text`、
   `task_line`、`task_detail`、`event_text`、`next_event`）拆去 `live/command.rs`。

验证：`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cargo test --workspace`
734 通过（新增 8 个单测）；`scripts/live_check.py` 38 项全过，其中任务相关 10 项为新增
（派活并拿报告、`tasks` 列表、`task` 详情、`--async` 只回 id、第三个被拒、`cancel` 生效且状态保持、
`--follow` 看到步骤与终态、`--after` 续跑、按 pid 过滤、TUI 有提示）。

## 10. 审查修复记录（2026-11-10，Phase 1–2 本地改动的 code review）

1. **任务日志改放宿主会话目录**：`Session::task(dir, n)` 写 `<session>/task-N.jsonl`（与 `agent-N.jsonl`
   并列），不再用 `Session::create(cwd)`——否则任务日志是项目目录下的顶层日志，`killi -c` 会续到任务对话，
   `/resume` 里每个任务占一行。
2. **`CRASH_LOG` 只由 `Agent::new` 与 resume 设置**（`note_crash_log`），`assemble` 不再碰它；任务 agent 走新的
   `Agent::begin`（`new` 的后半段），不会抢走宿主会话的崩溃记录。
3. **`TaskState` 序列化改 `kebab-case`**：线上是 `input-required`，与 `as_str()`、CHANGELOG、A2A 一致。
4. **`outcome_of` 去装饰后再读**：取最后一个非空行，忽略反引号/加粗/大小写/词后的标点，
   `` `Status: failed` … `` 不再被当成 completed。未知的词仍算 failed。
5. **`/undo` 的警告覆盖任务**：本会话跑过的任务（`App::tasks_since`）和别的会话在跑/跑过的任务
   （`Entry` 新增 `tasks`、`task_since`，`Entry::worked_since`）。未处理：任务 agent 不等回合快照
   （`snapshot_wait`），快照拍摄瞬间的并发写入与另一个会话在同树写入是同一类竞争。
6. **card 的 tasks/streaming 取自对方的 Status**：Status 回复新增 `tasks`（在跑的任务数），有这个字段才报
   `tasks: true` 与 `killi-live/2`；旧版本会话和 headless 仍是 `killi-live/1`、`tasks: false`。
7. **`killi task --follow`**：`{"error": …}` 行报错退出；终态不是 completed 时退出码非零（同 `killi send`）；
   终态事件同时有 message 和 report 时两者都打印；`--json` 时逐行输出原始事件；不再有每事件 630s 的固定超时
   （任务自身的超时由宿主会话执行，连接随任务结束而关闭）。
8. **任务提示行不切断段落**：模型正在写一段话时，到达/结束/取消的提示进 `held_asides`，段落结束后再打印。
9. **`--timeout` 校验**：必须是 1..=86400 的秒数，否则报错；`--after` 缺值报错；宿主侧同样钳制，
   客户端等待用 `saturating_add`。
10. **拆文件（纯移动）**：`live.rs` → `live/task.rs`（任务类型）、`live/wire.rs`（请求与 socket）、
    `live/command.rs`（CLI 与文本）；`tui/tasks.rs` 的 runner → `tui/tasks/run.rs`。第 9 节偏差 8 已完成。

验证：clippy 干净；`cargo test --workspace` 742 通过；`scripts/live_check.py` 52 项全过（新增：任务日志位置、
`input-required` 的终态与线上拼写、follow 不存在的任务、非法 `--timeout`、undo 对任务的警告、card 的 tasks 能力）。
