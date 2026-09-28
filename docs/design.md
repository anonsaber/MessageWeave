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
4. [Telegram 渠道实现选型](#4-telegram-渠道实现选型)
5. [整体架构与数据流](#5-整体架构与数据流)
6. [模块划分](#6-模块划分)
7. [配置与安全](#7-配置与安全)
8. [错误处理与可观测性](#8-错误处理与可观测性)
9. [测试策略](#9-测试策略)
10. [分阶段实施计划](#10-分阶段实施计划)
11. [历史问题与决策归档（产品/架构类，均已有结论）](#11-历史问题与决策归档产品架构类均已有结论)
12. [AI 辅助能力：架构、确认门槛、失败回退](#12-ai-辅助能力架构确认门槛失败回退)

> 部署/平台类决策已确认（单账户、App Password+Basic、Redis 托管+AOF、平台 HTTPS URL、外部 Cron）并**全部收敛归档**（含 `Q-DEP-A`/`Q-DEP-B`，见 `docs/deployment.md` 的已确认决策一节）；**未完成的代码缺口见 `docs/roadmap.md`**。

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
| 搜索 | `email_query`（`Filter::text`）+ `SearchSnippet/get` 高亮 | `Filter` 是 serde 单标签枚举，**无 comparator 语法**；`SearchSnippet/get` 只返回 `emailId`/`subject`/`preview`，**无 `bodyProperties`/`parts`**，正文级高亮在锁定版本 `0.4.2` 做不到（降级为纯 ID 列表） |
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
| `search_emails(account_id, query, limit)` | → `Result<Vec<SearchResult>, JmapError>` | `email_query`（`Filter::text`，`limit` 封顶 100）取 ID + `SearchSnippet/get` 取高亮；后者不支持（`unknownMethod`/超时）时**降级**返回空 snippets 而非报错，由调用方渲染纯 ID 列表 |

> 设计要点：`email_query` 的 `anchor`+`position` 分页是 JMAP 标准做法，比传统 offset 更稳；`sinceState` + `changes` 用于增量同步，避免重复拉全量。

---

## 4. Telegram 渠道实现选型

Telegram 渠道用 `src/channel.rs` 的 `reqwest` 自研实现（`ARCH-DEPS-STAGE4`），**不引入第三方 Bot 框架**。
评估过 `teloxide` 但**未采用**：本项目只用 Bot API 的少数调用，不值得为它引入 dptree 调度与自带会话中间件这两层抽象；`throttle` 与 Redis 会话能力用 `src/channel.rs` 的重试与外部 Redis TTL 各实现了一部分。
候选对比（`teloxide` / `grammers` / 旧 `telegram-bot`）、当初倾向 `teloxide` 的 6 条理由、feature 集计划与 `teloxide-core` 降级方案，全部记录在 `docs/retired.md`。

---

## 5. 整体架构与数据流

### 5.1 高层拓扑
```
            ┌───────────────────────────────────────────────────┐
            │                    Telegram                       │
            └───────────────────────┬───────────────────────────┘
                     Webhook(HTTPS) │
            ┌───────────────────────▼───────────────────────────┐
            │   HTTP 入口 (axum，单端口 PORT)   │
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
> 渠道解耦（见 §5.2）：领域层与渠道层以领域 Command / Notification 交互，领域层不出现任何具体渠道 SDK 类型；钉钉/飞书仅保留扩展位，不提前实现。

### 5.2 渠道抽象与多通道扩展策略
- **现状**：首个（也是当前唯一）渠道是 Telegram。目标是保留平行扩展钉钉/飞书的能力，**但不提前实现**。
- **原则**：领域层与渠道层解耦——邮件/JMAP/AI/意图状态机**不得依赖 Telegram 类型**。
- **抽象（不过度设计）**：

  | 抽象 | 职责 | 当前状态 |
  |---|---|---|
  | `Channel` | 渠道生命周期：接收输入、分发命令、配置端点 | **无实现**（阶段0占位）；命令解析实际在 `parse_intent`（`src/worker.rs`） |
  | `Notifier` | 主动推送：把领域 `Notification` 发送到用户 | **无实现**（阶段0占位）；出站实际走 `TelegramClient::send`（`src/channel.rs`） |
  | `MessageAdapter` | 领域数据 ↔ 渠道消息渲染（文本/按钮/转义） | **无实现**（阶段0占位）；渲染逻辑散在 `src/worker.rs` / `src/notify.rs` |

  - `src/channel.rs` 里三个 trait（`Channel` / `Notifier` / `MessageAdapter`）目前**没有任何实现**，都标着 `#[expect(dead_code)]`（注释：阶段0占位，供后续 adapter 使用）。实际 Telegram 出站走 `channel::telegram::TelegramClient`（`reqwest` 自研），由 `worker.rs` 的 `MetadataWorker` 和 `notify.rs` 持有，**不经过这三个 trait**。
  - 领域与渠道之间用**领域 `UserCommand` / `Notification`** 数据结构传递，渠道只在边缘做适配（解析→领域命令；领域 `Notification`→渲染）。
- **约束**：
  - `jmap` / `llm` / 意图状态机 / `notify::core` 的公开接口只接受/返回领域类型；
  - 领域层不得出现具体渠道 SDK 类型（渠道层当前不引入第三方 Bot 框架，见 §4）；
  - 新增渠道 = 新 adapter + 装配，不改领域层。
- **不做什么**：不定义多态配置注册表、不预先抽象"渠道能力矩阵"、不建 plugins 机制；按需再演进（YAGNI）。

### 5.3 运行模型（短请求，无长连接）
- 单一二进制 `message-weave`，`#[tokio::main]`。
- 启动时：
  1. 加载配置（`config::Config`）。
  2. 构造 `JmapService`（`Client::connect` 完成 session 解析、account_id 缓存、mailbox role→id 映射预热）；sinceState 从外部 Redis 恢复（`MOD-SINCESTATE`）。
  3. 启动 **HTTP 入口**（axum，单端口 `PORT`）：`/webhook/tg`、`/push/jmap`、`/reconcile`、`/healthz`、`/ready`；其中三条写路径先经 `SAF-AUTH-*` 入口鉴权（fail-closed，§7.3），`/healthz`、`/ready` 为公开探针（`SAF-PROBE-PUBLIC`）；`/ready` 已做端到端探测（配置 + Redis + 出站只读探测 JMAP session `GET` 与 TG `getMe`，各 3s、并行，最坏约 3s）。
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

当前**没有 FSM**。命令路由由 `src/worker.rs` 的 `parse_intent` 解析为 `Intent`；AI 授权态是 Redis 里的一个布尔加过期时间（键与 TTL 见 `docs/reference.md` 的 AI 授权态一节），不是多步确认态。

仍成立的不变量：AI 摘要必须先有用户显式授权，**没有授权态不得调用 LLM**。授权一旦生效，摘要会拉取邮件正文全文送 LLM（`read_email` 请求 `TextBody` + `BodyValues` 并 `fetch_text_body_values(true)`，见 `src/domain/jmap/client.rs`），但正文**不回传给 Telegram**——出站只发摘要，或失败时回退为前 300 字符（`worker.rs:150` 注释：body text never reaches Telegram）。

曾设计过的 5 态 FSM（`Idle` / `AwaitClarify` / `AwaitConfirm` / `Analyzing` / `AwaitFallback`）连同状态转移表与渠道中立说明，见 `docs/retired.md`。

### 5.7 邮件搜索（`/search`，`bfe0fd8` 落地）

`/search <关键词>` 与中文前缀 `搜索`/`查找`/`检索`（**仅前缀匹配**，避免"帮我搜一下…"被劫持成搜索）触发 `Intent::Search`，经 `JmapService::search_emails(account_id, query, limit)` 走 `email_query`(`Filter::text`) 取 ID + `SearchSnippet/get` 取 `subject`/`preview` 高亮。

关键边界：
- **高亮降级**：`SearchSnippet/get` 不支持（`unknownMethod`）或超时时返回空 snippets 而非报错；渲染降级为纯 ID 列表（`高亮片段暂不可用，以下为匹配的邮件 ID`），不发明片段。
- **纯文本出站**：`<mark>` 高亮标记经 `strip_mark_tags`（大小写不敏感、未闭合标记丢弃）剥离后，再 `unescape_html_entities` 单趟解码；先剥标签再解码实体，防止邮箱正文里字面 `<mark>`（经服务器转义为 `&lt;mark&gt;`）被还原成真标签而误删。`subject`/`preview` 分别有 120/160 字符截断上限（`truncate_chars`，与正文截断同工具）。
- **失败映射**：JMAP 侧失败 → `SearchReply::Retry` → `process_telegram` 返回 `Err(())` → 协调器 503 + `Retry-After` 重试（与 `/reconcile`、其他 JMAP 路径一致），**不 panic、不向用户回错误栈**。
- **空查询**：`/search`（无关键词）→ 引导提示"请提供搜索关键词，例如：/search 发票"，不发请求。无匹配 → `没有找到匹配「…」的邮件。`。
- **每条命中带 `email_id`**，可直接接 `/summary <email_id>` 进入 AI 摘要流程。
- **正文级高亮在锁定版本做不到**：jmap-client `0.4.2` 的 `SearchSnippet` 只建模 `emailId`/`subject`/`preview`，无 `bodyProperties`/`parts`，按 RFC 8621 §5 的正文级 `body: String[Id]` 不被该 crate 建模且（无 `deny_unknown_fields`）被 serde 静默丢弃。每部分正文高亮需绕过 crate 直发原始 JMAP，当前不实现。
- **无新环境变量**；缺配置时 `/search` 给友好报错。

---

## 6. 模块划分

实际工程结构（本文件所有代码锚点均指此结构；历史的目标模块拆分计划见 `docs/retired.md`）：
```
message-weave/
├── Cargo.toml                    # jmap-client 0.4.2 / redis 0.27 / reqwest 0.13；不含 teloxide
├── .env.example                  # 仅 2 个必填项的占位样例（REDIS_URL / CONFIG_ENCRYPTION_KEY），不含业务配置
├── .gitignore
├── Dockerfile                    # debian:bookworm-slim + ca-certificates + tini（C-DEBIAN-SLIM）
├── hoststack.yaml                # 网关编排清单，非应用代码
├── src/
│   ├── main.rs                   # tokio main：校验 RUN_MODE 后启动 webhook HTTP 入口（两模式共用同一入口，无 CLI 子命令）
│   ├── config.rs                 # 环境变量解析（ARCH-CONFIG-ENV：std::env，非 figment/TOML）
│   ├── error.rs                  # BotError 统一错误（见 §8.1）
│   ├── domain.rs                 # 领域层根（渠道中立，见 §5.2）
│   ├── domain/jmap.rs            # JmapBackend trait + MockBackend（契约测试不联网）
│   ├── domain/jmap/client.rs     # 包装 jmap_client::Client（真实只读 adapter，MOD-JMAP-CLIENT）
│   ├── state.rs                  # Redis 读写封装：配置/开关/会话/TTL 键（C-REDIS-ONLY-STATE）
│   ├── channel.rs                # 渠道层：Channel/Notifier/MessageAdapter，Telegram 用 reqwest 自研
│   ├── notify.rs                 # /push/jmap 校验入队 + Redis Streams worker + /reconcile 对账
│   ├── worker.rs                 # parse_intent → Intent，命令路由与 AI 授权判定
│   ├── ai.rs                     # LlmClient / summarize：LLM 摘要调用
│   └── web.rs                    # /config 静态前端路由 + include_str! 嵌入 + CSP
├── web/                          # 前端资源，构建期 include_str! 嵌入二进制（C-NO-LOCAL-WRITE）
│   ├── index.html
│   ├── styles.css
│   ├── config.js
│   └── config.test.mjs           # Node 原生断言测试（非 cargo 测试）
└── cloudflare-worker/            # 边缘反向代理，JS，不在 Rust 工程中
```

测试：**没有 `tests/` 集成测试目录**，也没有 `src/util/`。所有 Rust 测试是各模块内的 `#[cfg(test)]` 单元测试；前端唯一测试是 `web/config.test.mjs`。

### 6.1 模块职责矩阵

> **依赖现况**：`jmap-client 0.4.2`、`redis 0.27`、`reqwest 0.13` **均已引入**（`ARCH-DEPS-STAGE1` / `ARCH-DEPS-STAGE4`，版本以 `Cargo.toml` 为准）。`teloxide` 经评估**未引入**，Telegram 渠道用 `reqwest` 自研实现（§4，详见 `docs/retired.md`）。下表每一行都是**真实存在的模块**。

| 模块 | 依赖 | 输出 | 可测性 | 现状 |
|---|---|---|---|---|
| `config` | 标准库 `env`（`ARCH-CONFIG-ENV`） | `Config` 结构 | 纯函数，易测 | 已实现（手工 `from_env`，无 figment） |
| `error` | — | `BotError`（§8.1，4 个变体） | 单测 | 已实现 |
| `domain` + `domain::jmap::client` | **jmap-client 0.4.2** | JMAP 只读语义；`client` = 真实只读 adapter（`MOD-JMAP-CLIENT`） | mock JMAP 响应 + `#[ignore]` 真机测试 | 代码已实现，待真实 Stalwart 端到端验证（`cargo test -- --ignored jmap::`） |
| `state` | redis 0.27 | Redis 读写：配置/开关/会话/TTL 键（`C-REDIS-ONLY-STATE`） | Redis mock | 已实现 |
| `channel` | reqwest 0.13；jmap-client | `Channel` / `Notifier` / `MessageAdapter`，邮件与 TG 双渠道（事件→领域 Command；领域 Notification→渲染） | mock HTTP | 已实现（`src/channel.rs`，Telegram 为 `reqwest` 自研） |
| `worker` | — | `parse_intent` → `Intent`（6 种，含 `Search(query)`：`/search` + `SearchSnippet/get` 高亮渲染），命令路由与 AI 授权判定 | 表驱动纯单测 | 已实现 |
| `ai` | reqwest 0.13 | `LlmClient` / `summarize` | mock OpenAI 兼容端点 | 已实现 |
| `notify` | axum；redis | HTTP 鉴权、全局开关、`/push/jmap` 入队、`Email/changes` 对账和游标提交 | webhook 校验失败路径覆盖 403 | 已覆盖真实对账路径；仍需真实 Stalwart 环境做端到端验收 |
| `web` | axum | `/config` 静态页 + `include_str!` 嵌入 + CSP | 前端由 `web/config.test.mjs` 覆盖 | 已实现 |

> 注：「现状」列是**模块级**口径（模块已落地），不代表行为完备。行为级缺口不在本表内：`docs/roadmap.md`「代码缺口」中 `/search`、Telegram 429 退避、多实例重复投递窗口均已实现，仅剩 1 条按产品决策保留不改（`SAF-DEBUG-ALLOWLIST`）。`worker` 模块的 `/search` 路径已随 `bfe0fd8` 落地。

---

## 7. 配置与安全

### 7.1 历史配置读取（已迁移至 Redis）

> **迁移目标（`C-REDIS-ONLY-STATE`）**：生产启动环境仅保留 `REDIS_URL`。空 Redis
> 的首次配置必须通过 `CONFIG_ENCRYPTION_KEY` 认证的一次性 bootstrap/admin 会话完成；
> 不得提供未鉴权写入口。该密钥仅用于常数时间比较，不回显、不记录、不写入业务配置；
> Redis ACL 密码只承担 Redis 连接本身，不再是任何 HTTP 认证凭据。bootstrap 成功后
> 管理员会话哈希及 TTL 保存在 Redis，重启可恢复；业务 token 从 Redis 在启动时装载，
> 密钥 GET 永不回显；bootstrap/管理员 PUT 成功后先构建并原子替换客户端，后续请求即时
> 使用新配置，失败保留旧实例。该迁移替代下述阶段0环境变量清单，阶段0列表仅作为历史
> 兼容说明。

> **实际实现**（`ARCH-CONFIG-ENV`）：阶段0 起配置**只从环境变量读取**，由 `config::Config::from_env()` 手工解析（`std::env::var`），**不使用 figment、不使用 TOML 配置文件**。缺失必填项即启动失败；布尔值接受 `true/1/yes` 与 `false/0/no`。

> ⚠️ **下表是 `Config::from_env()` 遗留/引导路径的完整变量表**（阶段0 口径，保留作兼容参考）。**当前生产部署只需要 `REDIS_URL` + `CONFIG_ENCRYPTION_KEY` 两个启动变量**（`src/main.rs`）；其余业务字段已迁移到 Redis 业务配置（`PUT /api/business-config` 热加载，`C-REDIS-ONLY-STATE`）。表中的「必填」仅指**走环境变量引导路径时**必填，不代表当前生产必须配置。

| 变量 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `PORT` | 否 | `8080` | 单监听端口（`C-NO-TCP-EXPOSE`） |
| `RUN_MODE` | 否 | `webhook` | `webhook` / `reconcile`（`NG-SERVER-MODE` 已删除）；**两种取值当前不改变任何运行时行为**（两模式共享同一套路由表），仅作前向占位 |
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
  - `/ready` 做端到端探测：配置完整性 + Redis 可达性 + 出站只读探测（`GET {jmap_origin}/.well-known/jmap`，带配置的 Basic 认证；`GET https://api.telegram.org/bot<token>/getMe`；各 `PROBE_TIMEOUT` = 3000ms、**并行**（`tokio::join!`），最坏约 3s）；四者全过 `200` 与就绪报告（`{"status":"ready","configured":...,"jmap":...,"telegram":...}`，其中 `jmap`/`telegram` 是真实探针结果），任一失败 `503`（标准错误 envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`）。探针只读、只读配置状态，**不**触发邮件同步等业务副作用，也**不**回显 token 或第三方响应内容；`refresh_business_config` 仅读 Redis，无状态写入。因此 `/ready` 要求到 JMAP host 与 `api.telegram.org:443` 的出站 egress 可达（若该 egress 需要代理则 `/ready` 不可用，见 deployment.md）。
- **chat 白名单（硬约束 `SAF-CHAT-ALLOWLIST`）**：`CHAT_ALLOWLIST` 是**必填**配置；任何入站事件（TG 命令 / 回调触发的动作）在**做任何 JMAP 调用、AI 调用或状态变更之前**，必须先校验 `chat.id ∈ CHAT_ALLOWLIST`，不在白名单则**直接拒绝并终止**（防止 token 泄露后被任意人调用）。阶段0 已完成 `CHAT_ALLOWLIST` 解析骨架；强制拒绝逻辑已随 Telegram 渠道接入落地（`src/notify.rs` 的 `telegram_webhook` 在任何 JMAP/AI/状态操作之前先校验白名单，拒绝即终止）。
- **命令最小化**：只暴露必要命令；发邮件等写操作必须二次确认（当前未实现发信，见 §10.3）。
- **速率**：出站侧未建本地令牌桶；Telegram 出站发送按 Redis 运行参数 `max_retries`（默认 3、上限 5）重试；当前仅对 Push 验证码写入做 Redis 限流（`ratelimit:push-verify:*`）。Telegram 服务端 30 msg/s 限制下的 429 **按 `parameters.retry_after` 秒自动退避**（`channel.rs`，`f4cae00`）：`retry_after_ms` 解析后截断到 60s 预算上限，缺该字段或非数字时回退指数退避 `backoff_delay_ms`（250ms 起、封顶 4s），整体重试预算 60s。仍不做本地令牌桶限流——超出预算直接返回失败，交由上游重试。

### 7.3.1 配置管理 API
- `GET /` 提供嵌入 Rust 二进制的 SPA；`/assets/config.js` 与 `/assets/styles.css` 提供页面资源。服务不在运行时读取或写入本地文件（`C-NO-LOCAL-WRITE`）。
- `GET /api/status` 公开返回 `{ "ready": boolean, "mode": "configured" | "configuration-setup", "missing": string[] }`，只列缺少的环境变量名称。缺少 `REDIS_URL` 或 `CONFIG_ENCRYPTION_KEY` 时，SPA 只显示配置引导状态与缺失变量；服务状态确认 ready=true 后才显示管理会话授权区。
- `POST /api/admin/session` 接受 `Authorization: Bearer <CONFIG_ENCRYPTION_KEY>`，返回 `{ "session": "<opaque>", "expires_in": 900 }`；admin session 仅存 Redis 中的摘要并在 900 秒后过期。`POST /api/admin/session/revoke` 撤销当前 session，成功返回 `204`。
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

### 7.6 远程联调面为何默认绝对关闭

该面存在的唯一理由是缩短生产排障路径：在无法登录容器、只能靠外部请求观察系统时，需要有人能在不发版的前提下探到 JMAP/Telegram 连通性与当前业务配置。代价是它的入口软度必然高于三条写路径——除只读探测外还保留一次**真实出站发送**，且复用业务白名单而非独立白名单。因此设计选择是「默认关闭」而不是「默认开启、靠网关挡」：进程同时带 `--debug` 且设置了非空 `DEBUG_TOKEN` 时才会挂载这组路由（`SAF-DEBUG-GATE`）；缺一时路由在路由器里**根本不存在**，请求落到 axum 通用 `404`，而非「存在但 401」——后者会泄漏路由存在性。也不存在第三种状态：没有「未配置即开放」的回退，也没有任何配置项能把它设为默认开启。再叠一层位置约束：它不在网关的安全路由白名单内，即便后端开错，经平台入口也会被 fail-closed 拒掉，唯一可达路径是直连后端 origin。这四处——双因子挂载、404 而非 401、无默认开启回退、网关不可达——共同构成「暴露面默认为零」的架构决策。启用方式、逐端点状态码与部署确认清单见 deployment.md §2.1。

---

## 8. 错误处理与可观测性

### 8.1 统一错误枚举

当前实现（`src/error.rs` 全文，4 个变体）：

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BotError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state error")]
    State(#[from] crate::state::StateError),
}
```

设计目标形态曾包含 `Jmap(#[from] jmap_client::Error)`、`Telegram(#[from] teloxide::errors::RequestError)`、`Storage(#[from] redis::RedisError)`、`RateLimited`、`Unauthorized(i64)`、`Llm(LlmErr)` 六个额外变体；`Telegram` 随 `teloxide` 未采用而废弃，其余至今未落地，完整清单见 `docs/retired.md`。

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
- `tracing`（直接依赖）+ `tracing-subscriber`（fmt + EnvFilter）。
- `tracing`（直接依赖）+ `tracing-subscriber`（fmt + EnvFilter）。**当前只记录启动期事件**：`src/main.rs` 共 7 处 `info!`/`warn!`（`RUN_MODE` 取值横幅×2、启动变量缺失降级到 setup 模式、启动横幅、debug 端点开启、JMAP 服务不可用降级×2）。请求级事件（Push 回调、Reconcile 拉取、渠道推送、LLM 耗时）**尚未实现**——`src/` 里除 `main.rs` 外没有任何 tracing 调用，也没有命名 span（无 `#[instrument]` / `span!`）。
- 指标（可选 `metrics` crate）：Push 回调到达数、去重命中率、Redis Streams 积压深度（pending）、DLQ 条数、对账补差条数、JMAP 请求延迟、推送失败率。**当前未接入任何指标后端**（`metrics` 不在 `Cargo.toml`）。
- 优雅退出：**当前未实现**——`src/` 里没有信号处理（无 `tokio::signal` / `ctrl_c`），也没有 Streams pending 条目的 flush 逻辑；容器停机即终止进程。Redis 侧 `C-NO-STATEFUL-RECOVERY` 保证重启后从 Redis 重建，不依赖进程内状态。
- **可靠性目标与策略**（Streams ACK/retry、幂等去重、Push 重试、对账恢复、指标/告警、**≥99.9% 通知可用性及边界**）见 deployment.md §6.4/§6.5（`NFR-NOTIFY-SLA`）。

---

## 9. 测试策略

### 9.1 层次

| 层 | 位置 | 内容 |
|---|---|---|
| 单元 | `src/*.rs` 内 `#[cfg(test)] mod tests` | `config` 解析、`worker` 命令路由、`ai` 的 HTTPS 校验、`state` Redis 封装、`channel` 出站、`notify` 校验与去重、`main` 启动分支 |
| 契约 | `src/domain/jmap.rs` | `JmapBackend` trait + `MockBackend`：用预置数据验证领域动词，不联网 |
| 集成 | `src/channel.rs` 测试模块 | 用进程内 `tokio::net::TcpListener` 起 mock HTTP 服务，验证出站请求的路径、状态码与重试 |
| 真实（手动） | `src/domain/jmap/client.rs` | 标 `#[ignore]`，需显式配置 JMAP 测试服务器后 `cargo test -- --ignored` 才跑 |

**没有 `tests/` 集成测试目录**；前端唯一测试是 `web/config.test.mjs`（Node 原生断言）。

### 9.2 mock 策略
- `JmapService` 持有 trait `JmapBackend`（`list_emails` / `read_email` / `send_email` / `changes` 等），生产实现包装 `jmap_client::Client`（`src/domain/jmap/client.rs`），测试用 `MockBackend`（`src/domain/jmap.rs`）。
- Telegram 出站不引入任何框架 mock：`src/channel.rs` 起一个进程内 `TcpListener`，用 `AtomicUsize` 记录请求次数，验证 URL、状态码与超时/重试行为。
- `src/ai.rs` 通过构造 `LlmClient` 的配置断言 HTTPS 强制（`llm_requires_https`）；摘要逻辑在 mock OpenAI 兼容端点上验证。
- 渠道与领域解耦靠类型约束（`domain.rs` 的公开接口不出现渠道 SDK 类型），不依赖 mock channel。

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
  - **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz` 返回 `200` 表示进程存活；`/ready` 以 `200/503` 表示就绪——检查配置完整性 + Redis 可达性 + 出站只读探测（`GET {jmap_origin}/.well-known/jmap` 带 Basic 认证、`GET https://api.telegram.org/bot<token>/getMe`，各 3s、并行，最坏约 3s），任一失败返回标准错误 envelope（`service_unavailable` + `Retry-After: 30`），全过返回就绪报告 JSON；响应体不含敏感信息。Uptime Kuma 按状态码（`/ready` 期望 200）监控，不受响应体变化影响。
  - **渠道解耦**：领域模块（`src/domain.rs` / `src/ai.rs` / `src/worker.rs`）的公开接口不出现任何渠道 SDK 类型；渠道装配集中在 `src/channel.rs`。
  - **JMAP session URL（`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`）**：基地址与完整 `…/.well-known/jmap` 两种输入**均接受且归一化结果一致**，传给 `Client::connect` 的 URL **不含重复 `/.well-known/jmap`**；`http://` 被拒绝；**内嵌凭据（`https://user:pass@host`）被拒绝**；危险 query 被拒绝。
  - **JMAP 多 part 原文（`REQ-JMAP-RAW-MULTIPART`）**：`read_email` 对多 part 正文按 `text_body` 顺序拼接"有 `part_id` 且有 `bodyValue`"的部分；构造"无可用部分"用例断言返回**明确错误**（非空串）。

---

## 10. 分阶段实施计划

> 前提：先 `rustup` 装工具链（stable）。CI 与发布流见 deployment.md。

### 10.0 阶段 0：脚手架与 HTTPS 入口骨架（已完成，`GATE-P0` 已过）

> **当前实际边界**：单端口 axum 入口提供 `/webhook/tg`、`/push/jmap`、`/reconcile`、`/healthz`、`/ready`。三条写路径的入口鉴权已 fail-closed 落地（`R1`/`SAF-AUTH-*`）；`/reconcile` 已使用 JMAP `Email/changes` 分页、Redis `state:jmap:since` 和 Redis 单飞锁，只有全部事件入队成功后才推进游标；`/healthz` 为 liveness，`/ready` 做端到端探测（配置 + Redis + 出站只读探测，最坏约 3s），但不覆盖真实消息投递验收。

- **实际依赖**（Cargo.toml 现状，`ARCH-DEPS-STAGE0`/`ARCH-DEPS-STAGE1` + `ARCH-DEPS-STAGE4`）：`axum 0.8`（单一 HTTPS 入口）、`async-trait`、`secrecy`、`subtle`（常数时间鉴权比较）、`serde`、`serde_json`、`thiserror`、`tokio`、`tracing`、`tracing-subscriber`、`url`（`JMAP_SESSION_URL` 归一化解析）、`jmap-client =0.4.2`（`default-features = false, features = ["async","rustls"]`）、`redis 0.27`（Redis XPING/PING 活性探测，`ARCH-STATE-REDIS`）、`reqwest 0.13`（JMAP/TG HTTP 客户端）；dev-dependencies：`tower 0.5`（路由测试）。`teloxide` 未引入（`ARCH-DEPS-STAGE4`）：Telegram 渠道在 `src/channel.rs` 用 reqwest 自研实现。
- **尚未引入**（文档不得声称已用）：`teloxide` 等任何 Telegram Bot 框架（Telegram 出站由 `src/channel.rs` 用 `reqwest` 直发，评估记录见 `docs/retired.md`）。**未使用 figment**：配置为手工 `std::env` 解析（`ARCH-CONFIG-ENV`，§7.1）。
- **阶段1 依赖现况**（`ARCH-DEPS-STAGE1`）：`jmap-client` **已引入**（当前 `=0.4.2`，`default-features = false, features = ["async","rustls"]`，版本与 features **以 `Cargo.toml` 为准**）。⚠️ 其默认 features `["async","websockets","aws_lc_rs"]` **含 WebSocket 栈**，故必须关闭默认 features 且不选 `websockets`；JMAP 仅走 **HTTPS 短请求**（Core/Mail），以遵守 `C-NO-LONG-CONN`（无 WS/SSE/长轮询）。
- `ACCOUNT_ID` **可选**（`REQ-SINGLE-ACCOUNT`）：**留空 → 取 JMAP session 的默认/主账户**；显式值经校验后使用；多账户 = 多个 bot 实例。
- axum 采用 **0.8**（`ARCH-AXUM-08`）；如后续审核决定调整版本，以 Cargo.toml 为准并同步本节。
- 当前不实现 SSE/WebSocket/长轮询/SQLite/本地卷（`C-NO-LONG-CONN`/`NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`）；入口鉴权（`R1`/`SAF-AUTH-*`）作为 fail-closed 硬门禁落地，鉴权之后的 Push、Streams worker 和 `/reconcile` 业务路径已实现。

**阶段0 P0 门禁（`GATE-P0`）——已通过。首次冻结时为 45 passed / 0 failed / 1 ignored，此后随 ④–⑥ 轮实现持续增长，当前基线以 `docs/roadmap.md` 头部为准。以下为阶段0 当时的判据，保留作历史记录：**
1. `cargo fmt --check` 通过（无格式差异）。
2. `cargo clippy --all-targets -- -D warnings` 通过（零告警；禁 crate 级 `allow`）。
3. `cargo test` 通过（含路由/配置最小测试）。
4. 上述三条**在 Debian `rust:1-slim-bookworm` 容器内**执行通过（`C-DEBIAN-SLIM`）。
5. 配置读取为 env-only（无 figment/TOML）；`SecretString` 包裹秘密且 `Debug` 不泄密；`JMAP_PASSWORD` 安全访问器保留。
6. `RUN_MODE` **不**参与路由分派：`webhook` 与 `reconcile` 共享同一套路由表，`reconcile` 作为独立 HTTP 端点 `POST /reconcile` 提供。`RUN_MODE` 仅在启动时读取并校验取值（`src/main.rs:75`，只接受 `webhook`/`reconcile`），**当前不影响任何运行时行为**，留待后续阶段在此挂载差异化副作用（见 §12.2 的运行模式说明）。
7. `.gitignore` 存在（排除 `target/` 等；**不**擅自初始化 git）。
8. 所有新增代码有引用稳定 ID 的必要注释；单 `.rs` ≤ 500 行。

**运行监控（`GATE-UPTIME-KUMA`）：**
- 使用 Uptime Kuma HTTP(s) Monitor 检查 `/healthz`（进程存活）和 `/ready`（配置 / Redis / 上游可达就绪），分别期望 HTTP 200；`/ready` 最坏约 3s，探针超时需设 ≥10s；`/ready` 不就绪时返回 `503` 与标准错误 envelope（`service_unavailable` + `Retry-After: 30`），Uptime Kuma 仍按状态码判定，不受响应体变化影响。
- `/healthz` 是纯 liveness（无条件 `200`）；`/ready` 检查配置完整性 + Redis 可达性 + 出站只读探测（JMAP session `GET`、TG `getMe`，各 3s、并行，最坏约 3s），探针只读、不回显 token 或第三方响应、不触发业务副作用，报告体不含敏感信息；由于 `/ready` 是最重的一环（可能 3s），平台侧应优先使用网关聚合的 `/healthz` 作为存活探测，避免高频出站请求。
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

### 10.2 阶段 2：渠道适配（已完成）
- `src/channel.rs` 定义 Channel / Notifier / MessageAdapter 抽象 + 领域 Command/Notification 类型。
- Telegram 装配用 `reqwest` 直发 `https://api.telegram.org/bot{token}/sendMessage`（**未引入 teloxide**；评估记录见 `docs/retired.md`）。
- 入站：`POST /webhook/tg` 校验 secret token → 按 `update_id` 去重（`dedup:tg:{update_id}`）→ 入 Redis Streams → 2xx；出站为 Telegram 唯一用到的 Bot API 端点。
- 意图路由：`src/worker.rs` 的 `parse_intent` 解析 6 种意图 —— `Help` / `Consent { ttl, label }` / `Summary(email_id)` / `Search(query)` / `Query` / `Unknown`，全部走自然语言触发词（AI 授权词见 `docs/reference.md` 的 AI 授权态一节）。`/search` 详见 §2.3、§5.7；中文搜索词只做**前缀匹配**（`搜索/查找/检索` + `/search`），避免覆盖授权与摘要意图。
- 渲染在 adapter 内部：领域 Notification → TG Markdown/HTML + 转义。
- 会话存储走外部 Redis（`C-REDIS-ONLY-STATE`；**不用 SQLite**）。
- 验收：本地 webhook 形态，测试客户端发消息能得到回复；领域模块不导入任何渠道 SDK 类型。

### 10.3 阶段 3：发送邮件 + 状态管理（**未实施**）
- `send_email`（draft + submission_set）**当前未实现**：`JmapBackend` 只有只读动词。
- 发信流程原本计划用多步 FSM 收集 to/subject/body，**从未实现**（见 `docs/retired.md` 的对话 FSM 一节）。
- `/flag /unseen` 关键词标记**未实现**。
- 缺口与阶段归属见 `docs/roadmap.md`。

### 10.4 阶段 3.5：LLM 门面 + 回退（1.5d，未执行）
未实施，见 §12.8 与 `docs/retired.md`。

### 10.5 已实现能力：实时推送（Push 回调 + Streams worker + 外部 Cron 对账）
`src/notify.rs` 是**单个近 2000 行的平铺文件**，没有子模块（下表中不存在 `notify::push_handler` / `notify::worker` / `notify::reconcile` / `mod_streams` / `mod_sincestate` 这些模块名，对应实现是文件内的自由函数）：
- `register_push`（`POST /api/push/register`）：接受显式 HTTPS callback URL 并创建订阅；收到 Stalwart 推送时用 JMAP `PushSubscription/set` 回写 `verificationCode`。
- `jmap_push`（`POST /push/jmap`）：校验 subscription ID + verificationCode → 去重 → 入 Redis Streams → 2xx。
- `worker`（`POST /worker`）：Redis Streams 消费 → `Email/changes` 增量 → 通知 → 推进 sinceState → XACK。
- `reconcile`（`POST /reconcile`）：外部 HTTPS Cron 触发（`FLOW-RECONCILE`）；兼做 Redis 丢失后的游标重建。**没有 CLI 子命令形态**。
- sinceState 写外部 Redis（`state.rs`，`C-REDIS-ONLY-STATE`；不用 SQLite/文件 `NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`）。
- 目标验收：向 Stalwart 发测试邮件，经 Push 回调 + worker，Telegram 在 ~秒级收到推送；重启不重发；清空 Redis 后由对账补发。当前代码门禁已通过，但仍需在真实 Stalwart、Redis 和 Telegram 环境完成端到端验证，不能将此处目标当作已验证事实。

### 10.5.1 Redis Streams 事件保障边界（FLOW 运行语义）

Push 事件经 Streams 消费并投递到 Telegram，其关键路径交付语义如下：

**键与 TTL 的唯一权威来源是 `docs/reference.md`（Redis 键与 TTL 各节）。**本节不再重复表格——重复的表
会被改坏：此前曾把 `push:subscription:{id}` 标成 7d、把 `push:registration:{...}` 标成 360s 注册单飞锁，
两处都错，而真正的 360s 单飞锁 `lock:push-register:{sha256(callback_url)}` 当时整行缺失。现已收敛到
单一权威表，本节只保留影响设计判断的四条"为什么"：

- **对账锁 TTL 300s，刻意大于单页 120s 上限**，避免持锁期间锁过期导致同一账号重复进入对账
  （`REQ-RECONCILE-IDEMPOTENCY`、`SAF-RECONCILE-LOCK`）。
- **push 注册单飞锁 TTL 360s，刻意大于单条出站请求上限 300s**，否则一次慢注册会放进重复请求
  （`SAF-AUTH-JMAP-PUSH`）。
- **对账去重 24h 是有意设计**：同一封邮件 24h 内只通知一次（`MOD-DEDUP`，范围见本文 11.5）。
- **配置键缺键即视为关闭（fail-closed）**，不进入业务路径。

**投递流程**（`notify::worker`，一次 XREADGROUP 批量 ≤10 条）

1. 读批 → 逐条 `process` → XACK。
2. 处理中写入 `delivery:inflight`（60s）作租约；成功后写 `delivery:committed`（7d）。
3. 崩溃于 `inflight` 租约窗口内的条目，由另一实例经 XAUTOCLAIM 回收重试（空闲阈值由运行超时配置推导，公式与上下限见 `docs/reference.md` §6.3）—— 至多重复、不丢。
4. `retry_or_dlq`（`state.rs`）：重试计数（`max_attempts.max(1)`）未到上限留在源流重试；达到上限则以**单个 Lua 脚本**原子地 `INCR`+`XADD`（入 DLQ）+`XACK`（源流确认），保证不会出现"源已 ACK 但既不在源也不在 DLQ"的缝隙。

**告警边界（当前实现，见 §8.3 监控约定）**

- 采用 Uptime Kuma HTTP(s) Monitor，不引入 Prometheus/exporter。
- DLQ 条数、inflight 积压深度、对账补齐条数等指标尚未自动上报（§8.3 标记为可选）。当前运维需通过 Redis 直接查看：`XLEN messageweave:dlq:*`、`XPENDING` 等。
- 若需自动化告警，建议在 Uptime Kuma 增加对 `/ready` 或对账入口的健康检查，并手动复核 DLQ 深度。

**已知边界（当前实现仍存在，见 `docs/roadmap.md`）**

- 多实例重复投递窗口（**已收口，`6c99ce5`**）：XAUTOCLAIM 空闲阈值不再按固定值缩放，改由运行超时配置推导——`(max_retries + 1) × (jmap + telegram + llm 超时) × 2` 为单条上限，再乘批大小，下限 300s、上限 6h（见 `docs/reference.md` §6.3）。提前认领窗口在单实例与多实例部署下均关闭；单实例不受影响，多实例最多重复、不丢。

**审计意见 → 收口（2026-09-26）**

- 【应修-2】入队失败时 dedup 释放 best-effort 曾可能造成 24h 静默丢事件 → 已修复为 `claim_dedup_and_enqueue`（Lua 原子：`SET NX EX` 成功才 `XADD`），claim 与入队之间无中间失败窗口。
- 【应修-1】`Email/changes` 依赖 `newState` 续传，`jmap-client 0.4.2` 无 `upToId` → 已改为「同 `sinceState` 下逐次翻倍 `maxChanges` 扩窗（上限 4096），仅在无法扩窗时才推进 `new_state`」，避免按页推进时漏批；`newState` 语义本身仍需真实 Stalwart 复验（见 `docs/roadmap.md`）。
- 其余低风险项均已收口：未知 stream 的空值改为 `Err`（fail-closed，进重试/DLQ）；`push:disable` 经 `forget_push_subscription` 清理验证码摘要键；`SET NX EX` TTL 下限收紧为 `.max(1)`；XAUTOCLAIM 空闲阈值按批大小缩放；无 payload 的畸形流条目由 `ack_malformed` 经 `XACK` 移出 PEL；CSPRNG 兜底 owner-token 改为「时间 + PID + 计数器」，不再使用常量。
- 未排期待办（阶段 5「搜索 + 搜索片段」）**已收口（`bfe0fd8`）**：`/search` 走 `email_query`(`Filter::text`) + `SearchSnippet/get`，高亮降级与截断上限见 §2.3、§5.7；正文级高亮在锁定版本做不到（见 §2.3 备注）。阶段 5 待办已清零。

### 10.6 总估时
~10.5 人日（不含等待用户确认与真实联调排障）。

---

## 11. 历史问题与决策归档（产品/架构类，均已有结论）

> ⚠️ 部署/平台类决策已**全部确认**（单账户 / App Password+Basic / Redis 托管+AOF / 平台 HTTPS URL / 外部 Cron 对账 / `Q-DEP-A` 平台 URL 与证书配置方 / `Q-DEP-B` 调度器选型），已归档到 `docs/deployment.md` 的已确认决策一节，不在本文重复。

### 11.1 认证方式（已确认）
- **已确认**：**App Password + Basic**（`C-AUTH-APP-BASIC`）。不用主密码、不用 OAuth2 Bearer（无 OIDC 需求）。
- 影响：`Credentials::Basic` 构造；无需 OAuth client / token 自动刷新模块。

### 11.2 实时通道（已定，见 deployment.md）
- 通道 = **JMAP Push HTTPS 回调 + 外部 Cron `/reconcile` 对账**（`C-NO-LONG-CONN`/`C-HTTPS-INBOUND`）；EventSource/SSE/WebSocket 均**非目标**（`NG-POLLING-SSE`）。Push 注册通过受保护的 `POST /api/push/register` 显式触发，外部 Cron 仍是必须的可靠补偿通道。
- 两条尚需在真实 Stalwart 上校准的点——账号角色是否具备 `PushSubscription` 权限、以及 Stalwart 回调重试次数与 TTL/对账间隔的匹配——已作为验收项登记在 `docs/roadmap.md`，不在本文以问题形式留存。

### 11.3 部署形态（已确认，架构相关）
- **已确认**：**单账户实现**（`REQ-SINGLE-ACCOUNT`）；多账户暂用**多个 bot 实例**（各自 token/配置），**不做多账户单实例**（因此无需 chat→account 路由与 `JmapService` 池化）。
- 部署形态 = **webhook-only + 通用 HTTPS-only Docker + 外部 Cron 对账**（deployment.md `C-NO-LONG-CONN`/`NG-LONG-POLLING`/`NFR-RECONCILE-INTERVAL`）。
- **多实例 LB/HA（已确认，`ARCH-LB-WORKER`）**：可选在多个 serverless 平台部署同镜像、共享同一 Redis，前置免费 Cloudflare Worker 做唯一入口与故障转移；Worker 代码位于子目录 [`cloudflare-worker/`](../cloudflare-worker/)（非本 Rust 二进制），部署见 `docs/deployment.md` 的 Worker 部署一节。信任模型为**透传**（后端仍 fail-closed 校验，`SAF-LB-PASSTHRU`），后端间**共享同一组 secret**（`C-LB-SHARED-SECRETS`），`/reconcile` **Redis 锁**单实例（`SAF-RECONCILE-LOCK`），Worker 提供**聚合健康视图**（`MOD-HEALTH-AGG`）。**Redis 单点故障不在本方案范围**（`NFR-HA-MULTI-INSTANCE`，用户外部解决）。双活或主备均可。详见 `docs/deployment.md` 的多实例部署一节。

### 11.4 已由代码回答的早期问题（不再待确认）
以下问题在设计阶段以 Q6–Q30 形式列出，**代码落地时已各自给出答案**，因此不再是"待确认项"，此处只记结论：
- **消息格式**（Q6）：`TelegramClient::SendMessage` 只带 `chat_id` + `text` 两个字段，**没有 `parse_mode`**——Telegram 收到后按纯文本渲染，既不用 HTML 也不用 MarkdownV2。
- **长邮件 / 原文直发**（Q7、Q29）：不存在"正文超过 4000 字符不发全文"的策略。`read_email` 拉全文送 LLM；AI 失败时回退为前 300 字符且**无"已截断"标注**（见 §12.3/§12.5）。
- **附件**（Q8、Q28）：完全未实现。领域模型只有 `has_attachment: bool`，出站只有 `sendMessage`，没有 `send_document` 也没有 `Blob/get`（见 §12.3）。
- **推送范围**（Q9）：对账拉全量 `changes`，Bot 侧不做文件夹/发件人/关键词过滤，也没有 Sieve 依赖（Q11 因此无影响）。
- **摘要聚合 / 定时汇总**（Q10）：未实现，只有 `/reconcile`。
- **多发件身份**（Q12）：`Identity` 未使用，单账户（`REQ-SINGLE-ACCOUNT`）。
- **unsafe**（Q13）：`src/` 中没有 `unsafe`，但也没有加 `#![forbid(unsafe_code)]`。
- **监控**（Q15）：实现为 `/healthz` + `/ready` 两个 HTTP 探针，不引入 Prometheus/Exporter；运行平台配置见 deployment.md。
- **LLM 提供方 / 网出许可**（Q25、Q26、Q30）：`LLM_BASE_URL` 由部署方指定（只校验 https）；`LLM_ENABLED` 与 `LLM_ALLOW_NET` **默认均为 `false`**，二者须同时为真才构造客户端，否则 `llm` 字段为 `None`（`Option<Arc<LlmClient>>`，仓库内不存在 `noop()` 实现），不探测（见 §12.2）。
- **熔断冷却 / 阈值**（Q27）：不适用——熔断本身未实现（见 §12.4）。

部署/平台类决策已无未决项：`Q-DEP-A`（平台 URL / 域名与证书由谁配置，由部署环境在发布时确定）与 `Q-DEP-B`（外部 Cron 用哪个调度器，不限定实现）均已决策，归档到 `docs/deployment.md` 的已确认决策一节，不在本文重复。

---

## 12. AI 辅助能力：架构、确认门槛、失败回退

> **用户已确认的 7 项需求（本节据此设计，后续所有表述以此为准）**：
> 1. **AI 仅在被明确要求并确认后接触正文**（`REQ-AI-CONFIRM` / `REQ-AI-EXTERNAL-CONSENT`）：只有当用户明确发起"分析/总结/翻译"意图**并再次确认**后，才允许把正文喂给 LLM；**仅当用户明确允许时才向外部 AI 发送邮件正文**（默认不外发）；其余场景（包括查看原文）**一律不经 AI**，由 JMAP 直取。
> 2. **查看原文始终 JMAP 直取**（`REQ-VIEW-DIRECT`）：`Intent::Query` 与任何"看正文"动作走 `JmapService::read_email`，不走 LLM。
> 3. **长邮件禁止直接发送完整原文**（`REQ-LONG-EMAIL`）：正文超过阈值（默认 4000 字符，见 §12.3，可调）时，Bot **不直接发送全文**，而是提示"正文较长，请电脑查看"或"选择 AI 总结"；AI 摘要目标约 **300 字**，**不做代码侧硬性数学截断**（由 LLM 自然生成 ~300 字）。
> 4. **分析/摘要结果不持久化**（`REQ-ANALYSIS-EPHEMERAL`）：不写缓存、不入 Redis、不留磁盘；每次请求即取即弃（无状态天然友好）。
> 5. **附件按需拉取（保留）**（`REQ-ATTACH-ONDEMAND`）：不预载；`Blob/get` 仅在用户点击下载时触发，单文件 ≤50MB 发文件，超限给 `download_url` 链接。
> 6. **AI 失败约 3 次后需用户确认的非 AI 回退（保留）**（`REQ-AI-FUSE`）：连续 3 次失败 → 熔断 + 向用户弹确认；选择"关闭 60s" 冷却期内直接走规则模板 + 原文直取路径；选择"恢复 AI" 则做一次探测。
> 7. **OpenAI-compatible 环境变量（保留）**（`REQ-LLM-OPENAI-COMPAT`）：`LLM_API_KEY / LLM_BASE_URL / LLM_MODEL` 等。
>
> 本节仅做设计，不写业务代码。

### 12.1 模块划分（当前实现）
LLM 相关代码只有**一个文件**：
```
src/ai.rs   # LlmClient（唯一实现）：summarize() 打 OpenAI 兼容 /chat/completions
```
配置装载在 `src/config.rs` 的 `LlmConfig`，运行时参数在 `src/state.rs` 的 `OutboundConfig`（经 `RuntimeConfigProvider` 下发）。
**不存在** `src/domain/llm/` 目录，也不存在其中的 `mod.rs` / `client.rs` / `config.rs` / `policy.rs` / `fallback.rs` / `audit.rs`——熔断、规则回退、审计 span 全部未实现（见 §12.4–12.7）。
调用方只有 `worker.rs` 的 `MetadataWorker`（已授权摘要）和 `notify.rs`（新邮件通知）。

### 12.2 环境变量（当前实现）
| 变量 | 类型 | 默认 | 用途 |
|---|---|---|---|
| `LLM_ENABLED` | bool | `false` | 总开关；默认关闭，未启用时不校验 `LLM_API_KEY`/`LLM_BASE_URL`/`LLM_MODEL` |
| `LLM_ALLOW_NET` | bool | `false` | 运行时出站许可；`LLM_ENABLED && LLM_ALLOW_NET` 同时为真才构造 `LlmClient`，否则 `llm` 字段为 `None`（`src/main.rs:125`） |
| `LLM_API_KEY` | string | 可省略 | Bearer token；仅 `LLM_ENABLED=true` 时必填 |
| `LLM_BASE_URL` | URL | 可省略 | 仅启用时必填，且必须 `https`，否则 `AiError::InvalidEndpoint` |
| `LLM_MODEL` | string | 可省略 | 仅启用时必填；透传给 `/chat/completions` 的 `model` |
| `LLM_SUMMARY_TARGET_CHARS` | int | `1024` | `LlmClient::max_chars`，对返回摘要文本做字符截断（**代码侧确有截断**） |
| `max_retries`（运行参数） | int | `3` | 由 `OutboundConfig` 下发（`RuntimeConfigProvider`），硬上限 5；LLM 与 Telegram 出站共用 |
| `llm_timeout_ms`（运行参数） | int | `30000` | 硬下限 100ms |

**不存在** `LLM_TEMPERATURE` / `LLM_MAX_TOKENS` / `LLM_TIMEOUT_SECS` / `LLM_MAX_RETRIES`：请求体只有 `model` / `messages` / `max_tokens`（= `max_chars * 2`），没有 temperature，也没有独立于 `llm_timeout_ms` 的超时开关。API key 只从配置读，不入源码、不打日志（`SAF-LOG-PURITY`），运行期 secret 注入方式见 deployment.md（`C-NO-SECRET-IN-IMAGE`）。

### 12.3 正文获取策略与长邮件处理（需求 1/2/3/5）
- **查看原文 = JMAP 直取**：`Intent::Query` 与任何"看正文"动作走 `JmapService::read_email`，**不经 LLM**；LLM 不在查看路径上。
- **AI 接触正文的前置门槛**：只有当用户发起明确的"分析/总结/翻译"意图**并再次确认**（例如点 `[AI 总结]` 按钮 / 执行 `/summarize`）后，才把正文作为 LLM 输入。未经确认 → LLM 看不到正文。
- **没有"长邮件保护"逻辑**：不存在 4000 字符阈值、不存在 `Preview` 类型、不存在 `[继续查看原文]` 按钮。授权后的摘要路径就是把**全文**交给 LLM，LLM 失败时回退为前 300 字符，全程无截断标注。
- 回退文本**没有任何标注**：`fallback()` 就是 `body.chars().take(300)` 直接返回，不追加"已截断"或"AI 不可用"字样。
- **附件功能完全未实现**：`src/domain/jmap.rs` 明确注释"no mutation, attachment, AI, or streaming APIs"，`read_email` 只返回正文文本；领域模型里唯一的附件信息是 `has_attachment: bool`。Telegram 出站只调 `sendMessage`，**不存在** `send_document` / `Blob/get` 流式下载 / 下载按钮。
- AI 摘要的输入只有邮件正文文本，没有附件通道。

### 12.3.1 AI 授权期限与 Redis TTL（`REQ-AI-CONSENT`）
- AI 授权必须由用户明确选择期限；可选 `临时一次`（一次授权窗口，3600 秒）、今天（86400 秒）、7 天（604800 秒）或直到撤销（最长 365 天，即 31536000 秒）。触发词按原文包含匹配（`临时`/`一次`/`今天`/`7天`/`直到我撤销`/`长期`，以及 `/ai on`、`/ai yes`、`/ai off`），不支持英文别名。
- Redis 的 consent key 仅保存 chat id 对应的到期 Unix 时间，并使用**与所选期限完全相同**的 `SET EX` 秒数；不使用隐式 30 天默认值，也不保存正文或摘要。
- 到期由 Redis TTL 和读取时的到期校验共同保证，摘要请求回到元数据模式并提示“授权已到期”；用户重新选择期限后才可再次授权。`/ai off` 立即删除 key。

### 12.4 失败检测（当前实现）
`LlmClient::summarize`（`src/ai.rs`）**只有重试，没有熔断**：
- 超时 `llm_timeout_ms`（默认 30_000ms，最小 100ms），最多重试 `max_retries` 次（默认 3，硬上限 5，与 Telegram 出站共用同一配置项）。
- 只对上一次的失败重试：429 与 5xx 会重试；**其他 4xx（含 401/403）立即返回错误，不重试**。
- 重试耗尽或全部超时 → `Err(AiError::Response)`；URL 解析失败或非 https → `Err(AiError::InvalidEndpoint)`；JSON 反序列化失败或缺 `choices[0].message.content` → `Err(AiError::Response)`。
- `AiError` 只有三个变体：`InvalidEndpoint` / `Request` / `Response`。**不存在** 429/401/Timeout 的细分类型。

### 12.5 非 AI 回退（当前实现）
`worker.rs` 的 `fallback()` 就是**取正文前 300 个字符**，仅此而已：
- AI 授权有效且 LLM 调用失败 → 静默换成这 300 字符，**不提示用户**、不加"AI 不可用"标记。
- AI 未授权 → 不发摘要，回一条 `发件人 — 主题` + "授权已到期或尚未授权"。
- 因此不存在熔断半开态、不存在确认回退的 inline-button、不存在 60s 冷却期、不存在写进 Redis 的熔断计数（Redis 上没有任何 LLM 相关状态键）。

### 12.6 无状态/容器化适配
- 摘要**不持久化、不缓存**：`worker.rs` 每次即时调用 LLM，结果不写 Redis、不落盘。与 `C-REDIS-ONLY-STATE` 一致。
- 日志不记 prompt 明文与 API key（`SAF-LOG-PURITY`）——**但当前日志也不记任何 LLM 调用事件**（见 §8.3）。
- 生效的运行参数只有 `llm_timeout_ms`、`max_retries`、`LLM_SUMMARY_TARGET_CHARS`（对应 `LlmClient::max_chars`，对摘要输出截断）。**不存在** `LLM_MAX_RETRIES` 这个环境变量。
- 请求体只有 `model` / `messages` / `max_tokens`（`max_tokens = max_chars * 2`）；**没有 temperature、没有 system prompt**——正文直接作为唯一的 user 消息发出。

### 12.7 错误处理与可观测性（接 §8）
**未实现** `BotError::Llm` 变体（`BotError` 只有 `Config`/`Io`/`Json`/`State` 四个），LLM 错误在 `worker.rs` 内被就地吞掉并降级为回退文本，不向上传播。
**未实现** `llm.call` 的 `tracing` span（无 prompt 哈希、response 长度、latency 记录）。这两项连同 12.4 的熔断/半开设计、12.5 的规则回退与"AI 不可用"徽标，一并记入 `docs/retired.md`。

### 12.8 计划中的「阶段 3.5」未执行
原计划要在阶段 3 与 4 之间插入「阶段 3.5：LLM 门面 + 回退（1.5d）」，交付 `llm::client` / `llm::policy` / `llm::fallback` 三个模块，外加 4000 字符长邮件阈值、"AI 不可用"徽标和 3 次熔断 + Redis 共享计数。
**这套计划没有按设计执行**：LLM 能力最终落在单文件 `src/ai.rs`（见 §12.1），没有门面、没有 policy、没有 fallback 模块，也没有徽标和阈值分支。被放弃的部分记入 `docs/retired.md`。
本节原有一组 Q25–Q30 的设计提问也已作废——代码落地时已各自给出结论，答案见 §11.4。

---

## 附：调研依据（可复核）
- `stalwartlabs/jmap-client` main 分支：`src/lib.rs`（URI/Method/DataType/Error）、`src/client.rs`（认证/连接/event_source）、`src/email/`、`src/email_submission/helpers.rs`、`src/event_source/`、`src/push_subscription/`（create/verify/update_types/destroy）、`src/core/error.rs`、`Cargo.toml`、`README.md`、`examples/`。
- crates.io：`jmap-client` 元数据。
- `stalwartlabs/mail-server` main 分支：`crates/common/src/auth/credential.rs`（Password/AppPassword/ApiKey）、`crates/http/src/auth/authenticate.rs`（AccessScope 权限裁剪）、`crates/jmap/src/push/`、`api/v1/openapi.yml`（`securitySchemes`: basicAuth/bearerAuth/liveToken 60s）。
- 项目目录 `/home/okabe/Repo/messageweave/`（工具链要求见 §10.0 与 AGENTS.md）。
- 部署/平台相关调研依据（lambda_runtime/worker/aws-sdk 等）见 deployment.md。
