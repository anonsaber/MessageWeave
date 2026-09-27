# AGENTS.md — MessageWeave 开发守则

> 本文档是后续任何 AI coding agent 在本仓库工作的**操作守则与硬性约束**。
> 架构 / 模块接口 / 状态机 / 数据流 / 测试 / 实施阶段以 [docs/design.md](docs/design.md) 为准；
> 部署运维与发布（通用 HTTPS-only Docker / Secrets / Redis 状态层 / Webhook / Push / 外部 Cron 对账 / CI）以 [docs/deployment.md](docs/deployment.md) 为准。
>
> 三份文档职责分离，改动时请遵循 [§6 文档边界](#6-文档边界与引用关系)，避免内容重复堆砌与跨文档冲突。
>
> **跨文档引用一律用稳定 ID**（`C-` / `NG-` / `MOD-` / `FLOW-` / `REQ-` 等），**不用章节号（§x.y）**；定义点位置与一句话说明见 [§7 跨文档引用索引](#7-跨文档引用索引表)。

---

## 1. 项目目标

构建一个 Rust 写的 Telegram 机器人，作为 Stalwart JMAP 邮箱的**个人邮件助手**（详见 [design.md §1](docs/design.md#1-任务与范围)）：

- 通过 Telegram 命令查询/阅读邮件、查看文件夹、发送邮件、管理关键词等。
- 利用 **JMAP Push HTTPS 回调**（唯一实时通道）+ 外部 Cron 对账（建议 5–10 分钟），新邮件到达时主动推送到 Telegram；**不使用** EventSource/SSE/长轮询（`NG-POLLING-SSE`/`NG-LONG-POLLING`，见 deployment.md `C-NO-LONG-CONN`）。
- **单账户**实现（`REQ-SINGLE-ACCOUNT`），多账户 = 多个 bot 实例；部署为**通用 HTTPS-only Docker 容器**（短请求模型，平台提供公网 HTTPS URL `C-HTTPS-URL`），详见 deployment.md。
- 可靠性目标：**通知可用性 ≥ 99.9%**，允许少量延迟（`NFR-NOTIFY-SLA`，策略见 deployment.md §6.4/§6.5）。

技术栈：`jmap-client`（rustls，**App Password + Basic** `C-AUTH-APP-BASIC`）+ Telegram Bot API（**版本不锁定**；渠道在 `src/channel.rs` 用 `reqwest` 自研实现，`teloxide` 未引入，见 `ARCH-DEPS-STAGE4`）+ tokio + **外部 Redis（用户托管 + AOF）**（`C-REDIS-MANAGED-AOF`；**服务端版本不锁定**，只要求能力集：Streams、XPING/PING、SET NX EX）。**未写代码前不得初始化 cargo 工程之外的内容**（仅文档阶段）。

---

## 2. 硬性安全边界（不可违反）

> 任何改动都不得破坏以下不变量。实现时必须有对应断言测试（见 §5 与 [design.md §9.3](docs/design.md#93-关键不变量测试)）。

1. **查看原文 = JMAP 直取，绝不经 AI**：`/read` 与任何"看正文"路径调用 `JmapService::read_email`，LLM 不在查看路径上。
2. **AI 仅在用户明确要求并确认后接触正文**：用户明确发起"分析/总结/翻译"意图**并再次确认**（点 `[AI 总结]` / `/summarize`）后才把正文交给 LLM；未经确认，LLM 看不到正文。意图或目标不唯一 → 追问确认，禁止猜测执行。
3. **长邮件禁止发送完整原文**：正文 > 阈值（默认 4000 字符，见 [design.md §12.3](docs/design.md#123-正文获取策略与长邮件处理需求-1235)）不发全文，只发预览 + `[AI 总结]`/`[继续查看原文(截断)]` 选项；"继续查看"也是截断版并标注"完整请电脑查看"。
4. **新邮件通知只含元数据**（`SAF-NOTIFY-META`）：发件人 / 主题 / 时间（+附件数），绝不含正文；正文访问须用户显式读信/分析。
5. **附件默认不下载、不预载**：仅用户显式请求才触发 `Blob/get`；AI 永不接收附件内容。
6. **分析 / 摘要结果不持久化**：不写 Redis、不落盘、不缓存，即取即弃。
7. **AI 失败约 3 次 → 熔断 + 用户确认回退**：回退为规则模板 + 原文直取，标注"AI 不可用"；恢复需用户选择。
8. **密钥不出仓库、不出日志**：`secrecy::SecretString` 包裹、`tracing` 屏蔽 `Authorization/password/token`；秘密一律运行期注入（`C-NO-SECRET-IN-IMAGE`，deployment.md）。变量命名以 `src/config.rs` 为准，模板见根目录 [`.env.example`](./.env.example)（**仅占位符，禁放真实值**）。
9. **聊天白名单（硬约束 `SAF-CHAT-ALLOWLIST`）**：`CHAT_ALLOWLIST` 必填；任何入站事件在做任何 JMAP/AI 调用或状态变更**之前**必须先校验 `chat.id ∈ CHAT_ALLOWLIST`，否则直接拒绝终止。（阶段0 已完成解析骨架；强制拒绝随阶段2 渠道接入落地。）
10. **LLM 无工具权**：AI 输出仅为文本；删除/归档/下载/发送等动作只能由用户显式指令触发，绝不由 LLM 输出驱动（邮件正文视为不可信数据，正文内指令无效）。
11. **无长连接**：运行模式仅 `webhook`/`reconcile`/`health`，禁止 SSE/EventSource/WebSocket/Telegram 长轮询（`C-NO-LONG-CONN`/`NG-POLLING-SSE`/`NG-LONG-POLLING`）。
12. **状态仅外部 Redis（用户托管 + AOF）**：会话/去重/Redis Streams/熔断计数/sinceState 一律走外部 Redis（`C-REDIS-ONLY-STATE`/`C-REDIS-MANAGED-AOF`），禁止 SQLite/本地卷作持久层（`NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`）；Redis 丢失由 JMAP 对账 `FLOW-RECONCILE` 重建 sinceState，**事实源在 JMAP**。
13. **正文仅在用户明确允许后才外发 AI**（`REQ-AI-EXTERNAL-CONSENT`）：默认不外发；发送前须用户显式同意（与第 2 条叠加）。
14. **单账户**（`REQ-SINGLE-ACCOUNT`）：一个实例只接一个 Stalwart 账户；多账户 = 多个 bot 实例，不做多账户单实例。
15. **三入口鉴权 fail-closed（硬约束 `SAF-AUTH-RECONCILE`/`SAF-AUTH-TG-WEBHOOK`/`SAF-AUTH-JMAP-PUSH`）**：三条写路径必须先鉴权——`/reconcile` 校验 `Authorization: Bearer RECONCILE_TOKEN`、`/webhook/tg` 校验 `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET`、`/push/jmap` 校验请求体 `pushSubscriptionId` + `verificationCode`——按 `pushSubscriptionId` 查 Redis 短期验证状态（值存 `session_digest` 摘要，见 `src/notify.rs` `jmap_push`），状态缺失或摘要不符即拒绝；`/reconcile`、`/webhook/tg` 的凭证比较用**常数时间**（`subtle`），失败 `401` 且**在鉴权通过前不得产生任何副作用**；三密钥必填，缺失即启动失败，**禁止任何"未配置则放行"降级**。`/healthz`（`ARCH-HEALTHZ`）与 `/ready` 为**公开探针**（`SAF-PROBE-PUBLIC`：无鉴权、仅健康状态、不含敏感信息）；**`/ready` 已做基础探测**（`ARCH-READY-BASELINE`）：配置完整性 + Redis 可达性，不就绪返 `503`（标准错误 envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`；Uptime Kuma 按状态码 200/503 监控，不受响应体影响）；**仍不做** JMAP session / TG getMe 等端到端探测（余下范围见 `GATE-READY-DEPS`）——**禁止**声称 `/ready` 已验证端到端依赖。
16. **多实例高可用（`ARCH-LB-WORKER`）**：允许同一镜像跨多个 serverless 平台实例化、前置免费 Cloudflare Worker 做唯一入口与故障转移（`C-LB-SINGLE-REG-URL`），共享同一 Redis（`C-REDIS-ONLY-STATE`）。**信任模型为透传（`SAF-LB-PASSTHRU`）——后端鉴权不可省**（小平台无防火墙/ACL，后端入口可能被公网直连）；多实例**必须共享同一组 `SAF-AUTH-*` secret**（`C-LB-SHARED-SECRETS`）。`/reconcile` **禁止多实例并发**，用 Redis 锁单实例执行（`SAF-RECONCILE-LOCK`）；Streams 用**同一消费组**分摊（`MOD-STREAMS-GROUP`）；Worker 可提供**聚合健康视图**（`MOD-HEALTH-AGG`）。**Redis 单点故障不在本方案范围**（`NFR-HA-MULTI-INSTANCE`，用户外部解决）。业务代码无需为 LB 改动。详见 deployment.md §10。
17. **生产红线（无数据库 / 无本地写入 / 标准输出日志，`C-NO-DB` / `C-NO-LOCAL-WRITE` / `C-LOG-STDOUT-ONLY` / `SAF-LOG-PURITY` / `C-NO-STATEFUL-RECOVERY`）**：
    - **不使用任何数据库**（`C-NO-DB`）：无 SQLite / Postgres / MySQL / 嵌入式数据库；Redis 是唯一生产状态存储（`C-REDIS-ONLY-STATE`）。应用**不自建、不连接第二个数据库实例**。
    - **禁止本地文件/目录写入**（`C-NO-LOCAL-WRITE`）：无日志文件、无数据文件、无临时缓存、不挂载本地卷（`NG-LOCAL-VOLUME`）。
    - **日志只写 stdout/stderr**（`C-LOG-STDOUT-ONLY`）：容器/平台负责采集落盘；禁用 `rolling-file` / `FileAppender` 等文件日志后端。
    - **日志与 Redis 写入内容约束**（`SAF-LOG-PURITY`）：仅限结构化事件、计数、时间戳、脱敏后的请求摘要；**禁止**写入密钥原文、JMAP 邮件正文、AI 请求/响应内容、附件内容。
    - **禁止依赖进程内状态做生产恢复**（`C-NO-STATEFUL-RECOVERY`）：任何"重启续跑"（去重、sinceState、Streams 断点、熔断计数、会话）必须由外部 Redis + JMAP 对账（`C-REDIS-ONLY-STATE` / `FLOW-RECONCILE`）实现；进程内缓存仅为性能优化，**丢失必须安全可重入**。
18. **无数据库/无本地写入/标准输出日志 = 发布门禁**：发布前必做 deployment.md §8.2 的 6 项红线自检（镜像无 DB 引擎、无本地可写挂载、日志仅 stdout/stderr、日志/Redis 无敏感数据、重启恢复不依赖进程内状态、Redis 由外部提供）；任一失败禁止发布。

### 2.1 代码规模（软性指导，非硬限制）
- 单个 `.rs` 文件原则上不超过 **500 行**；这是**软性指导**而非硬性门禁。
- 复杂度、内聚性与可维护性优先：禁止为了凑 500 行制造奇怪的拆分（如过度抽象、跨文件跳转式"减肥"）。
- 若要拆分，只按职责/内聚边界拆；超过 500 行本身不是错误，但**需要在 PR/提交说明中写明原因**（如"本文件保持命令表 + 全部子命令处理，超行因命令数量，拆开反而分散"）。

### 2.2 代码注释与文档一致性（必须）
- 代码必须包含与设计文档匹配的必要注释；注释至少覆盖非显而易见的架构边界、状态流转、可靠性取舍、安全不变量和外部协议约束。
- `domain/`、`channel/`、`notify/`、Redis Streams、`sinceState`、AI 确认/回退、长邮件与附件处理等关键路径，必须能从代码注释追溯到 `design.md` 或稳定语义 ID（`C-*`、`REQ-*`、`FLOW-*`、`SAF-*`）。**注释中的稳定 ID 必须取自 §7 索引所用的前缀集**（`C-`/`NG-`/`MOD-`/`FLOW-`/`REQ-`/`SAF-`/`NFR-`/`GATE-`/`BOUND-`/`ARCH-`）；**禁止自造未登记 ID**（如 `JMAP-n`、`DATA-n`、`JMAP-*`、`DATA-*`）——新增语义需先在 §7 登记再引用。
- 设计规则、安全边界或数据流变更时，必须同步更新受影响的代码注释和对应文档；禁止保留与实现不符的注释。
- 注释应解释“为什么”和约束，不要逐行复述代码；简单自解释代码不要求添加废话注释。
- Code review/测试应检查关键模块注释是否与文档一致；注释缺失或过时视为实现不完整，但不要求为每个函数添加固定格式的注释。

---

## 3. 实现顺序

按 [design.md §10](docs/design.md#10-分阶段实施计划) 推进（每阶段验收标准见对应小节）：

```
阶段 0  脚手架 + HTTPS 入口骨架（已完成，待过 P0 门禁 GATE-P0）
        —— 交付：cargo 工程 + 模块骨架(main/config/error/domain/channel/notify)
           + 单端口 axum 路由占位(/webhook/tg /push/jmap /reconcile /healthz /ready)
           + /healthz=liveness；/ready 阶段0 交付为占位 200，现已演进为基础探测（ARCH-READY-BASELINE：配置完整性 + Redis 可达性，不就绪→503；仍不做端到端探测，余下见 GATE-READY-DEPS）
           + env 配置骨架 + 最小测试；实际依赖：axum 0.8 / serde / serde_json / thiserror / secrecy / subtle / url / tokio / tracing；jmap-client =0.4.2（default-features=false, features=["async","rustls"]）
           + 入口鉴权已落地：/reconcile(Bearer) /webhook/tg(secret 头) /push/jmap(verificationCode)，fail-closed SAF-AUTH-*
阶段 1  JMAP 只读（`jmap-client` 0.4.2 **已引入**，`ARCH-DEPS-STAGE1`；R1 入口鉴权已在阶段0 落地，不重复实现）
          + `JMAP_SESSION_URL` 归一化（`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）：接受服务基地址或完整 /.well-known/jmap，交给 jmap-client 前归一化为 origin/base；仅 HTTPS、禁内嵌凭据
          + `read_email` 多 part 原文拼接（`REQ-JMAP-RAW-MULTIPART`）
阶段 2  渠道适配骨架（Channel/Notifier/MessageAdapter + 首个渠道 Telegram；CHAT_ALLOWLIST 强制 + throttle；/start /folders /list /read）
阶段 3  发送 + 状态（send_email draft+submission；/send FSM；/flag）
阶段 3.5 LLM 门面 + 回退（见 design.md §12.9）
阶段 4  实时推送（Push 回调 + Redis Streams worker + 外部 Cron 对账 + sinceState→Redis）
阶段 5  搜索 + 搜索片段 + 打磨
```

> **阶段门禁**：`GATE-P0`（阶段0 P0 门禁，8 项）与 `BOUND-STAGE1`（阶段1 推进边界）定义见 design.md §10.0。**未过 `GATE-P0` 不得进入阶段1**；阶段0 业务体为无副作用占位，**不应暴露公网**。**注意**：三条写路径的**入口鉴权 `R1` 已在阶段0 落地**（`SAF-AUTH-*`，fail-closed），阶段1 不再重复实现，也**禁止放宽/绕过**。
> **依赖引入节奏**：`jmap-client` **已引入**（`=0.4.2`，实际 `default-features = false, features = ["async","rustls"]`，`ARCH-DEPS-STAGE1`：版本与 features **以 `Cargo.toml` 为准**；⚠️ 其默认 features `["async","websockets","aws_lc_rs"]` **含 WebSocket 栈**，故须关闭默认 features 且不选 `websockets`，以遵守 `C-NO-LONG-CONN`）；`redis`(`0.27`) / `reqwest`(`0.13`) **已加入 Cargo.toml**（阶段3.5/4 已落地，版本**以 `Cargo.toml` 为准**，`ARCH-DEPS-STAGE4`）；`teloxide` **未引入**——Telegram 渠道在 `src/channel.rs` 用 reqwest 自研实现，不依赖 teloxide 类型（`src/domain.rs` 注释禁止 teloxide 类型跨越领域边界）；不得在文档/注释中声称已使用未引入的依赖。配置为**环境变量手工解析**（`ARCH-CONFIG-ENV`，无 figment/TOML）。`ACCOUNT_ID` **可选**：留空 → 取 JMAP session 的**默认/主账户**；显式值经校验后使用（`REQ-SINGLE-ACCOUNT`）。

> 部署/镜像相关工作（Dockerfile、compose、健康检查、CI 发布）不是各功能阶段的目标，集中在 deployment.md；功能阶段验收通过 `cargo test`，不需要先做镜像。
> **JMAP session URL 语义**（`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）：`JMAP_SESSION_URL` 可填**服务基地址**（`https://host[:port]`）或**完整** `…/.well-known/jmap`；代码在 `Client::connect` 前归一化为 **origin/base**（jmap-client 自行追加 `/.well-known/jmap`），**不得出现重复路径**（`…/.well-known/jmap/.well-known/jmap`）。仅 **HTTPS**；**禁止 URL 内嵌用户名/密码**（凭据只经 `JMAP_USERNAME`/`JMAP_PASSWORD`）；拒绝危险 query。

---

## 4. 禁止事项

- 禁止把 AI 输出当作可执行操作（LLM 无工具权）。
- 禁止未确认就把正文 / 附件内容送入 LLM；禁止把分析结果持久化（Redis/磁盘/缓存）。
- 禁止在生产路径绕过 chat 白名单、关闭 TLS 校验、关闭熔断/回退。
- 禁止把 secrets（token / 密码 / LLM API key）写入代码、配置、日志或镜像。
- 禁止把部署/平台细节塞回 `design.md`（见 §6）；反之亦然。
- 禁止引入 Alpine/musl 基础镜像或非 Docker 部署形态（deployment.md 硬约束 C-DOCKER/C-DEBIAN-SLIM）。
- 禁止引入 SSE/EventSource/WebSocket/长轮询等长连接运行模式（`C-NO-LONG-CONN`/`NG-POLLING-SSE`/`NG-LONG-POLLING`）。
- 禁止 SQLite/本地卷作持久层；状态与游标仅外部 Redis（`NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`/`C-REDIS-ONLY-STATE`），Redis 丢失走 JMAP 对账 `FLOW-RECONCILE` 恢复。
- 禁止无授权改动 `docs/design.md`、`docs/deployment.md`、`AGENTS.md` 之外的内容；安全边界变更须同步三份文档。
- 禁止在领域层（`jmap` / `llm` / 意图状态机 / `notify::core`）引入 Telegram/teloxide 类型；`teloxide` 仅允许出现在 `channel/telegram/`。
- 禁止提前实现钉钉/飞书 adapter（仅保留抽象与扩展位，不写具体渠道代码、不建渠道专属配置项）。
- 禁止在文档/注释中声称已使用**尚未加入 `Cargo.toml`** 的依赖（`teloxide`/`figment` 等）；`jmap-client`/`redis`/`reqwest` 已引入（版本以 `Cargo.toml` 为准，`ARCH-DEPS-STAGE4`），其中 jmap-client **禁止启用其 WebSocket feature**（`C-NO-LONG-CONN`）；配置恒为环境变量手工解析（`ARCH-CONFIG-ENV`），禁止引入 figment/TOML 配置文件。
- 禁止在未通过 `GATE-P0` 时进入阶段1；禁止把阶段0 容器暴露公网（业务体仍为占位，`BOUND-STAGE1`）。入口鉴权（`SAF-AUTH-*`，R1）**已在阶段0 落地**，禁止移除、放宽或绕过（fail-closed）。
- 禁止在**透传 LB 模型**（`SAF-LB-PASSTHRU`）下削弱/关闭任何后端的 `SAF-AUTH-*` 校验（小平台无防火墙，后端可能被公网直连）；禁止让多实例使用**不同**的 `SAF-AUTH-*` secret（`C-LB-SHARED-SECRETS`）；禁止让 `/reconcile` 多实例并发（必须 Redis 锁，`SAF-RECONCILE-LOCK`）。
- 禁止引入**任何数据库**（`C-NO-DB`）：不引入 SQLite/Postgres/MySQL/嵌入式数据库；不连接 Redis 之外的第二个状态存储。
- 禁止**本地文件/目录写入**（`C-NO-LOCAL-WRITE`）：不写日志文件、不写数据文件、不写临时缓存、不挂载本地卷。
- 禁止将日志写入非 stdout/stderr 的目的地（`C-LOG-STDOUT-ONLY`）：不用 `rolling-file`/`FileAppender`/自定义文件 sink；日志只能输出到容器标准输出/错误，由平台采集。
- 禁止把**密钥、邮件正文、AI 请求/响应内容、附件内容**写入日志或 Redis（`SAF-LOG-PURITY`）：日志/Redis 仅允许结构化事件、计数、时间戳、脱敏摘要。
- 禁止依赖**进程内状态**做生产恢复（`C-NO-STATEFUL-RECOVERY`）：任何重启续跑（去重、sinceState、Streams 断点、熔断计数、会话）必须能从 Redis + JMAP 对账重建；进程内缓存仅为性能优化，丢失必须安全可重入。

---

## 5. 测试验收（至少满足）

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全绿；**并在 Debian `rust:1-slim-bookworm` 容器内执行**（`GATE-P0`/`C-DEBIAN-SLIM`；阶段0 P0 门禁要求）。
- 占位符使用**局部** `#[expect(dead_code, reason="稳定ID+阶段0占位")]`；**禁止 crate 级 `#[allow]`**；不得为过 clippy 删除安全访问器（如 `JMAP_PASSWORD` getter）或让 `Debug` 泄密。
- 不变量测试（[design.md §9.3](docs/design.md#93-关键不变量测试)）：
  - **VIEW / 查看原文**：mock AI 端点零请求（断言 LLM client 未被调用）。
  - **新邮件通知**：消息内无正文内容（断言泄漏）。
  - **长邮件**：正文 > 4000 字符不发送全文，仅预览 + 选项。
  - **AI 前置确认**：未确认前 LLM 零调用；确认后才发起请求。
  - **结果不落盘**：分析处理后无新增磁盘/Redis 写入路径。
  - **AI 3 次失败**：触发熔断 → 弹确认 → 回退带"AI 不可用"徽标；`LLM_ENABLED=false` 全走回退。
  - 正文转义、sinceState 存 Redis + 模拟 Redis 删除后由对账 `FLOW-RECONCILE` 恢复、通知去重。
  - **入口鉴权（`SAF-AUTH-*`）**：`/reconcile`/`/webhook/tg`/`/push/jmap` 在缺失或错误凭证下返回 `401` 且**无副作用**（无 Redis 写入、无 JMAP/AI 调用）；正确凭证放行。
  - **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz` 返回 `200`；`/ready` 已做基础探测（`ARCH-READY-BASELINE`：配置完整性 + Redis 可达性，不就绪返 `503`）——测试**可以**断言 `/ready` 在基础依赖不就绪时返回 `503`，但**不得**断言其已执行 JMAP session / TG getMe 等端到端探测（余下范围 `GATE-READY-DEPS`）；两者响应体均不含敏感信息。
  - **JMAP 只读 adapter（`MOD-JMAP-CLIENT`/`GATE-G1-JMAP-READONLY`）**：mock 测试覆盖 session/account 选择（`ACCOUNT_ID` 空→主账户、显式值校验）、**URL 归一化**（`JMAP_SESSION_URL` 基地址与完整 `…/.well-known/jmap` 两种输入结果一致、无重复路径、`http://` 拒绝、内嵌凭据拒绝，`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）、`list_folders`/`list_emails`（`limit` 边界）/`read_email`（多 part 拼接与"无可用部分"明确错误，`REQ-JMAP-RAW-MULTIPART`）、`received_at` 解析；真机测试用 `#[ignore]` 标记、经环境变量驱动（运行：`cargo test -- --ignored jmap::`；仅编译：`cargo test --no-run`），**缺环境时清晰跳过且不泄密**，CI 默认不跑真机用例。**G1/D-G1-1 代码已实现，待真实 `cargo test -- --ignored jmap::` 验证；未实际运行 `--ignored` 前不得声称"真机通过"。**
- 领域/渠道解耦测试：`jmap`/`llm`/意图状态机/`notify::core` 不 import teloxide 类型（编译期/脚本检查）；`MockChannel` 可驱动全部领域与通知流程（不依赖 Telegram）。
- 文件行数抽查：>500 行的 `.rs` 文件在 PR 说明中可见（软性，不自动 fail）。
- 代码注释一致性：抽查关键领域、渠道、通知、Redis/AI 模块，确认注释引用的稳定语义 ID、状态流转和安全边界与 `design.md`/`deployment.md` 一致。
- 涉及部署的改动需过 deployment.md 的 CI/验收清单（`C-DOCKER` 镜像扫描、非 root、health 端点等）。
- 涉及部署/日志/状态的改动需过 deployment.md §8.2 红线自检 6 项（`C-NO-DB` / `C-NO-LOCAL-WRITE` / `C-LOG-STDOUT-ONLY` / `SAF-LOG-PURITY` / `C-NO-STATEFUL-RECOVERY` / 外部 Redis）；任一失败禁止发布。

---

## 6. 文档边界与引用关系

| 文件 | 内容 | 何时更新 |
|---|---|---|
| `docs/design.md` | 产品行为、架构、模块接口、状态机、数据流、错误处理、测试、实施阶段、渠道抽象（Channel/Notifier/MessageAdapter）、产品/架构类待确认问题 | 行为/接口变更时 |
| `docs/deployment.md` | 通用 HTTPS-only Docker、Debian、构建/运行时、Secrets、Redis（状态唯一载体）、Webhook/Push/对账短请求路由、**多实例 LB/HA（Worker 前置，§10）**、健康检查、CI、2 项待确认（`Q-DEP-A` 监控日志落盘 / `Q-DEP-B` 日志采集） | 部署/发布变更时 |
| `AGENTS.md`（本文件） | 目标、硬性安全边界、实现顺序、禁止事项、测试验收、文档引用关系、**跨文档引用索引（§7）** | 安全边界/流程变更时 |
| `README.md` / `README.zh-CN.md` | 面向最终用户的项目简介、安全模型、配置/环境变量、首次联调流程、已知限制（英文版 / 中文版，内容须对等，顶部互链） | 用户可见行为或流程变更时 |
| `docs/todo.md` | 未完成项与阻塞原因（替代已删除的根目录 `HANDOFF.md`） | 待办项增减时 |

- 改动产品规则必须同步三份文档的相关表述，保持一致、不重复堆砌。
- `design.md` 不写部署细节，`deployment.md` 不写产品行为/接口；交叉处用**稳定 ID**（§7 索引）互相引用，不用章节号。

---

## 7. 跨文档引用索引表

> 约定：跨文档引用一律用下表的**稳定 ID**（不用 `§x.y` 章节号）。ID 语义稳定、全仓唯一、可 grep；内容搬家时**只更新本表的"文件/锚点"列**，所有引用本身零改动。
> 新增约束/组件时，先在本表登记一行，再在定义点写 `**[ID]**` 标签。

| ID | 定义文件 | 一句话 | 类别 |
|---|---|---|---|
| `C-DOCKER` | docs/deployment.md §0 | 必须 Docker 部署 | 部署约束 |
| `C-DEBIAN-SLIM` | docs/deployment.md §0 | Debian slim，禁 Alpine | 部署约束 |
| `C-NO-SECRET-IN-IMAGE` | docs/deployment.md §0 | secrets 不进镜像 | 部署约束 |
| `C-RUSTLS` | docs/deployment.md §0 | rustls + native-roots | 部署约束 |
| `C-HTTPS-INBOUND` | docs/deployment.md §0 | HTTPS-only 入站，容器内明文 HTTP | 部署约束 |
| `C-HTTPS-URL` | docs/deployment.md §0 | 公网 HTTPS URL 由平台提供（bot 不持证书） | 部署约束 |
| `C-AUTH-APP-BASIC` | docs/deployment.md §9 / design.md §3.1 | Stalwart 认证 = App Password + Basic | 部署约束 |
| `C-NO-TCP-EXPOSE` | docs/deployment.md §0 | 单监听 `PORT`，不暴露附加 TCP 端口 | 部署约束 |
| `C-NO-LONG-CONN` | docs/deployment.md §0 | 无 SSE/WS/长轮询等长连接 | 部署约束 |
| `C-REDIS-ONLY-STATE` | docs/deployment.md §0 | 状态仅外部 Redis，不用 SQLite/本地卷 | 部署约束 |
| `C-REDIS-MANAGED-AOF` | docs/deployment.md §0 | Redis 用户托管 + 开启 AOF 持久化 | 部署约束 |
| `C-PORT` | docs/deployment.md §0 | 通用 PORT 约定 | 部署约束 |
| `NG-SERVER-MODE` | docs/deployment.md §1 | `RUN_MODE=server` 常驻，非目标 | 非目标 |
| `NG-POLLING-SSE` | docs/deployment.md §1 | EventSource/SSE 长连接，非目标 | 非目标 |
| `NG-LONG-POLLING` | docs/deployment.md §1 | Telegram 长轮询，非目标 | 非目标 |
| `NG-SQLITE-PERSIST` | docs/deployment.md §1 | SQLite 持久化，非目标 | 非目标 |
| `NG-LOCAL-VOLUME` | docs/deployment.md §1 | 本地卷持久化，非目标 | 非目标 |
| `NG-SERVERLESS-BIND` | docs/deployment.md §1 | 绑定具体 serverless 平台，非目标 | 非目标 |
| `MOD-DEDUP` | docs/deployment.md §6 | Redis `SET NX` 幂等键 | 组件 |
| `MOD-STREAMS` | docs/deployment.md §6 | Redis Streams 队列 + worker | 组件 |
| `MOD-SINCESTATE` | docs/deployment.md §6 | sinceState 游标（存 Redis） | 组件 |
| `FLOW-NEW-MAIL` | docs/design.md §5.4 / deployment.md §6 | Push 新邮件流 | 数据流 |
| `FLOW-RECONCILE` | docs/deployment.md §6 | 外部 Cron 对账补差 + Redis 丢失恢复 | 数据流 |
| `REQ-AI-CONFIRM` | docs/design.md §12 | AI 仅在明确要求+确认后接触正文 | 需求 |
| `REQ-VIEW-DIRECT` | docs/design.md §12 | 查看原文始终 JMAP 直取 | 需求 |
| `REQ-LONG-EMAIL` | docs/design.md §12 | 长邮件禁止发全文，AI 摘要 ~300 字 | 需求 |
| `REQ-ANALYSIS-EPHEMERAL` | docs/design.md §12 | 分析/摘要结果不持久化 | 需求 |
| `REQ-ATTACH-ONDEMAND` | docs/design.md §12 | 附件按需拉取 | 需求 |
| `REQ-AI-FUSE` | docs/design.md §12 | AI 失败约 3 次 → 熔断 + 用户确认回退 | 需求 |
| `REQ-LLM-OPENAI-COMPAT` | docs/design.md §12 | OpenAI-compatible 环境变量 | 需求 |
| `REQ-AI-EXTERNAL-CONSENT` | docs/design.md §12 / AGENTS.md §2 | 仅用户明确允许后才向外部 AI 发正文 | 需求 |
| `REQ-AI-CONSENT` | docs/design.md §12.3.1 | AI 授权期限与 Redis 短期 TTL（授权有效期、到期后重新询问） | 需求 |
| `REQ-SINGLE-ACCOUNT` | docs/design.md §3.1/§11.3 | 单账户；多账户 = 多 bot 实例 | 需求 |
| `REQ-RECONCILE-IDEMPOTENCY` | src/state.rs `ReconcileState` / docs/design.md §8.2 | JMAP 对账游标只有在全部分页事件成功入队（XADD）后才推进；单次对账由 Redis SET NX EX 锁 `lock:reconcile` 保证单飞（TTL 300s，owner token 续期 90s，仅持有者可续期/释放）；处理端再经 `claim_dedup`（SET NX EX，86400s）保证同一流消息不重复投递 | 需求 |
| `NFR-NOTIFY-SLA` | docs/deployment.md §6.5 | 通知可用性 ≥99.9%，允许少量延迟 | 非功能 |
| `NFR-RECONCILE-INTERVAL` | docs/deployment.md §6.3 | 外部 Cron 对账间隔 5–10 分钟 | 非功能 |
| `SAF-NOTIFY-META` | AGENTS.md §2 | 新邮件通知只含元数据，正文不入通知 | 安全 |
| `SAF-CHAT-ALLOWLIST` | AGENTS.md §2 / design.md §7.3 | CHAT_ALLOWLIST 硬约束，处理前先拒绝非白名单 | 安全 |
| `SAF-AUTH-RECONCILE` | AGENTS.md §2 / design.md §7.3 | `/reconcile` 需 `Authorization: Bearer RECONCILE_TOKEN`，fail-closed | 安全 |
| `SAF-AUTH-TG-WEBHOOK` | AGENTS.md §2 / design.md §7.3 | `/webhook/tg` 需头 `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET` | 安全 |
| `SAF-AUTH-JMAP-PUSH` | AGENTS.md §2 / design.md §7.3 | `/push/jmap` 按 `pushSubscriptionId` 查 Redis 短期验证状态（存 `session_digest` 摘要），缺失或摘要不符拒绝；状态缺失回落 worker 重新验证（fail-closed） | 安全 |
| `SAF-ADMIN-SESSION` | src/config.rs `admin_session_digest` / §7 | 返回用于 Redis admin-session 记录的 SHA-256 摘要；bearer token 本体绝不写入 Redis | 安全 |
| `SAF-NO-SECRET-ECHO` | src/config.rs `SecretString` 字段与公共配置 API | 密钥字段不实现 `Debug` 且永不进入公共配置 API 响应；Redis 线格式类型私有，不得序列化进 HTTP 响应 | 安全 |
| `SAF-PROBE-PUBLIC` | AGENTS.md §2 / design.md §7.3 | `/healthz`、`/ready` 公开探针：无鉴权、无敏感信息 | 安全 |
| `ARCH-HEALTHZ` | docs/design.md §7.3/§10.0 | `/healthz` liveness（进程存活），语义长期稳定 | 架构 |
| `ARCH-READY-BASELINE` | docs/design.md §7.3 / src/notify.rs `ready` | `/ready` 基础探测：配置完整性 + Redis 可达性，就绪 `200`（就绪报告 JSON），不就绪 `503`（标准错误 envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`）；**不**含 JMAP session / TG getMe 端到端探测（阶段0 交付时曾为占位 200，现已演进） | 架构 |
| `GATE-READY-DEPS` | docs/design.md §10.0 | 余下门禁：`/ready` 补 JMAP session / TG getMe 等**端到端**依赖探测（阶段3.5+ 的剩余部分，含其测试与部署接线） | 流程 |
| `GATE-P0` | docs/design.md §10.0 | 阶段0 P0 门禁（fmt/clippy/test 过 Debian 容器等 8 项） | 流程 |
| `BOUND-STAGE1` | docs/design.md §10.0 | 阶段1 推进边界（过 GATE-P0 才进；R1 入口鉴权已在阶段0 落地） | 流程 |
| `ARCH-CONFIG-ENV` | docs/design.md §7.1 | 配置=环境变量手工解析，无 figment/TOML | 架构 |
| `ARCH-AXUM-08` | docs/design.md §10.0 | 单端口 axum 0.8 入口（版本以 Cargo.toml 为准） | 架构 |
| `ARCH-STAGE0` | docs/design.md §10.0 / §5.3 | 阶段0 实际交付与现状 | 架构 |
| `ARCH-DEPS-STAGE0` | docs/design.md §10.0 / AGENTS.md §3 | 阶段0 实际依赖集（axum/serde/secrecy/subtle/tokio/tracing…） | 架构 |
| `ARCH-DEPS-STAGE1` | docs/design.md §10.0-1 / AGENTS.md §3 | 阶段1 依赖现况：`jmap-client 0.4.2` 已引入，版本/features 以 Cargo.toml 为准，禁用 WebSocket feature | 架构 |
| `ARCH-DEPS-STAGE4` | docs/design.md §10.0-1 / AGENTS.md §3 | 阶段3.5/4 后依赖现况：`redis 0.27`、`reqwest 0.13` 已引入并实际使用；`teloxide` **未**引入（Telegram 由 `src/channel.rs` 用 reqwest 自研实现）；版本一律以 `Cargo.toml` 为准 | 架构 |
| `MOD-JMAP-CLIENT` | docs/design.md §6-7 / AGENTS.md §3 | `domain::jmap::client` 真实只读 adapter（session/account/mailbox/email 只读） | 组件 |
| `MOD-TELEGRAM-NOTIFY` | src/worker.rs / docs/design.md §12 | 有界元数据通知 worker：消费出站队列，把脱敏通知经 Telegram 投递（不承载正文/AI 响应） | 组件 |
| `REQ-JMAP-SESSION-URL` | docs/design.md §7.1 / .env.example | `JMAP_SESSION_URL` 接受服务基地址或完整 /.well-known/jmap，归一化为 origin/base 后再交 jmap-client（无重复路径）；**代码已实现（D-G1-1），待真机验证** | 需求 |
| `SAF-JMAP-URL` | docs/design.md §7.1 / AGENTS.md §3 | JMAP URL 约束：仅 HTTPS、禁止内嵌凭据、拒绝危险 query | 安全 |
| `REQ-JMAP-RAW-MULTIPART` | docs/design.md §3.2/§10.1 | `read_email` 多 part 原文：按 text_body 顺序拼接"有 part_id 且 bodyValue"的部分；无可用部分→明确错误 | 需求 |
| `GATE-G1-JMAP-READONLY` | docs/design.md §10.1 / AGENTS.md §5 | G1 门禁：只读 adapter **代码已实现**（mock + `#[ignore]` 真机测试），**待真实 `cargo test -- --ignored jmap::` 验证** | 流程 |
| `ARCH-LB-WORKER` | docs/deployment.md §10 | 多实例 LB/HA：免费 Cloudflare Worker 作唯一对外入口 + 故障转移，后端为多平台同镜像 | 架构 |
| `C-LB-SINGLE-REG-URL` | docs/deployment.md §10.1 | Telegram/Push/Cron 只登记 Worker 的稳定 URL；后端平台入口不对外登记 | 约束 |
| `C-LB-SHARED-SECRETS` | docs/deployment.md §10.3 | 多实例必须共享同一组 `SAF-AUTH-*` secret，否则随机 401 | 约束 |
| `SAF-LB-PASSTHRU` | docs/deployment.md §10.3 / AGENTS.md §2 | 信任模型=透传：Worker 不改写鉴权；后端必须继续 fail-closed 校验（后端可能被公网直连） | 安全 |
| `SAF-RECONCILE-LOCK` | docs/deployment.md §10.5 | `/reconcile` 不扇出，Redis 锁保证单实例执行，避免重复对账 | 安全 |
| `MOD-STREAMS-GROUP` | docs/deployment.md §10.5 | 多实例用同一 Streams 消费组名，Redis 自动分摊（at-least-once 不重复处理） | 组件 |
| `MOD-HEALTH-AGG` | docs/deployment.md §10.6 | Worker 聚合健康视图，报告各后端存活供外部监控 | 组件 |
| `NFR-HA-MULTI-INSTANCE` | docs/deployment.md §10.7 / §9.1 | 多实例高可用语义；双活或主备均可；Redis 单点故障不在方案范围（用户外部解决） | 非功能 |
| `C-NO-DB` | docs/deployment.md §0 / §9.1 / AGENTS.md §2 | 生产不使用任何数据库（无 SQLite/Postgres/MySQL/嵌入式），Redis 为唯一状态存储；应用不连接第二个数据库 | 约束 |
| `C-NO-LOCAL-WRITE` | docs/deployment.md §0 / §9.1 / AGENTS.md §2 | 禁止本地文件/目录写入（日志/数据/临时缓存/本地卷） | 约束 |
| `C-LOG-STDOUT-ONLY` | docs/deployment.md §0 / §9.1 / AGENTS.md §2 | 日志只写 stdout/stderr，由平台采集；禁用文件日志后端 | 约束 |
| `SAF-LOG-PURITY` | docs/deployment.md §0 / §9.1 / AGENTS.md §2 | 日志与 Redis 写入内容仅限结构化事件/计数/时间戳/脱敏摘要；禁止密钥/邮件正文/AI 请求响应/附件内容 | 安全 |
| `C-NO-STATEFUL-RECOVERY` | docs/deployment.md §0 / §9.1 / AGENTS.md §2 | 禁止依赖进程内状态做生产恢复；恢复一律走 Redis + JMAP 对账；进程内缓存仅为性能优化，丢失须安全可重入 | 约束 |
