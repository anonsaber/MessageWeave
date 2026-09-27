# Stalwart JMAP ↔ Telegram Bot — Rust 方案设计

> 状态：**设计与实现均已落地**（代码见仓库 `src/`、`web/`、`cloudflare-worker/`；本文同时保留设计决策与选型评估的原始记录，阶段 1/2/4 的表述属历史计划，当前进度见 §10.5）
> 作者：Cowork（team: MessageWeave）
> 日期：2026-09-21
> 目标读者：Codex CLI（lead）、后续 AI coding agent、最终用户评审
>
> **相关文档**（职责分离，避免重复堆砌）：
> - [AGENTS.md](../AGENTS.md) — 给后续 AI coding agent 的硬性安全边界、实现顺序、禁止事项、测试验收与文档引用关系
> - [docs/deployment.md](deployment.md) — 部署运维与发布：通用 HTTPS-only Docker 容器、Secrets、Redis（状态唯一载体）、短请求 Webhook/Push/对账路由、健康检查、CI 发布与仍需确认项
>
> 本文档只保留与**产品行为、代码架构、模块接口、状态机、数据流、错误处理、测试和实施阶段**直接相关的内容。部署运维、Docker/通用容器平台细节与给 coding agent 的通用操作指令已分别移至 `deployment.md` 与 `AGENTS.md`。

---

## 0. 文档导航

1. [任务与范围](#1-任务与范围)
2. [jmap-client 能力分析](#2-jmap-client-能力分析)
3. [认证与邮箱操作适配](#3-认证与邮箱操作适配)
4. [Telegram Rust 框架选型](#4-telegram-rust-框架选型)
5. [整体架构与数据流](#5-整体架构与数据流)
6. [模块划分](#6-模块划分)
7. [配置与安全](#7-配置与安全)
8. [错误处理与可观测性](#8-错误处理与可观测性)
9. [测试策略](#9-测试策略)
10. [分阶段实施计划](#10-分阶段实施计划)
11. [待确认问题（产品/架构类）](#11-待确认问题产品架构类)
12. [AI 辅助能力：架构、确认门槛、失败回退](#12-ai-辅助能力架构确认门槛失败回退)

> 部署/平台类决策已确认（单账户、App Password+Basic、Redis 托管+AOF、平台 HTTPS URL、外部 Cron）并收敛；剩余待办仅 `Q-DEP-A`/`Q-DEP-B`，见 deployment.md §9。

---

## 1. 任务与范围

### 1.1 目标
构建一个 Rust 写的 Telegram 机器人，作为 Stalwart JMAP 邮箱的**个人邮件助手**：

- 通过 Telegram 命令查询/阅读邮件、查看文件夹、发送邮件、管理关键词等。
- 利用 JMAP 的 **Push HTTPS 回调**（+ 外部 Cron 对账兜底）在新邮件到达时**主动推送到 Telegram**。（EventSource/SSE/长轮询为非目标，见 deployment.md `NG-POLLING-SSE`/`NG-LONG-POLLING`/`C-NO-LONG-CONN`。）
- 单用户或多账户部署（默认面向单账户自托管场景）。

### 1.2 范围（本阶段）
- ✅ 调研 + 方案设计（本文档）
- ❌ 不创建/修改项目代码、不初始化 cargo 工程
- ✅ 输出可供 lead 与用户评审的详细方案，标注关键不确定项

### 1.3 调研依据
- [`stalwartlabs/jmap-client`](https://github.com/stalwartlabs/jmap-client)（main 分支，截至本次调研）
- 项目目录：`/home/okabe/Repo/messageweave/`（工具链要求见 §10.0 与 AGENTS.md）

---

## 2. jmap-client 能力分析

来源：直接阅读 `stalwartlabs/jmap-client` 仓库 `src/lib.rs`、`src/client.rs`、`src/email/`、`src/email_submission/`、`src/event_source/`、`Cargo.toml`、`README.md`、`examples/`。

### 2.1 crate 概况
| 项 | 值 |
|---|---|
| crate 名 | `jmap-client`（crates.io） |
| 协议覆盖 | JMAP Core (RFC 8620)、Mail (RFC 8621)、WebSocket (RFC 8887)、Sieve (draft-12) |
| 异步运行时 | tokio + reqwest |
| 许可证 | Apache-2.0 OR MIT |
| `forbid(unsafe_code)` | 是（lib.rs 顶部声明）✅ |
| 默认 features | **0.4.2 实测 `default = ["async", "websockets", "aws_lc_rs"]`**（含 WebSocket 栈 `tokio-tungstenite`）。⚠️ `default-features = true` 会**隐式启用 WebSocket**，违反 `C-NO-LONG-CONN`。**本项目实际选用 `default-features = false, features = ["async", "rustls"]`（无 `websockets`）**，以 `Cargo.toml` 为准。 |

### 2.2 模块结构（`src/`）
```
lib.rs            顶层：URI / Method / DataType / PushObject / Error 枚举
client.rs         Client：认证、connect、session、send_request、event_source、ws
core/             request / response / get / set / query / query_changes / changes / error / session
email/            mod + helpers（email_get/email_query/email_import/email_copy/email_set/email_parse…）
email_submission/ helpers（email_submission_get/query/set → 发送邮件 + 状态查询）
mailbox/          helpers（mailbox_create/query/get/set/destroy…）
thread/           thread/get
identity/         Identity（发件身份）get/set
blob/             blob 上传/复制（附件）
sieve/            SieveScript（服务端过滤脚本）
vacation_response/
principal/        Principal（账户/共享）
push_subscription/ PushSubscription（HTTP push 回调注册）
event_source/     SSE 流：mod / parser / stream
client_ws/        WebSocket 客户端（feature = "websockets"）
```

### 2.3 关键能力映射到本 Bot
| Bot 需求 | jmap-client API | 备注 |
|---|---|---|
| 登录/会话 | `Client::new().credentials(...).connect(url)` | 支持 Basic 与 Bearer；connect 解析 session URL、capabilities |
| 列文件夹 | `mailbox_query` / `mailbox_get` | 带 `role` 可识别 INBOX/重要/草稿等 |
| 列邮件 | `email_query`（Filter + Comparator + anchor 分页） | Filter: `subject`/`from`/`to`/`in_mailbox`/`has_keyword`/`after`/`before`… |
| 读邮件正文 | `email_get` + `Property` 选择 | `BodyStructure`/`BodyValues`/`Preview`/`TextBody`/`HtmlBody` |
| 读附件 | `Blob/get`（blobId）或 `email_parse` | 大附件需分片/流式下载 |
| 发邮件（2 步） | ① `email_set`/`email_import` 建 draft ② `email_submission_set` 发送 | submission 关联 identityId |
| 删除/归档 | `email_set`（keywords `$seen`/`$flagged`）、`mailbox_destroy` | JMAP 无真"删除"，靠 keyword/搬家 |
| 搜索 | `email_query` Filter + `SearchSnippet/get` 高亮 | |
| 实时通知（Push + 对账兜底） | Push HTTPS 回调 → `StateChange`；外部 Cron 调用 `/reconcile` 使用 `Email/changes` 补差 | 需公网 HTTPS 入口（deployment.md `C-HTTPS-INBOUND`/`FLOW-NEW-MAIL`）；Push 不是唯一可靠来源 |
| SSE / WebSocket（非目标） | `event_source` / `client_ws` | 本部署**不使用**（deployment.md `NG-POLLING-SSE`/`NG-LONG-POLLING`/`C-NO-LONG-CONN`）；仅列 crate 能力供调研 |

### 2.4 认证机制（`client.rs`）
- `Credentials::Basic { username, secret }` — 用户名/密码，Stalwart 原生支持。
- `Credentials::Bearer { token, .. }` — OAuth2 access_token；可选 `refresh_token` + `refresh_url` + `refresh_grace`，client 会在过期前自动刷新。
- `connect()` 作用：GET session URL → 解析 `accounts`/`capabilities`/`download_url`/`upload_url`/`event_source_url`，缓存 account_id。
- 支持自定义 `reqwest::Client`（`Client::new().client(reqwest_client)`）：可注入代理、TLS 配置、超时、UA。
- 支持 `accept_long_responses` / `event_source(ping, ..)` 用于长连接保活。

### 2.5 错误模型（`Error` 枚举）
```
Transport(reqwest::Error)   网络/TLS/超时
Parse(serde_json::Error)    序列化
Internal(String)            客户端内部
Problem(Box<ProblemDetails>) JMAP problem-details
Server(String)
Method(MethodError)         JMAP method-level 错误（NotJSON/Forbidden/RateLimit/StateMismatch…）
Set(SetError<String>)       /set 级别错误（每条记录的 creation/update/destroy 失败）
WebSocket(...)              ws 错误（feature 开启时）
```
- `MethodErrorType` 细粒度：`ServerUnavailable`/`ServerFail`/`RateLimit`/`InvalidArguments`/`Forbidden`/`StateMismatch`/`TooManyChanges`… → 可直接驱动 Bot 的重试/限流/状态重置策略。

### 2.6 实时通道决策（已定，见 deployment.md）

> 部署目标 = **通用 HTTPS-only Docker、无长连接**（deployment.md `C-NO-LONG-CONN`/`C-HTTPS-INBOUND`）。
> 当前通道 = JMAP Push HTTPS 回调（先由管理员通过 `POST /api/push/register` 显式注册，见 §7.3）+ 外部 Cron `/reconcile` 对账兜底。Push 订阅不会自动创建，不能把 Push 当作唯一可靠来源。
> EventSource/SSE 与 WebSocket 均标记为**非目标**（`NG-POLLING-SSE`），仅保留下表作 crate 能力调研参考。

| 通道 | 是否采用 | 备注 |
|---|---|---|
| **Push Subscription（HTTP 回调）** | 已接入，需显式注册 | 短请求模型，契合无状态 + 无长连接；`POST /api/push/register` 创建订阅，`/push/jmap` 自动完成 Stalwart 验证回写 |
| EventSource/SSE | ❌ 非目标 | 长连接，与 `C-NO-LONG-CONN` 冲突（`NG-POLLING-SSE`） |
| WebSocket（RFC 8887） | ❌ 非目标 | 长连接，与 `C-NO-LONG-CONN` 冲突；且需服务端支持 |

对账兜底：外部 HTTPS Cron 周期调 `/reconcile`（deployment.md `FLOW-RECONCILE`），用 `Email/changes` + Redis `sinceState` 补差，兼做 Redis 丢失后的游标重建。

---

## 3. 认证与邮箱操作适配

### 3.1 认证策略（已确认：App Password + Basic）
- **确认采用**（`C-AUTH-APP-BASIC`）：`Credentials::Basic`（Stalwart 账户邮箱 + **应用专用密码 App Password**）。App Password 可独立吊销/设到期，不用主密码。配置注入，不入代码。
- **不采用**：OAuth/OIDC `Bearer`（无 OIDC 需求时增加复杂度，不选）；主密码 Basic。
- **安全**：密码仅存配置或密钥管理器；运行期用 `secrecy::SecretString` 包裹，日志永不打印明文（见 §7.2）。
- 运行期注入方式见 deployment.md（`C-NO-SECRET-IN-IMAGE`）。
- **单账户**（`REQ-SINGLE-ACCOUNT`）：本实例只接一个 Stalwart 账户；多账户 = 部署多个 bot 实例。

### 3.2 邮箱操作适配层（JMAP ↔ Bot 语义）
建议在 `domain::jmap` 模块封装一层领域语义，对上层只暴露业务动词：

| Bot 动词 | 封装函数 | 内部 JMAP |
|---|---|---|
| `list_folders()` | → `Vec<Folder>` | `mailbox_query` + `mailbox_get`（缓存 role→id） |
| `list_emails(folder, page)` | → `Vec<EmailSummary>` | `email_query`(anchor 分页) + `email_get`([Subject, From, Preview, ReceivedAt, HasAttachment]) |
| `read_email(id, want_body)` | → `EmailBody { text, html, attachments }` | `email_get`([BodyStructure, BodyValues, BlobIds])；**多 part 原文**（`REQ-JMAP-RAW-MULTIPART`）：按 `text_body` 顺序筛选"有 `part_id` 且 `bodyValue`"的 part 后**拼接**为 `text`；若无可用部分 → 返回**明确错误**（不静默返回空串）。附件用 `Blob/get` |
| `send_email(to, subject, body, attachments)` | → `EmailId` | ① `email_import`/`email_set` 建 draft ② `email_submission_set`(onSend) |
| `set_flag(id, keyword)` | → () | `email_set` keywords |
| `search(query)` | → `Vec<EmailSummary>` | `email_query`(Filter::and[…]) + `SearchSnippet/get` |

> 设计要点：`email_query` 的 `anchor`+`position` 分页是 JMAP 标准做法，比传统 offset 更稳；`sinceState` + `changes` 用于增量同步，避免重复拉全量。

---

## 4. Telegram Rust 框架选型

### 4.1 候选评估
| 框架 | crate | 维护状态 | 特性 | 适配度 | 结论 |
|---|---|---|---|---|---|
| **teloxide** | `teloxide` | 活跃，最新 0.17，下载量大 | dptree 分发、对话 FSM、`webhooks`+`webhooks-axum`、Redis 会话存储、`throttle`、`macros`、`tracing`、`rustls` | ★★★★★ | **首选** |
| grammers | `grammers` / `grammerslib` | 维护一般 | MTProto（非 Bot API），无需 Telegram Bot Token | ★★ | 仅在不能用 Bot API 时 |
| telegram-bot (旧) | `telegram-bot` | 基本停更 | reqwest + futures | ★ | 不推荐 |

### 4.2 选定 teloxide 的理由
1. 与 jmap-client 同为 tokio + reqwest 生态，运行时与 TLS 栈（rustls）可复用。
2. 内建 Dispatcher + UpdateKind 枚举匹配命令，与 Bot 的命令路由天然契合。
3. 支持 `webhooks-axum`（生产 Webhook 形态；**不使用长轮询** `NG-LONG-POLLING`）与 Redis 会话存储（记住用户上下文：当前选中的文件夹/分页游标；不用 SQLite）。
4. `throttle` feature 天然契合 Telegram 的 30 msg/s 速率限制。
5. `tracing` feature 与本项目观测性统一。
6. `macros` feature 可用 `#[teloxide::command]` 自动解析命令参数，减少样板。

### 4.3 teloxide 关键 feature 选择（**计划，阶段2 引入**）

> ⚠️ 阶段0 **未引入 teloxide**（`ARCH-DEPS-STAGE0`）；下方为阶段2 引入时的目标 feature 集，不得据此认为 `Cargo.toml` 已含 teloxide。

```
# 阶段2 计划（尚未加入 Cargo.toml）
teloxide = { version = "0.17", features = [
  "macros", "rustls", "rustls-native-roots",     # TLS 与 jmap-client 一致
  "redis-session",                                # 无状态会话：走外部 Redis（deployment.md C-REDIS-ONLY-STATE）
  "throttle",                                      # 遵守 TG 速率限制
  "tracing",
] }
# 部署形态 = Webhook（非长轮询，deployment.md NG-LONG-POLLING）；按需启用 "webhooks-axum"
```

### 4.4 备选/降级方案
- 若 teloxide 升级/破坏性改动 → 可退到直接基于 `teloxide-core`（更薄、API 稳定）。
- 若需更高吞吐（多账户）→ 用 `webhooks-axum` + 共享 `axum::Router`（与阶段0 的单端口 axum 入口叠加）。

---

## 5. 整体架构与数据流

### 5.1 高层拓扑
```
            ┌───────────────────────────────────────────────────┐
            │                    Telegram                       │
            └───────────────────────┬───────────────────────────┘
                     Webhook(HTTPS) │
            ┌───────────────────────▼───────────────────────────┐
            │   HTTP 入口 (axum/teloxide-webhooks，单端口 PORT)   │
            │  POST /webhook/tg  ·  POST /push/jmap  ·          │
            │  POST /reconcile   ·  GET /healthz /ready          │
            │  GET / + admin session + config APIs (管理 SPA)     │
            └───────────────────────┬───────────────────────────┘
                                命令 │            通知 ↓(Redis Streams worker)
            ┌───────────────────────▼───────────────────────────┐
            │            bot 层 (handlers)                     │
            │  /list /read /send /search /folders /flag ...     │
            └───────────────────────┬───────────────────────────┘
                                领域 │
            ┌───────────────────────▼───────────────────────────┐
            │        jmap 适配层 (JmapService)                   │
            │  list_folders/list_emails/read_email/send_email…  │
            │  + sinceState 缓存 → 外部 Redis (MOD-SINCESTATE)  │
            └───────────────────────┬───────────────────────────┘
                                JMAP │ HTTPS（短请求）
            ┌───────────────────────▼───────────────────────────┐
            │     Stalwart JMAP Server                          │
            │  session_url / email/* / mailbox/* / submission/* │
            └───────────────────────▲───────────────────────────┘
                     Push 回调(HTTPS)│  + 外部 Cron 对账(FLOW-RECONCILE)
            ┌───────────────────────┴───────────────────────────┐
            │  push handler → Redis Streams(MOD-STREAMS)        │
            │  worker: Email/changes → 通知 → 发往 TG → 推进游标│
            └───────────────────────────────────────────────────┘
```
> 部署形态 = **通用 HTTPS-only Docker、无长连接**（deployment.md `C-NO-LONG-CONN`/`C-HTTPS-INBOUND`）：
> Telegram 仅 Webhook；JMAP 实时仅 Push 回调；慢任务异步入 Redis Streams；对账由**外部 HTTPS Cron** 触发。
> EventSource/SSE 与长轮询均为非目标（`NG-POLLING-SSE`/`NG-LONG-POLLING`）。
> 渠道解耦（见 §5.2）：领域层（`domain/`）与渠道层（`channel/`）以领域 Command / Notification 交互，不出现 Telegram/teloxide 类型；钉钉/飞书仅保留扩展位，不提前实现。

### 5.2 渠道抽象与多通道扩展策略
- **现状**：首个（也是当前唯一）渠道是 Telegram。目标是保留平行扩展钉钉/飞书的能力，**但不提前实现**。
- **原则**：领域层与渠道层解耦——邮件/JMAP/AI/意图状态机**不得依赖 Telegram 类型**。
- **抽象（不过度设计）**：

  | 抽象 | 职责 | 首个实现 |
  |---|---|---|
  | `Channel` | 渠道生命周期：接收输入、分发命令、配置端点 | `TelegramChannel`（teloxide Dispatcher） |
  | `Notifier` | 主动推送：把领域 `Notification` 发送到用户 | `TelegramNotifier`（teloxide requester） |
  | `MessageAdapter` | 领域数据 ↔ 渠道消息渲染（文本/按钮/转义） | `TelegramMessageAdapter`（`render.rs`） |

  - 领域与渠道之间用**领域 Command / 领域 Notification** 数据结构传递，渠道只在边缘做适配（解析→领域 Command；领域 Notification→渲染）。
- **约束**：
  - `jmap` / `llm` / 意图状态机 / `notify::core` 的公开接口只接受/返回领域类型；
  - `teloxide` 类型只允许出现在 `channel/telegram/` 内；
  - 新增渠道 = 新 adapter + 装配，不改领域层。
- **不做什么**：不定义多态配置注册表、不预先抽象"渠道能力矩阵"、不建 plugins 机制；按需再演进（YAGNI）。

### 5.3 运行模型（短请求，无长连接）
- 单一二进制 `message-weave`，`#[tokio::main]`。
- 启动时：
  1. 加载配置（`config::Config`）。
  2. 构造 `JmapService`（`Client::connect` 完成 session 解析、account_id 缓存、mailbox role→id 映射预热）；sinceState 从外部 Redis 恢复（`MOD-SINCESTATE`）。
  3. 启动 **HTTP 入口**（axum，单端口 `PORT`）：`/webhook/tg`、`/push/jmap`、`/reconcile`、`/healthz`、`/ready`；其中三条写路径先经 `SAF-AUTH-*` 入口鉴权（fail-closed，§7.3），`/healthz`、`/ready` 为公开轻量探针（`SAF-PROBE-PUBLIC`），不代表外部依赖已完成端到端验收。
  4. 启动 **Redis Streams worker**（后台 task）消费 Push 事件 → `Email/changes` → 通知 → 发往 TG → 推进 sinceState → XACK。
  5. **不持有任何长连接、不自建定时器**（`C-NO-LONG-CONN`）；对账由**外部 HTTPS Cron** 触发 `/reconcile`（`FLOW-RECONCILE`）。
- 通知发送与命令处理共享 `Arc<JmapService>`，内部 `tokio::sync::RwLock` 保护可变缓存；跨请求状态一律落外部 Redis（`C-REDIS-ONLY-STATE`）。
- **多实例与负载均衡（部署形态，`ARCH-LB-WORKER`）**：由于状态全在外部 Redis（`C-REDIS-ONLY-STATE`）且投递幂等（`MOD-DEDUP`），**同一镜像可跨多个 serverless 平台实例化**，前面用免费 Cloudflare Worker 做唯一入口与故障转移（`C-LB-SINGLE-REG-URL`）；`/reconcile` 用 Redis 锁单实例执行（`SAF-RECONCILE-LOCK`），Streams 用同一消费组自动分摊（`MOD-STREAMS-GROUP`）。详见 deployment.md §10。**领域/渠道逻辑无需改动。**
- **生产红线（`C-NO-DB` / `C-NO-LOCAL-WRITE` / `C-LOG-STDOUT-ONLY` / `SAF-LOG-PURITY` / `C-NO-STATEFUL-RECOVERY`，详见 deployment.md §0 / §8.2）**：
  - **生产不使用任何数据库**（`C-NO-DB`）：无 SQLite/Postgres/MySQL/嵌入式数据库；Redis 是唯一生产状态存储。
  - **禁止本地文件/目录写入**（`C-NO-LOCAL-WRITE`）：无日志文件、无数据文件、无临时缓存、不挂载本地卷。
  - **日志只写 stdout/stderr**（`C-LOG-STDOUT-ONLY`）：容器/平台负责采集落盘；禁用文件日志后端。
  - **日志与 Redis 写入内容约束**（`SAF-LOG-PURITY`）：仅限结构化事件、计数、时间戳、脱敏摘要；**禁止**密钥原文、JMAP 邮件正文、AI 请求/响应、附件内容。
  - **禁止依赖进程内状态做生产恢复**（`C-NO-STATEFUL-RECOVERY`）：重启续跑（去重、sinceState、Streams 断点、熔断计数、会话）一律由外部 Redis + JMAP 对账实现；进程内缓存仅为性能优化，丢失必须安全可重入。
- 优雅退出：信号 + `CancellationToken`，退出前 flush Redis Streams 待处理条目（**不写本地文件**，`C-NO-LOCAL-WRITE`）。
- **当前现状**：完成配置引导、HTTP 入口和写入口鉴权（`R1`/`SAF-AUTH-*`，fail-closed）；JMAP session、Email/changes、PushSubscription create/update、Redis Streams worker 和 `/reconcile` 已接入。`/reconcile` 与 Push 闭环仍需在真实 Stalwart、Redis、Telegram 环境完成端到端验收，不能仅凭本地门禁宣称生产链路已验证。

### 5.4 数据流：新邮件推送（关键路径，FLOW-NEW-MAIL）
```
Stalwart JMAP Push → POST /push/jmap (StateChange{Email/EmailDelivery: new_state})
  → 校验 pushSubscriptionId + verificationCode → 幂等去重(MOD-DEDUP) → 入 Redis Streams(MOD-STREAMS) → 立即 2xx ACK
  → worker: XREADGROUP → 用 sinceState(存 Redis, MOD-SINCESTATE) 调 Email/changes → 取 created[] 的 id
  → email_get([From, Subject, Preview, ReceivedAt, HasAttachment])
  → notify::core 组装领域 Notification（仅元数据 + 行内按钮意图，绝不含正文）
  → channel::Notifier.send(chat, notification)（Telegram adapter 渲染并发送）
  → 更新 sinceState（写外部 Redis）→ XACK
  → 若 Redis 丢失：由外部 Cron 对账(FLOW-RECONCILE) 重建游标并补发
```
> 关键：`Email/changes` 在 `created` 列出新建邮件 id，避免全量 `email_query`；`sinceState` 存外部 Redis（`C-REDIS-ONLY-STATE`），Redis 丢失时由对账从 JMAP 重建，**事实源在 JMAP**。
> 通知只含发件人/主题/时间（+附件数），正文绝不出现在通知里（见 AGENTS.md 安全边界 `SAF-NOTIFY-META`）。
> 无长连接：Push 为短请求回调，对账由**外部 HTTPS Cron**触发（`C-NO-LONG-CONN`），非容器内自持定时器。

### 5.5 数据流：命令 `/read <seq>`（长邮件分支）
```
TG /read 3 → handler 取会话里的 folder+page 游标
  → JmapService.list_emails(folder, page=3)
  → 取第 3 封 id → read_email(id, body=true)   # JMAP 直取，绝不经 LLM
  → 格式化：
      正文 ≤ long_email_char_limit(默认 4000) → 正常发送（HTML escape）
      正文 > 阈值 → 不发全文，发 Preview(120) + 发件人/主题/附件概览 + 选项：
        "[AI 总结](约300字，需确认) / [继续查看原文(截断，标注‘完整请电脑查看’)]"
  → reply
（"AI 总结"分支必须用户确认后才把正文交 LLM 生成 ~300 字摘要；见 §12.3）
```

### 5.6 会话状态机（对话 FSM）
**目标设计**：命令路由基于对话 FSM（多步交互如 `/send`、`/summarize` 确认）。当前实现**未引入 teloxide**，worker 用 `parse_intent` 解析为 `Intent`，AI 授权态走 Redis TTL 而非多步确认态；AI 与附件相关流程必须经过显式确认态：

| 状态 | 含义 | 进入 | 离开 |
|---|---|---|---|
| `Idle` | 空闲 | 任意完成态 | 收到消息 |
| `AwaitClarify` | 目标/意图不明，等用户选择 | 意图或邮件目标不唯一 | 用户给出明确选择 |
| `AwaitConfirm` | 等确认（AI 分析 / 附件下载） | 用户发起分析/下载但未确认 | 确认 / 取消 |
| `Analyzing` | AI 请求 in-flight | 用户确认分析 | 成功 / 失败 |
| `AwaitFallback` | AI 失败，等确认回退 | 连续 3 次失败熔断 | 用户确认 / 取消 |

不变量：`AwaitConfirm`/`Analyzing`/`AwaitFallback` 涉及 AI 或附件下载，**未到确认态不得调用 LLM 或拉取附件**。会话状态为短期状态，统一走**外部 Redis 短期 TTL**（`C-REDIS-ONLY-STATE`；不使用 SQLite，丢失可接受），运维 TTL 见 deployment.md（`C-REDIS-ONLY-STATE`）。
> 渠道中立：FSM 状态与事件用领域类型（`domain::state`），不依赖 teloxide；当前实现未引入 teloxide，会话/授权态统一走外部 Redis 短期 TTL。

---

## 6. 模块划分

目标 cargo 工程结构（**阶段0 已落地** `main.rs`/`config.rs`/`error.rs`/`domain.rs`/`channel.rs`/`notify.rs`；下方 `jmap/`、`llm/`、`channel/telegram/`、`notify/*` 为**后续阶段目标**，详见 §10）：
```
message-weave/
├── Cargo.toml
├── .gitignore                    # 排除 target/ 等（阶段0 P0 门禁 GATE-P0）
├── src/
│   ├── main.rs                   # tokio main：装配置；按 RUN_MODE 分派 webhook / reconcile
│   ├── config.rs                 # 环境变量解析（ARCH-CONFIG-ENV：std::env，非 figment/TOML）
│   ├── error.rs                  # BotError 统一错误（见 §8）
│   ├── domain/                   # 领域层（不依赖任何渠道类型，见 §5.2）
│   │   ├── command.rs            # 领域 Command（List/Read/Analyze/Confirm…，渠道无关）
│   │   ├── notification.rs       # 领域 Notification（NewEmail/ResultMsg/ConfirmRequest…）
│   │   ├── state.rs              # 会话状态机（§5.6：Idle/AwaitClarify/AwaitConfirm/Analyzing/AwaitFallback）
│   │   ├── jmap/                 # JmapService（领域封装，§3.2）—— 阶段1+
│   │   │   ├── client.rs         # 包装 jmap_client::Client（构造、重连、自定义 reqwest）
│   │   │   ├── model.rs          # Folder / EmailSummary / EmailBody 领域模型
│   │   │   └── state.rs          # mailbox role→id 缓存、sinceState 游标（写外部 Redis，MOD-SINCESTATE；不用 SQLite/本地卷）
│   │   └── llm/                  # LlmService + policy + fallback + audit（§12.1）—— 阶段3.5
│   ├── channel/                  # 渠道层（每渠道一个 submodule，见 §5.2）
│   │   ├── mod.rs                # Channel / Notifier / MessageAdapter traits（thin）—— 阶段0 边界已落地
│   │   └── telegram/             # 首个也是当前唯一渠道（adapter #1）—— 阶段2+
│   │       ├── mod.rs            # TelegramChannel / TelegramNotifier 装配（teloxide Dispatcher）
│   │       ├── commands.rs       # 领域 Command ↔ teloxide 命令映射（/folders /list /read /send /search /flag）
│   │       ├── render.rs         # TelegramMessageAdapter：领域 Notification → TG 消息/行内按钮（HTML escape！）
│   │       └── session.rs        # teloxide 会话键（当前 folder/page/last_email_id）
│   │       # 预留扩展位：dingtalk/、feishu/（不实现，见 §5.2）
│   ├── notify/
│   │   ├── core.rs               # 新邮件处理：JMAP 增量 → 领域 Notification（渠道无关）
│   │   ├── push_handler.rs       # POST /push/jmap：校验 subscription ID + verificationCode → 去重 → 入 Redis Streams → 2xx
│   │   ├── worker.rs             # Redis Streams 消费：Email/changes → 通知 → 推进 sinceState → XACK
│   │   └── reconcile.rs          # /reconcile：外部 Cron 触发对账补差（FLOW-RECONCILE，Redis 丢失恢复）
│   └── util/
│       ├── retry.rs              # 指数退避（Transport/RateLimit/ServerUnavailable）
│       └── shutdown.rs           # CancellationToken 协调
└── tests/
    ├── jmap_mock.rs              # 对 JmapService 的契约测试（mock Push 回调 + changes 响应）
    ├── channel_mock.rs           # MockChannel/Notifier 契约：领域流程不依赖 Telegram
    └── telegram_dispatch.rs      # teloxide 测试模式（mock Bot，仅 channel::telegram）
```

### 6.1 模块职责矩阵

> **依赖现况**：阶段0 已落地（`main` / `config` / `error` / `domain` / `channel` / `notify`，见 §10.0）；`jmap-client 0.4.2` **已引入**（`ARCH-DEPS-STAGE1`）；`redis`(`0.27`) / `reqwest`(`0.13`) 亦**已引入**（`ARCH-DEPS-STAGE4`，版本以 `Cargo.toml` 为准）。`teloxide` 经评估后**未引入**——Telegram 渠道改用 `reqwest` 自研实现。下表保留选型评估的原始记录（含未采纳方案），标「计划」的行已不代表当前计划。

| 模块 | 依赖 | 输出 | 可测性 | 现状 |
|---|---|---|---|---|
| `config` | 标准库 `env`（`ARCH-CONFIG-ENV`） | `Config` 结构 | 纯函数，易测 | 阶段0 已实现（手工 `from_env`，无 figment） |
| `domain::jmap`（含 `client`） | **jmap-client 0.4.2（已引入）** | 领域动词（§3.2）；`client` = 真实只读 adapter（`MOD-JMAP-CLIENT`） | mock JMAP 响应 + `#[ignore]` 真机测试 | 阶段1（G1/D-G1-1 **代码已实现，待真实 `cargo test -- --ignored jmap::` 验证**） |
| `domain::llm` | reqwest/axum（计划） | 摘要/意图/草稿 | mock OpenAI 端点 | 阶段3.5 |
| `domain::state` | — | 意图路由 + 会话 FSM（零渠道依赖） | 表驱动纯单测 | 阶段3+ |
| `channel`（traits） | — | Channel/Notifier/MessageAdapter | 契约测试（MockChannel） | 阶段0 边界已实现 |
| `channel::telegram` | teloxide（计划） | TG adapter：事件→领域 Command；领域 Notification→渲染 | teloxide mock Bot / MockChannel | 阶段2+ |
| `notify`（入口 + Push/worker/reconcile） | axum；Redis Streams | HTTP 鉴权、全局开关、Push 入队、`Email/changes` 对账和游标提交 | 领域 Notification → Streams worker | 当前实现已覆盖真实对账路径；仍需真实 Stalwart 环境做端到端验收 |
| `util::retry` | — | 重试策略 | 纯逻辑 | 阶段1+ |

---

## 7. 配置与安全

### 7.1 历史配置读取（已迁移至 Redis）

> **迁移目标（`C-REDIS-ONLY-STATE`）**：生产启动环境仅保留 `REDIS_URL`。空 Redis
> 的首次配置必须通过 Redis ACL 密码认证的一次性 bootstrap/admin 会话完成；不得
> 提供未鉴权写入口。ACL 密码仅用于常数时间 bootstrap 校验，不回显、不记录、不写入
> 业务配置。bootstrap 成功后管理员会话哈希及 TTL 保存在 Redis，重启可恢复；业务
> token 从 Redis 在启动时装载，密钥 GET 永不回显；bootstrap/管理员 PUT 成功后先构建并原子
> 替换客户端，后续请求即时使用新配置，失败保留旧实例。该迁移替代下述阶段0环境变量清单，
> 阶段0列表仅作为历史兼容说明。

> **实际实现**（`ARCH-CONFIG-ENV`）：阶段0 起配置**只从环境变量读取**，由 `config::Config::from_env()` 手工解析（`std::env::var`），**不使用 figment、不使用 TOML 配置文件**。缺失必填项即启动失败；布尔值接受 `true/1/yes` 与 `false/0/no`。

> ⚠️ **下表是 `Config::from_env()` 遗留/引导路径的完整变量表**（阶段0 口径，保留作兼容参考）。**当前生产部署只需要 `REDIS_URL` + `CONFIG_ENCRYPTION_KEY` 两个启动变量**（`src/main.rs`）；其余业务字段已迁移到 Redis 业务配置（`PUT /api/business-config` 热加载，`C-REDIS-ONLY-STATE`）。表中的「必填」仅指**走环境变量引导路径时**必填，不代表当前生产必须配置。

| 变量 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `PORT` | 否 | `8080` | 单监听端口（`C-NO-TCP-EXPOSE`） |
| `RUN_MODE` | 否 | `webhook` | `webhook` / `reconcile`（`NG-SERVER-MODE` 已删除） |
| `BOT_TOKEN` | 是 | — | Telegram Bot Token（`SecretString`） |
| `TG_WEBHOOK_SECRET` | 是 | — | `/webhook/tg` 鉴权：请求头 `X-Telegram-Bot-Api-Secret-Token`（`SAF-AUTH-TG-WEBHOOK`；`SecretString`） |
| `CHAT_ALLOWLIST` | 是 | — | 逗号分隔整数 chat id；**白名单硬约束**（`SAF-CHAT-ALLOWLIST`） |
| `JMAP_SESSION_URL` | 是 | — | Stalwart JMAP session URL（`REQ-JMAP-SESSION-URL`）：可填**服务基地址** `https://host[:port]` 或**完整** `…/.well-known/jmap`；代码归一化为 origin/base 后再交 `jmap-client`（其自动追加 `.well-known/jmap`），**不得重复路径**。仅 HTTPS、**禁止 URL 内嵌凭据**（`SAF-JMAP-URL`） |
| `JMAP_USERNAME` | 是 | — | Stalwart 账户（邮箱） |
| `JMAP_PASSWORD` | 是 | — | **App Password**（`C-AUTH-APP-BASIC`；`SecretString`） |
| Push verification | 否 | — | Stalwart 动态生成；后端通过 `PushSubscription/set` 自动回写并在 Redis 保存短期验证状态 |
| `REDIS_URL` | 是 | — | 外部 Redis（用户托管 + AOF，`C-REDIS-MANAGED-AOF`；`SecretString`） |
| `RECONCILE_TOKEN` | 是 | — | `/reconcile` 鉴权：`Authorization: Bearer`（`SAF-AUTH-RECONCILE`；`SecretString`）。`/reconcile` 路由始终挂载，故**必填** |
| `ACCOUNT_ID` | 否 | 空 | 单账户（`REQ-SINGLE-ACCOUNT`）；留空则取 session 主账户 |
| `LLM_ENABLED` | 否 | `false` | 关闭时不校验 `LLM_*`（`REQ-LLM-OPENAI-COMPAT`） |
| `LLM_ALLOW_NET` | 否 | `false` | AI 出网开关（`REQ-AI-EXTERNAL-CONSENT`） |
| `LLM_API_KEY` / `LLM_BASE_URL` / `LLM_MODEL` | `LLM_ENABLED=true` 时必填 | — | OpenAI-compatible |
| `LLM_MAX_RETRIES` | 否（环境变量路径**不读取**） | `3` | 熔断阈值（`REQ-AI-FUSE`）。当前经 Redis 运行参数管理（`OutboundConfig.max_retries`，回落默认 `3`，见 `src/ai.rs`）；`Config::from_env()` **不读取**此变量 |
| `LLM_SUMMARY_TARGET_CHARS` | 否 | `300` | 摘要目标字数（`REQ-LONG-EMAIL`） |

> 当前 `config.rs` 通过环境变量和 Redis 业务配置提供上述字段（含 `TelegramConfig` / `JmapConfig` / `LlmConfig`）。未来若引入 TOML/figment 需另立决策；当前文档不假设配置文件存在。
> **入口鉴权密钥（`TG_WEBHOOK_SECRET`/`RECONCILE_TOKEN`）在业务配置完成后必须有效**（fail-closed，`SAF-AUTH-*`）。JMAP Push 的验证码由 Stalwart 在订阅创建后动态生成，不属于业务配置。启动引导变量缺失时服务进入配置引导模式并保持 SPA 可访问；不会因为缺少启动变量而伪造业务成功，也不会绕过入口鉴权。
> **JMAP session URL 归一化（`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）**：`JMAP_SESSION_URL` 既接受**服务基地址**（`https://mail.example.com`）也接受**完整 session URL**（`https://mail.example.com/.well-known/jmap`）。代码在调用 `jmap_client::Client::connect` **之前统一归一化为 origin/base**——`jmap-client` 会自动追加 `/.well-known/jmap`，故**不会出现重复路径**（如 `…/.well-known/jmap/.well-known/jmap`）。约束：**仅 HTTPS**（http 拒绝）；**禁止 URL 内嵌用户名/密码**（凭据只经 `JMAP_USERNAME`/`JMAP_PASSWORD`，`C-AUTH-APP-BASIC`）；拒绝危险 query。（状态：D-G1-1 **代码已实现，待真实 `cargo test -- --ignored jmap::` 验证**。）

### 7.2 密钥管理
- **禁止**把 token/密码写入仓库或任何配置文件（本项目配置走环境变量，`ARCH-CONFIG-ENV`）。
- 环境变量由 `config` 层直接读取（`std::env::var`，`ARCH-CONFIG-ENV`）；缺失必填项即启动失败（**无** `${VAR}` 插值/figment）。
- 运行期用 `secrecy::SecretString` 包裹，`Debug` 实现打 `***`。
- 日志过滤：`tracing` 字段层屏蔽 `Authorization`/`password`/`token`。
- 运行期 secret 注入方式（env / `*_FILE` / 编排器 secret）见 deployment.md（`C-NO-SECRET-IN-IMAGE`）。

### 7.3 访问控制
- **入口鉴权（硬约束 `SAF-AUTH-RECONCILE`/`SAF-AUTH-TG-WEBHOOK`/`SAF-AUTH-JMAP-PUSH`）**：三条写路径必须先鉴权，**fail-closed**——
  - `/reconcile`：请求头 `Authorization: Bearer <RECONCILE_TOKEN>`；
  - `/webhook/tg`：请求头 `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET`；
  - `/push/jmap`：按 `pushSubscriptionId` 校验 Redis 短期状态；首次 verification 由 JMAP `PushSubscription/set` 回写成功后才放行 StateChange。
  比较使用**常数时间**算法（`subtle`，防时序侧信道）；校验失败一律 `401` 且**在鉴权通过前不产生任何副作用/状态变更**。业务配置完成后，Webhook、Push、Reconcile 和 Push 注册接口的凭证必须有效；启动引导变量缺失时进入配置引导模式，不绕过鉴权（§7.1）。
- **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz`（`ARCH-HEALTHZ`）与 `/ready` 为**公开探针**——无鉴权、只返回健康状态、**不含任何敏感信息**（不回显配置/密钥/内部错误细节）。
  - `/healthz` = liveness（进程存活），语义长期稳定。
  - `/ready` 检查配置完整性与 Redis 可访问性；未就绪返回 `503`，不执行 JMAP/Telegram 请求，也不触发邮件同步等业务副作用。
- **chat 白名单（硬约束 `SAF-CHAT-ALLOWLIST`）**：`CHAT_ALLOWLIST` 是**必填**配置；任何入站事件（TG 命令 / 回调触发的动作）在**做任何 JMAP 调用、AI 调用或状态变更之前**，必须先校验 `chat.id ∈ CHAT_ALLOWLIST`，不在白名单则**直接拒绝并终止**（防止 token 泄露后被任意人调用）。阶段0 已完成 `CHAT_ALLOWLIST` 解析骨架；强制拒绝逻辑已随 Telegram 渠道接入落地（`src/notify.rs` 的 `telegram_webhook` 在任何 JMAP/AI/状态操作之前先校验白名单，拒绝即终止）。
- **命令最小化**：仅暴露必要命令；`/send` 需二次确认。
- **速率**：出站侧未建本地令牌桶（未引入 `teloxide`）；Telegram 出站发送按 Redis 运行参数 `max_retries`（默认 3、上限 5）重试；当前仅对 Push 验证码写入做 Redis 限流（`ratelimit:push-verify:*`）。Telegram 服务端 30 msg/s 限制下的 429 不做专门的自动退避处理。

### 7.3.1 配置管理 API
- `GET /` 提供嵌入 Rust 二进制的 SPA；`/assets/config.js` 与 `/assets/styles.css` 提供页面资源。服务不在运行时读取或写入本地文件（`C-NO-LOCAL-WRITE`）。
- `GET /api/status` 公开返回 `{ "ready": boolean, "mode": "configured" | "configuration-setup", "missing": string[] }`，只列缺少的环境变量名称。缺少 `REDIS_URL` 或 `CONFIG_ENCRYPTION_KEY` 时，SPA 只显示配置引导状态与缺失变量；服务状态确认 ready=true 后才显示管理会话授权区。
- `POST /api/admin/session` 接受 `Authorization: Bearer <REDIS_URL ACL password>`，返回 `{ "session": "<opaque>", "expires_in": 900 }`；admin session 仅存 Redis 中的摘要并在 900 秒后过期。`POST /api/admin/session/revoke` 撤销当前 session，成功返回 `204`。
- 管理页面仅在 JavaScript 内存中保存 session。请求设置 `credentials: omit`、`cache: no-store`，不使用 Cookie、localStorage 或 sessionStorage。管理 API 接受有效 admin session；兼容路径也接受 `WORKER_TOKEN`。Worker 原样透传鉴权头（`SAF-LB-PASSTHRU`）。
- `GET /api/config` 与 `PUT /api/config` 只读取和写入非敏感运行参数：
  ```json
  {
    "jmap_timeout_ms": 15000,
    "telegram_timeout_ms": 10000,
    "llm_timeout_ms": 30000,
    "max_retries": 3
  }
  ```
- 三个 timeout 单位均为毫秒，范围 `100..=300000`；`max_retries` 范围 `0..=5`，表示首次请求之外的最大重试次数。Redis 中尚无配置时 GET 返回默认值 `15000 / 10000 / 30000 / 3`；PUT 成功返回保存后的相同对象并持久化到 Redis（`C-REDIS-ONLY-STATE`）。
- `PUT /api/business-config` 接受完整 `BusinessConfigWire`，完整替换加密保存的业务配置，先构建客户端再切换运行 worker；Push verification 不属于 Wire，由 Stalwart 动态生成并由后端回写。成功返回 `204 No Content`，不会返回配置或密钥。
- `PUT /api/config` 的运行参数错误使用 `401`（未授权）、`400`（JSON 无效）、`422`（范围错误）、`503`（Redis 不可用）；GET 读取失败时也以 `503` 表示 Redis 不可用。业务 PUT 使用 `401`、`400`、`422` 与 `503`（Redis 写入或依赖客户端构建失败）；其余错误不回显敏感数据。Redis/会话初始化未完成时管理页面显示 `503` 状态；运行参数在重新读取成功前禁用保存（`C-REDIS-ONLY-STATE`）。LLM 启用时，业务配置要求提供 HTTPS Base URL、非空 API key 和模型名。

### 7.4 TLS（代码/依赖行为）
- TLS provider 由依赖 feature 决定：**jmap-client 0.4.2 默认含 `aws_lc_rs`（并引入 `rustls`）**，`default-features = true` 时并非"rustls 默认"。**本项目实际 `default-features = false, features = ["async","rustls"]`**（不启用 `aws_lc_rs`/`websockets`），最终以 `Cargo.toml` 为准。
- 证书校验**默认强制**，仅测试可关。
- 镜像内系统 CA 信任库、内部 CA 注入、入站 webhook TLS 终止等运维细节见 deployment.md。

### 7.5 生产红线：无数据库 / 无本地写入 / 标准输出日志（deployment.md §0/§8.2）

本节汇总跨代码与部署的绝对红线，**不得**在代码中引入任何"看起来方便"的本地状态：

| 红线 ID | 含义 | 违反示例（禁止） |
|---|---|---|
| `C-NO-DB` | 生产不使用任何数据库；Redis 是唯一生产状态存储 | 加 SQLite 去重表、加 Postgres 会话 |
| `C-NO-LOCAL-WRITE` | 禁止本地文件/目录写入；不挂本地卷 | 写 `./logs/app.log`、写 `./tmp/…`、写本地 `sinceState.json` |
| `C-LOG-STDOUT-ONLY` | 日志只写 stdout/stderr；平台负责采集 | 引入 `tracing-appender`、`rolling-file`、`FileAppender` |
| `SAF-LOG-PURITY` | 日志与 Redis 写入内容仅限结构化事件、计数、时间戳、脱敏摘要 | 日志打印 JMAP 正文、AI prompt/completion、密钥原文、附件内容 |
| `C-NO-STATEFUL-RECOVERY` | 生产恢复不依赖进程内状态 | 用 `static Mutex<HashSet>` 存 dedup、用内存 LRU 存 sinceState 作为唯一恢复源 |

**验证**：静态检查代码不出现本地路径常量 / `std::fs` 非测试调用 / 文件日志后端；运行期通过容器 `/proc/mounts` 与 `docker inspect` 确认无本地卷挂载；部署检查清单见 deployment.md §8.2。

---

## 8. 错误处理与可观测性

### 8.1 统一错误枚举
> 以下为设计目标形态。当前 `src/error.rs` 的 `BotError` 仅含 `Config` / `Io` / `Json` / `State` 四个变体；`Telegram` 变体未引入（`teloxide` 未使用）。

```rust
#[derive(thiserror::Error, Debug)]
pub enum BotError {
    #[error("jmap: {0}")]
    Jmap(#[from] jmap_client::Error),
    #[error("telegram: {0}")]
    Telegram(#[from] teloxide::errors::RequestError),
    #[error("config: {0}")]
    Config(String),
    #[error("storage: {0}")]
    Storage(#[from] redis::RedisError),
    #[error("rate limited, retry later")]
    RateLimited,
    #[error("unauthorized chat {0}")]
    Unauthorized(i64),
    #[error("llm: {0}")]
    Llm(LlmErr),   // 见 §12.7
}
```

### 8.2 错误 → 用户消息 映射
| 底层错误 | Bot 行为 |
|---|---|
| `Transport` / `ServerUnavailable` / `RateLimit` | 指数退避重试（`util::retry`），超阈值给用户"暂时不可用，稍后重试" |
| `Forbidden` / `Unauthorized chat` | 静默拒绝；日志告警 |
| `Method(InvalidArguments)` | 回复"参数有误 + 正确用法" |
| `Set(creation/update/destroy)` | 回复具体记录级失败原因 |
| `StateMismatch` | 重置 sinceState → 全量补一次（防丢/防重） |
| `Llm(*)` | 见 §12.7（熔断/回退/徽标） |

### 8.3 可观测性
- `tracing`（直接依赖，非 `teloxide` feature）+ `tracing-subscriber`（fmt + EnvFilter）。
- 关键事件：Push 回调到达/验证/拒绝、Reconcile 拉取与入队、渠道推送成功/失败/重试、LLM 调用耗时与状态等均落 `tracing` 事件（stdout）；**不记 prompt 明文与密钥**。当前未使用命名 span（无 `#[instrument]` / `span!`），字段级结构化观测待后续补齐。
- 指标（可选 `metrics` crate）：Push 回调到达数、去重命中率、Redis Streams 积压深度（pending）、DLQ 条数、对账补差条数、JMAP 请求延迟、推送失败率。
- 优雅退出：信号处理 + `CancellationToken`，退出前 flush Redis Streams 待处理条目（不涉及本地文件）。
- **可靠性目标与策略**（Streams ACK/retry、幂等去重、Push 重试、对账恢复、指标/告警、**≥99.9% 通知可用性及边界**）见 deployment.md §6.4/§6.5（`NFR-NOTIFY-SLA`）。

---

## 9. 测试策略

### 9.1 层次
| 层 | 工具 | 内容 |
|---|---|---|
| 单元 | `cargo test` | `config` 解析、`util::retry` 退避、`formatting` HTML escape |
| 契约 | mock | `jmap::JmapService` 用 mock Push 回调 + changes 响应 + 预置 JMAP JSON 验证领域动词；`llm` 用 mock OpenAI 端点 |
| 集成 | teloxide mock Bot + mock JMAP | 命令端到端：发 `/list` → 验证回复来自 mock 数据 |
| 真实（手动） | Stalwart 实例 | 标 `#[ignore]`，`cargo test -- --ignored` 按需跑 |

### 9.2 mock 策略
- `JmapService` 持有 trait `JmapBackend`（`list_emails/read_email/send_email/changes…`），生产实现包装 `jmap_client::Client`，测试用 `MockBackend`（基于 `Vec<EmailSummary>`）。
- Push 回调 payload 用 `futures::stream::iter` 造假 `StateChange`，验证 worker 的 `Email/changes` 增量逻辑与去重（`mod_dedup` + `mod_streams`）。
- teloxide 提供 `Requester` mock，验证 `send_message` 被以期望内容调用（仅 `channel::telegram` 测试使用）。
- 渠道契约：领域流程用 `MockChannel`（实现 `Channel/Notifier` trait）驱动，验证领域输出与渠道渲染解耦、领域层无 teloxide 依赖。
- `llm::client` 注入 mock OpenAI `/chat/completions` 端点。

### 9.3 关键不变量测试
- `sinceState` 存外部 Redis：模拟重启后从正确游标续传；模拟 Redis 清空后由对账（`reconcile`，`FLOW-RECONCILE`）从 JMAP 恢复游标。
- 通知去重：同一 `email_id` 不重复推送。
- 正文转义：含 `<script>` 的邮件正文在 HTML 模式下被转义。
- **安全边界断言（硬性，见 AGENTS.md）**：
  - VIEW/查看原文路径：mock AI 端点零请求（LLM client 未被调用）；
  - 新邮件通知：消息内无正文内容（断言泄漏）；
  - 长邮件：正文 > 4000 字符不发送全文；
  - AI 调用前置：未确认前 LLM 零调用；
  - 分析结果不落盘：处理后无新增磁盘/Redis 写入路径；
  - AI 3 次失败 → 熔断确认 → 回退带"AI 不可用"徽标。
  - **入口鉴权（`SAF-AUTH-*`）**：`/reconcile`、`/webhook/tg`、`/push/jmap` 在**缺少或错误的**凭证下返回 `401`，并断言鉴权失败时**无副作用**（无 Redis 写入、无 JMAP/AI 调用）；正确凭证放行。
  - **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz` 返回 `200` 表示进程存活；`/ready` 以 `200/503` 表示配置与 Redis 是否就绪，响应体不含敏感信息。
  - **渠道解耦**：领域模块（jmap/llm/intent/notify::core）编译不依赖 teloxide（crate 分层 + clippy/评审约束）；`channel::telegram` 是唯一导入 teloxide 的模块。
  - **JMAP session URL（`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）**：基地址与完整 `…/.well-known/jmap` 两种输入**均接受且归一化结果一致**，传给 `Client::connect` 的 URL **不含重复 `/.well-known/jmap`**；`http://` 被拒绝；**内嵌凭据（`https://user:pass@host`）被拒绝**；危险 query 被拒绝。
  - **JMAP 多 part 原文（`REQ-JMAP-RAW-MULTIPART`）**：`read_email` 对多 part 正文按 `text_body` 顺序拼接"有 `part_id` 且有 `bodyValue`"的部分；构造"无可用部分"用例断言返回**明确错误**（非空串）。

---

## 10. 分阶段实施计划

> 前提：先 `rustup` 装工具链（stable）。CI 与发布流见 deployment.md。

### 10.0 阶段 0：脚手架与 HTTPS 入口骨架（已完成，待过 P0 门禁）

> **当前实际边界**：单端口 axum 入口提供 `/webhook/tg`、`/push/jmap`、`/reconcile`、`/healthz`、`/ready`。三条写路径的入口鉴权已 fail-closed 落地（`R1`/`SAF-AUTH-*`）；`/reconcile` 已使用 JMAP `Email/changes` 分页、Redis `state:jmap:since` 和 Redis 单飞锁，只有全部事件入队成功后才推进游标；`/healthz` 为 liveness，`/ready` 仍是轻量探针，不代表 Redis/JMAP 依赖已完成业务验收。

- **实际依赖**（Cargo.toml 现状，`ARCH-DEPS-STAGE0`/`ARCH-DEPS-STAGE1` + `ARCH-DEPS-STAGE4`）：`axum 0.8`（单一 HTTPS 入口）、`async-trait`、`secrecy`、`subtle`（常数时间鉴权比较）、`serde`、`serde_json`、`thiserror`、`tokio`、`tracing`、`tracing-subscriber`、`url`（`JMAP_SESSION_URL` 归一化解析）、`jmap-client =0.4.2`（`default-features = false, features = ["async","rustls"]`）、`redis 0.27`（Redis XPING/PING 活性探测，`ARCH-STATE-REDIS`）、`reqwest 0.13`（JMAP/TG HTTP 客户端）；dev-dependencies：`tower 0.5`（路由测试）。`teloxide` 未引入（`ARCH-DEPS-STAGE4`）：Telegram 渠道在 `src/channel.rs` 用 reqwest 自研实现。
- **尚未引入**（文档不得声称已用）：`teloxide`（阶段2 渠道曾计划使用，最终未引入；Telegram 出站由 `src/channel.rs` 用 `reqwest` 直发）。**未使用 figment**：配置为手工 `std::env` 解析（`ARCH-CONFIG-ENV`，§7.1）。
- **阶段1 依赖现况**（`ARCH-DEPS-STAGE1`）：`jmap-client` **已引入**（当前 `=0.4.2`，`default-features = false, features = ["async","rustls"]`，版本与 features **以 `Cargo.toml` 为准**）。⚠️ 其默认 features `["async","websockets","aws_lc_rs"]` **含 WebSocket 栈**，故必须关闭默认 features 且不选 `websockets`；JMAP 仅走 **HTTPS 短请求**（Core/Mail），以遵守 `C-NO-LONG-CONN`（无 WS/SSE/长轮询）。
- `ACCOUNT_ID` **可选**（`REQ-SINGLE-ACCOUNT`）：**留空 → 取 JMAP session 的默认/主账户**；显式值经校验后使用；多账户 = 多个 bot 实例。
- axum 采用 **0.8**（`ARCH-AXUM-08`）；如后续审核决定调整版本，以 Cargo.toml 为准并同步本节。
- 当前不实现 SSE/WebSocket/长轮询/SQLite/本地卷（`C-NO-LONG-CONN`/`NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`）；入口鉴权（`R1`/`SAF-AUTH-*`）作为 fail-closed 硬门禁落地，鉴权之后的 Push、Streams worker 和 `/reconcile` 业务路径已实现。

**阶段0 P0 门禁（`GATE-P0`）——进入阶段1前必须全部通过：**
1. `cargo fmt --check` 通过（无格式差异）。
2. `cargo clippy --all-targets -- -D warnings` 通过（零告警；禁 crate 级 `allow`）。
3. `cargo test` 通过（含路由/配置最小测试）。
4. 上述三条**在 Debian `rust:1-slim-bookworm` 容器内**执行通过（`C-DEBIAN-SLIM`）。
5. 配置读取为 env-only（无 figment/TOML）；`SecretString` 包裹秘密且 `Debug` 不泄密；`JMAP_PASSWORD` 安全访问器保留。
6. `RUN_MODE` 被实际消费（`webhook`/`reconcile` 至少显式分派，保持无副作用）。
7. `.gitignore` 存在（排除 `target/` 等；**不**擅自初始化 git）。
8. 所有新增代码有引用稳定 ID 的必要注释；单 `.rs` ≤ 500 行。

**运行监控（`GATE-UPTIME-KUMA`）：**
- 使用 Uptime Kuma HTTP(s) Monitor 检查 `/healthz`（进程存活）和 `/ready`（配置/Redis 就绪），分别期望 HTTP 200。
- `/healthz` 保持纯 liveness；`/ready` 不执行 JMAP/Telegram 请求或业务副作用。
- 不引入 Prometheus、Exporter 或额外指标端口；真实 Stalwart/Telegram 端到端链路仍需单独联调。

**生产红线（贯穿所有阶段）**：
- **无数据库**（`C-NO-DB`）：Redis 是唯一生产状态存储；不引入 SQLite/Postgres/MySQL/嵌入式数据库。
- **无本地文件写入**（`C-NO-LOCAL-WRITE`）：无日志文件、无数据文件、无临时缓存、不挂载本地卷。
- **日志只写 stdout/stderr**（`C-LOG-STDOUT-ONLY`）：平台负责采集；禁用文件日志后端。
- **日志/Redis 内容约束**（`SAF-LOG-PURITY`）：仅限结构化事件、计数、时间戳、脱敏摘要；禁止密钥、JMAP 邮件正文、AI 请求/响应、附件内容。
- **无进程内恢复**（`C-NO-STATEFUL-RECOVERY`）：重启续跑一律走 Redis + JMAP 对账（`FLOW-RECONCILE`）；进程内缓存仅性能优化，丢失安全可重入。

**阶段1 推进边界（`BOUND-STAGE1`）：**
- 仅在 `GATE-P0` 全项通过后进入阶段1（JMAP 只读）。
- **R1（入口真实鉴权）已落地**（`SAF-AUTH-RECONCILE`/`SAF-AUTH-TG-WEBHOOK`/`SAF-AUTH-JMAP-PUSH`）：`/reconcile` 校验 `Authorization: Bearer RECONCILE_TOKEN`、`/webhook/tg` 校验 `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET`、`/push/jmap` 按订阅 ID 校验 Redis 短期验证状态；常数时间比较，失败 `401` 且无副作用。缺少启动引导变量时服务进入配置引导模式，不伪造业务成功；业务配置完成后再启用对应入口。
- **仍需注意**：本地 CI 门禁通过不等于真实 Stalwart、Redis、Telegram 链路已验收；公网部署前应完成真实端到端测试和外部监控配置。
- 阶段1 只做 JMAP 只读（`list_folders`/`list_emails`/`read_email`），不引入发送/推送/AI。

### 10.1 阶段 1：JMAP 只读（1.5d）

> 前置：`GATE-P0` 全项通过（`BOUND-STAGE1`）。

- 依赖 `jmap-client 0.4.2`（**已引入**，`ARCH-DEPS-STAGE1`；版本/features 以 `Cargo.toml` 为准，**禁用 WebSocket feature**）；`config.rs` 复用既有 env 骨架（**非** `bot.example.toml`，见 `ARCH-CONFIG-ENV`）。
- `domain::jmap::client` = 真实只读 adapter（`MOD-JMAP-CLIENT`，**G1/D-G1-1 代码已实现，待真实 `cargo test -- --ignored jmap::` 验证；未实际运行前不得声称真机通过**）：已实现 = 包装 `Client::connect`（Basic 认证 `C-AUTH-APP-BASIC`）、**URL 归一化**（`JMAP_SESSION_URL` 接受服务基地址或完整 `…/.well-known/jmap`，归一化为 origin/base 后传入，避免重复路径；仅 HTTPS、禁内嵌凭据与 query，`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）、**account 选择**：`ACCOUNT_ID` 留空取 session 主账户、显式值校验后使用（`REQ-SINGLE-ACCOUNT`）。
- `domain::jmap::JmapService::list_folders / list_emails / read_email`（query/get；`list_emails` 含 `limit` 边界；`received_at` 解析；`read_email` 多 part 拼接见 `REQ-JMAP-RAW-MULTIPART`）。
- **R1 入口鉴权已就绪**：`/reconcile`/`/webhook/tg`/`/push/jmap` 及 `/api/push/register` 鉴权（`SAF-AUTH-*`）已落地并通过单测；后续改动不得放宽或绕过鉴权。
- `#[ignore]` 真实测试（`GATE-G1-JMAP-READONLY`）：经**环境变量**驱动，运行命令 `cargo test -- --ignored jmap::`（`--ignored` 是 libtest 参数，必须置于 `--` 之后）；仅编译不执行用 `cargo test --no-run`（编译含 `#[ignore]` 的测试目标）。**缺环境时清晰跳过且不泄露任何密钥**；CI 默认不跑真机用例，mock 测试继续保留。**真机用例通过与否须以实际 `--ignored` 运行为准——不得在未运行的情况下声称"真机通过"。**
- 验收：`cargo test`（mock）全绿；`cargo test -- --ignored jmap::`（有真机环境时）能连真实服务器列文件夹/邮件并读原文（未实际运行前不得声称已通过）。

### 10.2 阶段 2：渠道适配骨架（2d）
- `channel/` 定义 Channel / Notifier / MessageAdapter 抽象 + 领域 Command/Notification 类型。
- `channel::telegram` 装配 Dispatcher + chat 白名单中间件 + throttle（teloxide 仅存在于该模块）。
- 命令：`/start /folders /list /read`（只读闭环）。
- `render.rs`：领域 Notification → TG Markdown/HTML 渲染 + 转义（adapter 内部）。
- 会话存储（外部 Redis，`C-REDIS-ONLY-STATE`；**不用 SQLite**）记当前 folder/page。
- 验收：本地 **webhook 形态**（`webhooks-axum` 或纯 axum），测试客户端发命令能读邮件；领域模块不 import teloxide。

### 10.3 阶段 3：发送邮件 + 状态管理（1.5d）
- `domain::jmap::JmapService::send_email`（draft + submission_set）。
- `/send` 用 teloxide 对话 FSM 多步收集 to/subject/body。
- `/flag /unseen`（keywords）。
- 验收：手机发邮件、标记已读。

### 10.4 阶段 3.5：LLM 门面 + 回退（1.5d）
见 §12.9。

### 10.5 已实现能力：实时推送（Push 回调 + Streams worker + 外部 Cron 对账）
- `notify::push_handler`：`POST /api/push/register` 接受显式 HTTPS callback URL 并创建订阅；接收 Stalwart 的 `PushVerification`，通过 JMAP `PushSubscription/set` 回写验证码，随后 `POST /push/jmap` 校验→去重→入 Redis Streams→2xx。
- `notify::worker`：Redis Streams 消费（`mod_streams`）→ `Email/changes` 增量 → 通知 → 推进 sinceState → XACK。
- `notify::reconcile`：`/reconcile` + `reconcile` 子命令，外部 HTTPS Cron 触发（`FLOW-RECONCILE`）；兼做 Redis 丢失后的游标重建。
- `sinceState` 写外部 Redis（`mod_sincestate`，`C-REDIS-ONLY-STATE`；不用 SQLite/文件 `NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`）。
- 目标验收：向 Stalwart 发测试邮件，经 Push 回调 + worker，Telegram 在 ~秒级收到推送；重启不重发；清空 Redis 后由对账补发。当前代码门禁已通过，但仍需在真实 Stalwart、Redis 和 Telegram 环境完成端到端验证，不能将此处目标当作已验证事实。

### 10.5.1 Redis Streams 事件保障边界（FLOW 运行语义）

Push 事件经 Streams 消费并投递到 Telegram，其关键路径交付语义如下：

**键与 TTL**

| 键 | 作用 | TTL | 说明 |
|---|---|---|---|
| `delivery:inflight:{stream}:{id}` | 处理中租约 | 60s | 崩溃后可被 XAUTOCLAIM 回收重试 |
| `delivery:committed:{stream}:{id}` | 幂等/重放标记 | 7d | 已成功投递的哨兵，防止重放重复打扰 |
| `dedup:jmap:{account}:{email}` | 对账去重 | 24h | 入队时原子抢占，同封邮件 24h 内只通知一次（有意设计，见 11.5） |
| `config:enabled` 等配置 | — | — | 缺键视为关闭（fail-closed），不进业务 |
| `push:subscription:{id}` | Push 订阅的验证码摘要 | 7d | 只存摘要不存明文；`push:disable` 一并清理 |
| `push:orphan:{subscription_id}` | 注册失败后的 JMAP destroy 补偿记录 | 7d | 订阅已创建但回写/销毁失败时记录，供后续清理 |
| `push:registration:{...}` | 注册单飞锁 | 360s | 幂等创建，owner-token 防并发重复注册 |

**投递流程**（`notify::worker`，一次 XREADGROUP 批量 ≤10 条）

1. 读批 → 逐条 `process` → XACK。
2. 处理中写入 `delivery:inflight`（60s）作租约；成功后写 `delivery:committed`（7d）。
3. 崩溃于 `inflight` 租约窗口内的条目，由另一实例经 XAUTOCLAIM（idle 阈值 300s）回收重试 —— 至多重复、不丢。
4. `retry_or_dlq`（`state.rs`）：重试计数（`max_attempts.max(1)`）未到上限留在源流重试；达到上限则以**单个 Lua 脚本**原子地 `INCR`+`XADD`（入 DLQ）+`XACK`（源流确认），保证不会出现"源已 ACK 但既不在源也不在 DLQ"的缝隙。

**告警边界（当前实现，见 §8.3 监控约定）**

- 采用 Uptime Kuma HTTP(s) Monitor，不引入 Prometheus/exporter。
- DLQ 条数、inflight 积压深度、对账补齐条数等指标尚未自动上报（§8.3 标记为可选）。当前运维需通过 Redis 直接查看：`XLEN messageweave:dlq:*`、`XPENDING` 等。
- 若需自动化告警，建议在 Uptime Kuma 增加对 `/ready` 或对账入口的健康检查，并手动复核 DLQ 深度。

**已知边界（当前实现仍存在，见 `docs/todo.md`）**

- Redis 错误映射：`read_batch` 与 `retry_or_dlq` 在 Redis 出错时仍可能返回 `Ok(())`，消费循环因此不会因单次失败而退出；这类故障只能靠 Redis 侧告警发现。
- 多实例重复投递窗口：XAUTOCLAIM 空闲阈值已由固定 300s 改为「批大小 × 单条上限 300s」；单实例不受影响，仅当单条事件处理耗时接近 300s 上限时，多实例部署下另一实例仍可能提前认领，导致**重复投递（仅重复，不丢）**。

**审计意见 → 收口（2026-09-26）**

- 【应修-2】入队失败时 dedup 释放 best-effort 曾可能造成 24h 静默丢事件 → 已修复为 `claim_dedup_and_enqueue`（Lua 原子：`SET NX EX` 成功才 `XADD`），claim 与入队之间无中间失败窗口。
- 【应修-1】`Email/changes` 依赖 `newState` 续传，`jmap-client 0.4.2` 无 `upToId` → 已改为「同 `sinceState` 下逐次翻倍 `maxChanges` 扩窗（上限 4096），仅在无法扩窗时才推进 `new_state`」，避免按页推进时漏批；`newState` 语义本身仍需真实 Stalwart 复验（见 `docs/todo.md`）。
- 其余低风险项均已收口：未知 stream 的空值改为 `Err`（fail-closed，进重试/DLQ）；`push:disable` 经 `forget_push_subscription` 清理验证码摘要键；`SET NX EX` TTL 下限收紧为 `.max(1)`；XAUTOCLAIM 空闲阈值按批大小缩放；无 payload 的畸形流条目由 `ack_malformed` 经 `XACK` 移出 PEL；CSPRNG 兜底 owner-token 改为「时间 + PID + 计数器」，不再使用常量。
- 未排期待办（阶段 5「搜索 + 搜索片段 + 打磨」）见 `docs/todo.md`「未排期」。

### 10.6 总估时
~10.5 人日（不含等待用户确认与真实联调排障）。

---

## 11. 待确认问题（产品/架构类）

> ⚠️ 部署/平台类决策已确认（单账户 / App Password+Basic / Redis 托管+AOF / 平台 HTTPS URL / 外部 Cron 对账），仅余 `Q-DEP-A`/`Q-DEP-B` 运维待办，见 deployment.md §9。

### 11.1 认证方式（已确认）
- **已确认**：**App Password + Basic**（`C-AUTH-APP-BASIC`）。不用主密码、不用 OAuth2 Bearer（无 OIDC 需求）。
- 影响：`Credentials::Basic` 构造；无需 OAuth client / token 自动刷新模块。

### 11.2 实时通道（已定，见 deployment.md）
- 通道 = **JMAP Push HTTPS 回调 + 外部 Cron `/reconcile` 对账**（`C-NO-LONG-CONN`/`C-HTTPS-INBOUND`）；EventSource/SSE/WebSocket 均**非目标**（`NG-POLLING-SSE`）。Push 注册通过受保护的 `POST /api/push/register` 显式触发，外部 Cron 仍是必须的可靠补偿通道。
- **Q2**：部署前确认 Stalwart JMAP capability 和账号权限；官方 user/admin 角色通常已包含 PushSubscription 的 get/create/update/destroy 权限。若 `POST /api/push/register` 返回 `forbidden`，按 Stalwart 版本和角色配置排查。
- **Q3**：Stalwart 对 Push 回调的重试策略/次数？当前实现对 callback 注册和对账采用 owner-token 单飞锁，
  Push 验证码只保存摘要；仍需根据真实部署的重试观测校准 TTL 与对账间隔（见 deployment.md）。

### 11.3 部署形态（已确认，架构相关）
- **已确认**：**单账户实现**（`REQ-SINGLE-ACCOUNT`）；多账户暂用**多个 bot 实例**（各自 token/配置），**不做多账户单实例**（因此无需 chat→account 路由与 `JmapService` 池化）。
- 部署形态 = **webhook-only + 通用 HTTPS-only Docker + 外部 Cron 对账**（deployment.md `C-NO-LONG-CONN`/`NG-LONG-POLLING`/`NFR-RECONCILE-INTERVAL`）。
- **多实例 LB/HA（已确认，`ARCH-LB-WORKER`）**：可选在多个 serverless 平台部署同镜像、共享同一 Redis，前置免费 Cloudflare Worker 做唯一入口与故障转移；Worker 代码位于子目录 [`cloudflare-worker/`](../cloudflare-worker/)（非本 Rust 二进制），部署见 `docs/deployment.md` §10.8。信任模型为**透传**（后端仍 fail-closed 校验，`SAF-LB-PASSTHRU`），后端间**共享同一组 secret**（`C-LB-SHARED-SECRETS`），`/reconcile` **Redis 锁**单实例（`SAF-RECONCILE-LOCK`），Worker 提供**聚合健康视图**（`MOD-HEALTH-AGG`）。**Redis 单点故障不在本方案范围**（`NFR-HA-MULTI-INSTANCE`，用户外部解决）。双活或主备均可。详见 `docs/deployment.md` §10。

### 11.4 邮件正文呈现
- **Q6**：Telegram 消息用 HTML 还是 MarkdownV2？HTML 转义更可控（推荐 HTML），但 MarkdownV2 视觉更好。
- **Q7**：长邮件策略——已确认"禁止直接发送完整原文，超过阈值(默认 4000)发预览 + `[AI 总结][继续查看原文]` 选项"，AI 摘要目标 ~300 字、代码侧不做字符级硬截断（见 §12.3/§12.2）。阈值默认值是否合适见 Q29。
- **Q8**：附件是否在 Telegram 直接下载发送？（Telegram 限 50MB 文件；大附件建议只给下载链接 via `download_url`）。

### 11.5 推送范围与去重
- **Q9**：推送所有新邮件，还是仅特定文件夹/发件人/关键词？（Stalwart 端可用 Sieve 过滤，Bot 端也可二次过滤。）
- **Q10**：是否需要"摘要聚合"（如每早 8 点汇总未读）？由**外部 HTTPS Cron 调度器**触发 `/reconcile` 或新增汇总路由实现（**不在容器内自持定时器**，`C-NO-LONG-CONN`；+0.5d）。

### 11.6 Stalwart 版本/能力
- **Q11**：Stalwart 版本是否启用 Sieve（draft-12）能力？若启用，部分"自动归档/标记"可下沉到服务端 Sieve，Bot 只做通知，减少往返。
- **Q12**：是否需要 `Identity` 多发件身份切换？（若只有一个身份可忽略。）

### 11.7 工程取向
- **Q13**：是否要求 `#![forbid(unsafe_code)]`（与 jmap-client 一致）？建议是。
- **Q15**：运行监控采用 Uptime Kuma HTTP(s) Monitor 检查 `/healthz` 与 `/ready`；不引入 Prometheus、Exporter 或额外指标端口。详细配置见 deployment.md。
  - （CI 平台为通用容器 CI，`NG-SERVERLESS-BIND`；具体见 deployment.md。）

---

## 12. AI 辅助能力：架构、确认门槛、失败回退

> **用户已确认的 7 项需求（本节据此设计，后续所有表述以此为准）**：
> 1. **AI 仅在被明确要求并确认后接触正文**（`REQ-AI-CONFIRM` / `REQ-AI-EXTERNAL-CONSENT`）：只有当用户明确发起"分析/总结/翻译"意图**并再次确认**后，才允许把正文喂给 LLM；**仅当用户明确允许时才向外部 AI 发送邮件正文**（默认不外发）；其余场景（包括查看原文）**一律不经 AI**，由 JMAP 直取。
> 2. **查看原文始终 JMAP 直取**（`REQ-VIEW-DIRECT`）：`/read` 与任何"看正文"动作走 `JmapService::read_email`，不走 LLM。
> 3. **长邮件禁止直接发送完整原文**（`REQ-LONG-EMAIL`）：正文超过阈值（默认 4000 字符，见 §12.3，可调）时，Bot **不直接发送全文**，而是提示"正文较长，请电脑查看"或"选择 AI 总结"；AI 摘要目标约 **300 字**，**不做代码侧硬性数学截断**（由 LLM 自然生成 ~300 字）。
> 4. **分析/摘要结果不持久化**（`REQ-ANALYSIS-EPHEMERAL`）：不写缓存、不入 Redis、不留磁盘；每次请求即取即弃（无状态天然友好）。
> 5. **附件按需拉取（保留）**（`REQ-ATTACH-ONDEMAND`）：不预载；`Blob/get` 仅在用户点击下载时触发，单文件 ≤50MB 发文件，超限给 `download_url` 链接。
> 6. **AI 失败约 3 次后需用户确认的非 AI 回退（保留）**（`REQ-AI-FUSE`）：连续 3 次失败 → 熔断 + 向用户弹确认；选择"关闭 60s" 冷却期内直接走规则模板 + 原文直取路径；选择"恢复 AI" 则做一次探测。
> 7. **OpenAI-compatible 环境变量（保留）**（`REQ-LLM-OPENAI-COMPAT`）：`LLM_API_KEY / LLM_BASE_URL / LLM_MODEL` 等。
>
> 本节仅做设计，不写业务代码。

### 12.1 模块划分（增量，接 §6）
```
src/domain/llm/   # 零渠道依赖（领域层，见 §5.2）
├── mod.rs          # LlmService（门面：summarize / intent / draft；仅确认后调用）
├── client.rs       # 包装 axum + reqwest::Client，OpenAI /chat/completions
├── config.rs       # 从环境装载 + 校验
├── policy.rs       # 熔断/退避/限次（对齐需求 6）
├── fallback.rs     # 非 AI 回退：规则模板 + 原文直取路径（对齐需求 1/2）
└── audit.rs        # tracing span：prompt hash / token / latency / status
```

### 12.2 OpenAI-compatible 环境变量（需求 7）
```bash
export LLM_API_KEY=...
export LLM_BASE_URL=https://api.openai.com/v1        # 可指向任何 OpenAI-compatible 端点
export LLM_MODEL=...                                  # 如 gpt-4o-mini
# 可选
export LLM_TEMPERATURE=0.2
export LLM_MAX_TOKENS=1024
export LLM_TIMEOUT_SECS=30
export LLM_MAX_RETRIES=3                               # 对应需求 6 的"约 3 次"
export LLM_ENABLED=true|false                           # 一键降级开关
export LLM_ALLOW_NET=true|false                         # 默认 false；开启才允许 LLM 出网
# 摘要目标长度（写入 prompt 的系统指令，由 LLM 自然控制；代码侧不做字符截断）
export LLM_SUMMARY_TARGET_CHARS=300
```
- 客户端：**axum**（或裸 `reqwest` + `serde`），`POST {LLM_BASE_URL}/chat/completions`，`Authorization: Bearer $LLM_API_KEY`。
- **仅在用户明确要求分析/总结/翻译并确认后**才调用 LLM；此时把**最小必需上下文**（Subject + 正文 + 用户指令）交给 LLM，prompt 系统指令写明"请生成约 300 字中文摘要"。
- 摘要结果**不持久化**（需求 4）：不写 Redis、不落盘，处理完即弃；无状态天然友好。
- 默认关闭网出能力（`LLM_ALLOW_NET=false`）降低数据外泄风险；密钥不入仓库；`config::Config` 启动时强校验，缺失且 `LLM_ENABLED=true` → 启动失败或降级为禁用并告警。
- 运行期 secret 注入方式见 deployment.md（`C-NO-SECRET-IN-IMAGE`）。

### 12.3 正文获取策略与长邮件处理（需求 1/2/3/5）
- **查看原文 = JMAP 直取**：`/read` 与任何"看正文"动作走 `JmapService::read_email`，**不经 LLM**；LLM 不在查看路径上。
- **AI 接触正文的前置门槛**：只有当用户发起明确的"分析/总结/翻译"意图**并再次确认**（例如点 `[AI 总结]` 按钮 / 执行 `/summarize`）后，才把正文作为 LLM 输入。未经确认 → LLM 看不到正文。
- **长邮件禁止直接发送完整原文**（需求 3）：
  - 当正文长度 > 阈值（默认 **4000 字符**，由 §12.2 配置，可调），Bot **不发送全文**，而是回复一段简短预览（如 Preview 120 字符 + 发件人/主题/附件概览）并给出选项：
    > "邮件正文较长（约 N 字）。在手机上不便完整阅读，建议在电脑端查看。如需在此查看：`[AI 总结]`（约 300 字）`[继续查看原文]`（截断）"
  - "AI 总结"路径：经用户确认 → 把正文交 LLM → 生成约 300 字摘要（目标由 prompt 控制，**代码侧不做字符级硬截断**）。
  - "继续查看原文"路径：仍 JMAP 直取，发送截断版（明确标注"已截断，完整请电脑查看"），**绝不直接发送完整原文**。
- **附件按需**（需求 5 保留）：Telegram 端先给"📎 3 个附件（PDF, 2.1MB）"概览 + 下载按钮；用户点击才触发 `Blob/get` 流式下载 → `Telegram::send_document`，单文件 ≤50MB，超限只给 `download_url` 链接。避免启动期预载全部 blob，减少 JMAP 出站与内存峰值。
- AI 摘要**永远不**把附件内容喂入 prompt（除非后续单独功能开启且白名单限定），保持隐私默认。

### 12.3.1 AI 授权期限与 Redis TTL（`REQ-AI-CONSENT`）
- AI 授权必须由用户明确选择期限；可选 `临时一次`（一次授权窗口，3600 秒）、今天（86400 秒）、7 天（604800 秒）或直到撤销（最长 365 天，即 31536000 秒）。触发词按原文包含匹配（`临时`/`一次`/`今天`/`7天`/`直到我撤销`/`长期`，以及 `/ai on`、`/ai yes`、`/ai off`），不支持英文别名。
- Redis 的 consent key 仅保存 chat id 对应的到期 Unix 时间，并使用**与所选期限完全相同**的 `SET EX` 秒数；不使用隐式 30 天默认值，也不保存正文或摘要。
- 到期由 Redis TTL 和读取时的到期校验共同保证，摘要请求回到元数据模式并提示“授权已到期”；用户重新选择期限后才可再次授权。`/ai off` 立即删除 key。

### 12.4 失败检测与熔断（需求 6）
```
LlmService.call(kind: LlmKind, input, ctx)
  ├─ 调用 axum → Result<Response, LlmErr>
  │   成功 → 正常路径
  │   失败（4xx/5xx/超时/网络/401） → 记入 policy
  └─ policy.tick() 计数：
       失败次数 >= 3（LLM_MAX_RETRIES） 且 在 TTL 窗口内
       → 触发"熔断 + 需确认回退"（进入半开状态）
```
- 触发后向用户发一条**显式确认消息**（`/llm-fallback` 风格的 inline-button）：
  > "AI 最近 3 次调用失败，我切换到规则模板回复。是否继续？ `[恢复AI(重试)]` `[关闭AI 60s]`"
- 用户选择后：
  - "恢复 AI" → 走一次探测请求，成功则重置计数、退出熔断；失败则维持熔断；
  - "关闭 60s" → 进入冷却期，期间所有 LLM 调用直接走 fallback 路径。
- 熔断状态**持久化**到 Redis（TTL = 冷却期），无状态化：实例重启/迁移后能从 Redis 恢复；Redis 丢失时退化到进程内短窗口（接受 60s 内的漏判）。Redis 运维与存活窗口见 deployment.md。

### 12.5 非 AI 回退（需求 6，与"原文直取"一致）
- `fallback.rs` 提供与 AI 接口相同的签名（`fn summarize(input:&Input) -> Output`），内部走**规则 + 原文直取**：
  - 摘要回退：截取 `Subject + Preview(120 chars)`，不解析 HTML；
  - 意图识别回退：用关键字/正则匹配（如"归档"、"删除"、"回复"）走确定性分支；
  - 草稿/格式化回退：纯模板字符串替换。
- 所有回退响应均**显式标注**"AI 不可用"徽标，避免用户误以为是 AI 生成。
- 回退路径**不经过 LLM 客户端**，保证零 LLM 依赖；且**不触发 3 次熔断逻辑**（熔断只统计 LLM 调用）。

### 12.6 无状态/容器化适配
- **分析/摘要结果不持久化**（需求 4）：不写 Redis、不落盘、不做结果缓存；每次请求即取即弃。**删除"摘要结果入 Redis 缓存"方案**——多实例/冷启动只影响重复调用成本，不缓存保证数据最少留存。（与 `C-REDIS-ONLY-STATE` / `SAF-LOG-PURITY` 一致：Redis 上 LLM 相关状态仅熔断计数；**不写** prompt/completion 明文。）
- **熔断计数存 Redis**（`INCR + EXPIRE`），跨实例共享（这是 Redis 上唯一与 LLM 相关的状态）。
- `LLM_SUMMARY_TARGET_CHARS`、`LLM_MAX_RETRIES`、熔断冷却时间通过 config 下发，不写死。

### 12.7 错误处理与可观测性（接 §8）
新增 `BotError::Llm(LlmErr)`；`LlmErr` 细分 `Unauthorized`（401/403）、`RateLimited`（429）、`Timeout`、`BadResponse`、`Transport`。用户消息映射：
- 熔断中 + 未确认 → 静默走回退，附"AI 不可用"徽标；
- 恢复失败 → 再次提示用户确认；
- 审计：`tracing` span `llm.call` 记录 prompt 哈希、response 长度、latency、status，不记录 prompt 明文（合规）。

### 12.8 新增待确认问题
- **Q25**：LLM 提供方是 OpenAI 官方还是自托管/Ollama/vLLM？`LLM_BASE_URL` 是否需要 TLS 互信？出网白名单是否允许？
- **Q26**：`LLM_ALLOW_NET` 默认 false 是否可接受？（即 AI 调用本身也视为敏感出网操作，需用户显式开关。）
- **Q27**：熔断冷却期（60s 还是 5min）与"约 3 次"阈值需确认；"约 3 次"是否含 429 限流？
- **Q28**：附件"按需"指**下载并作为 Telegram 文件回传**，还是**仅给 Stalwart `download_url` 链接**？后者更省流量与成本，前者体验更顺。
- **Q29**：长邮件"禁止直接发送完整原文"的阈值（默认 4000 字符）是否合适？"AI 总结"目标 ~300 字是否满足预期（过短/过长可调整，由 prompt 自然控制）？
- **Q30**：`LLM_ENABLED=false` 时是否**启动即禁用 AI 且不写熔断**（最简降级），还是仍保留探测？推荐前者。

### 12.9 实施计划增量（接 §10）
在阶段 3 与 4 之间插入 **阶段 3.5：LLM 门面 + 回退（1.5d）**
- `llm::client` + `axum`/reqwest 调用；
- `llm::policy` 3 次熔断 + Redis 共享计数（熔断是 Redis 上唯一的 LLM 相关状态；**摘要结果不持久化**）；
- `llm::fallback` 规则模板 + 原文直取；
- 长邮件策略：正文 > 4000 字符 → 不发送完整原文，改发预览 + `[AI 总结][继续查看原文]` 选项（对齐 §12.3）；
- `telegram/formatting` 增加"AI 不可用"徽标；
- 验收：
  - `LLM_ENABLED=true` 且用户明确确认分析 → 正常 ~300 字摘要（不做字符级硬截断）；
  - 长邮件默认**不发送完整原文**（仅预览 + 选项）；
  - 模拟 3 次 500 → 弹确认；选"关闭 60s" → 60s 内全走回退并带徽标；
  - `LLM_ENABLED=false` → 直接全走回退；
  - 查看原文（`/read`）路径**绝不**触发 LLM。

---

## 附：调研依据（可复核）
- `stalwartlabs/jmap-client` main 分支：`src/lib.rs`（URI/Method/DataType/Error）、`src/client.rs`（认证/连接/event_source）、`src/email/`、`src/email_submission/helpers.rs`、`src/event_source/`、`src/push_subscription/`（create/verify/update_types/destroy）、`src/core/error.rs`、`Cargo.toml`、`README.md`、`examples/`。
- crates.io：`teloxide` 0.17（features 列表）、`jmap-client` 元数据。
- `stalwartlabs/mail-server` main 分支：`crates/common/src/auth/credential.rs`（Password/AppPassword/ApiKey）、`crates/http/src/auth/authenticate.rs`（AccessScope 权限裁剪）、`crates/jmap/src/push/`、`api/v1/openapi.yml`（`securitySchemes`: basicAuth/bearerAuth/liveToken 60s）。
- 项目目录 `/home/okabe/Repo/messageweave/`（工具链要求见 §10.0 与 AGENTS.md）。
- 部署/平台相关调研依据（lambda_runtime/worker/aws-sdk 等）见 deployment.md。
