# Stalwart JMAP ↔ Telegram Bot — Rust solution design

> [中文版本 / Chinese version → design.zh-CN.md](design.zh-CN.md)

> Status: **Design and implementation have been implemented** (The code can be found in the warehouses `src/`, `web/`, `cloudflare-worker/`; this article also retains the original records of design decisions and selection evaluations. The description of stages 1/2/4 belongs to the historical plan, and the current progress can be found in §10.5)
> Author: Cowork (team: MessageWeave)
> Date: 2026-09-21
> Target readers: Codex CLI (lead), follow-up AI coding agent, end-user review
>
> **Related documents** (separation of responsibilities to avoid duplication):
> - [docs/charter.md](charter.md) — Project charter: project goals, technology selection, security invariants, implementation phases, prohibited matters, test acceptance and stable ID registry
> - [AGENTS.md](../AGENTS.md) — Universal, language-independent code writing and environment building specifications (excluding project-specific content)
> - [docs/deployment.md](deployment.md) — Deployment, operation, maintenance and release: general HTTPS-only Docker container, Secrets, Redis (the only carrier of status), short request Webhook/Push/reconciliation routing, health check, CI release and items that still need to be confirmed
>
> This document only retains content directly related to **product behavior, code architecture, module interfaces, state machines, data flow, error handling, testing and implementation phases**. Deployment operations and Docker/universal container platform details have been moved to `deployment.md`; project constraints and security invariants are in `docs/charter.md`; general, language-agnostic code writing and environment building specifications are in `../AGENTS.md`.

---

## 0. Document navigation

1. [Task and Scope](#1-Task and Scope)
2. [jmap-client capability analysis](#2-jmap-client-capability analysis)
3. [Adaptation of authentication and email operations] (#3-Adaptation of authentication and email operations)
4. [Telegram channel implementation selection](#4-telegram-channel implementation selection)
5. [Overall architecture and data flow](#5-Overall architecture and data flow)
6. [Module Division](#6-Module Division)
7. [Configuration and Security](#7-Configuration and Security)
8. [Error Handling and Observability](#8-Error Handling and Observability)
9. [Test Strategy](#9-Test Strategy)
10. [Phase-based implementation plan](#10-Phase-based implementation plan)
11. [Historical issues and decision-making archives (product/architecture category, both have been concluded)] (#11-Historical issues and decision-making archives, product architecture category have been concluded)
12. [AI auxiliary capabilities: architecture, confirmation threshold, failure fallback] (#12-ai-auxiliary capability architecture confirmation threshold failure fallback)

> Deployment/platform decisions confirmed (Single Account, App Password+Basic, Redis Hosting+AOF, Platform HTTPS URL, External Cron) and **all converged archive** (including `Q-DEP-A`/`Q-DEP-B`, see the Confirmed Decisions section of `docs/deployment.md`); **Unfinished code gaps can be found in `docs/opengaps.md`**.

---

## 1. Task and Scope

### 1.1 Goals
Build a Telegram robot written in Rust as a **personal email assistant** for Stalwart JMAP mailbox:

- Query/read emails, view folders, send emails, manage keywords, etc. through Telegram commands.
- Leverage JMAP's **Push HTTPS callback** (+ external Cron reconciliation) to **proactively push to Telegram** when new emails arrive. (EventSource/SSE/long polling is non-target, see deployment.md `NG-POLLING-SSE`/`NG-LONG-POLLING`/`C-NO-LONG-CONN`.)
- Single-user or multi-account deployment (the default is for single-account self-hosting scenarios).

### 1.2 Scope (this stage)
- ✅ Research + Plan Design (this document)
- ❌ Do not create/modify project code or initialize cargo project
- ✅ Output detailed plans for lead and user review, marking key uncertain items

### 1.3 Research basis
- [`stalwartlabs/jmap-client`](https://github.com/stalwartlabs/jmap-client) (main branch, as of this research)
- Project directory: `/home/okabe/Repo/messageweave/` (see §10.0 and `../AGENTS.md §3.3` for tool chain requirements)

---

## 2. jmap-client capability analysis

Source: Read directly from the `stalwartlabs/jmap-client` warehouse `stalwartlabs/jmap-client/src/lib.rs`, `stalwartlabs/jmap-client/src/client.rs`, `stalwartlabs/jmap-client/src/email/`, `stalwartlabs/jmap-client/src/email_submissi on/`, `stalwartlabs/jmap-client/src/event_source/`, `stalwartlabs/jmap-client/Cargo.toml`, `stalwartlabs/jmap-client/README.md`, `stalwartlabs/jmap-client/examples/`.

### 2.1 crate overview
| item | value |
|---|---|
| crate name | `jmap-client` (crates.io) |
| Protocol coverage | JMAP Core (RFC 8620), Mail (RFC 8621), WebSocket (RFC 8887), Sieve (draft-12) |
| Asynchronous runtime | tokio + reqwest |
| License | Apache-2.0 OR MIT |
| `forbid(unsafe_code)` | Yes (stated at top of lib.rs)✅ |
| Default features | **0.4.2 Tested `default = ["async", "websockets", "aws_lc_rs"]`** (including WebSocket stack `tokio-tungstenite`). ⚠️ `default-features = true` will **implicitly enable WebSocket**, violating `C-NO-LONG-CONN`. **This project actually uses `default-features = false, features = ["async", "rustls"]` (without `websockets`)**, subject to `Cargo.toml`. |

### 2.2 Module structure (`src/`)
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

### 2.3 Key capabilities mapped to this Bot
| Bot requirements | jmap-client API | Remarks |
|---|---|---|
| Login/Session | `Client::new().credentials(...).connect(url)` | Supports Basic and Bearer; connect resolves session URL, capabilities |
| Column folder | `mailbox_query` / `mailbox_get` | With `role` to identify INBOX/Important/Draft, etc. |
| List emails | `email_query` (Filter + Comparator + anchor paging) | Filter: `subject`/`from`/`to`/`in_mailbox`/`has_keyword`/`after`/`before`… |
| Read email text | `email_get` + `Property` selection | `BodyStructure`/`BodyValues`/`Preview`/`TextBody`/`HtmlBody` |
| Read attachments | `Blob/get` (blobId) or `email_parse` | Large attachments need to be downloaded in fragments/streaming |
| Send email (2 steps) | ① `email_set`/`email_import` create draft ② `email_submission_set` send | submission associate identityId |
| Delete/Archive | `email_set` (keywords `$seen`/`$flagged`), `mailbox_destroy` | JMAP has no real "delete", relying on keyword/move |
| Search | `email_query` (`Filter::text`) + `SearchSnippet/get` highlighting | `Filter` is a serde single-label enumeration, **no comparator syntax**; `SearchSnippet/get` only returns `emailId`/`subject`/`preview`, **no `bodyProperties`/`parts`**, body-level highlighting in locked version `0.4.2` can't do it (downgraded to a pure list of IDs) |
| Real-time notification (Push + reconciliation) | Push HTTPS callback → `StateChange`; external Cron calls `/reconcile` and uses `Email/changes` to make up the difference | Requires public HTTPS entrance (deployment.md `C-HTTPS-INBOUND`/`FLOW-NEW-MAIL`); Push is not the only reliable source |
| SSE / WebSocket (non-target) | `event_source` / `client_ws` | This deployment **does not use** (deployment.md `NG-POLLING-SSE`/`NG-LONG-POLLING`/`C-NO-LONG-CONN`); only crate capabilities are listed for investigation |

### 2.4 Authentication mechanism (`stalwartlabs/jmap-client/src/client.rs`)
- `Credentials::Basic { username, secret }` — username/password, natively supported by Stalwart.
- `Credentials::Bearer { token, .. }` — OAuth2 access_token; optional `refresh_token` + `refresh_url` + `refresh_grace`, the client will automatically refresh before expiration.
- `connect()` Function: GET session URL → Parse `accounts`/`capabilities`/`download_url`/`upload_url`/`event_source_url`, cache account_id.
- Support custom `reqwest::Client` (`Client::new().client(reqwest_client)`): can inject proxy, TLS configuration, timeout, UA.
- Support `accept_long_responses` / `event_source(ping, ..)` for long connection keep-alive.

### 2.5 Error model (`Error` enumeration)
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
- `MethodErrorType` fine-grained: `ServerUnavailable`/`ServerFail`/`RateLimit`/`InvalidArguments`/`Forbidden`/`StateMismatch`/`TooManyChanges`… → Can directly drive Bot’s retry/current limiting/state reset strategy.

### 2.6 Real-time channel decision-making (determined, see deployment.md)

> Deployment target = **Generic HTTPS-only Docker, no long connections** (deployment.md `C-NO-LONG-CONN`/`C-HTTPS-INBOUND`).
> Current channel = JMAP Push HTTPS callback (first explicitly registered by the administrator via `POST /api/push/register`, see §7.3) + external Cron `/reconcile` reconciliation. Push subscriptions are not created automatically, and Push cannot be regarded as the only reliable source.
> EventSource/SSE and WebSocket are both marked as **non-target** (`NG-POLLING-SSE`). Only the following table is retained for reference for crate capability research.

| Channel | Whether to use | Remarks |
|---|---|---|
| **Push Subscription (HTTP callback)** | Accessed, explicit registration required | Short request model, suitable for stateless + no long connection; `POST /api/push/register` creates subscription, `/push/jmap` automatically completes Stalwart verification writeback |
| EventSource/SSE | ❌ non-target | long connection, conflicts with `C-NO-LONG-CONN` (`NG-POLLING-SSE`) |
| WebSocket (RFC 8887) | ❌ Non-target | Long connection, conflicts with `C-NO-LONG-CONN`; and needs server support |

Reconciliation: External HTTPS Cron periodically adjusts `/reconcile` (deployment.md `FLOW-RECONCILE`), uses `Email/changes` + Redis `sinceState` to make up for the difference, and also serves as cursor reconstruction after Redis is lost.

---

## 3. Authentication and email operation adaptation

### 3.1 Authentication Strategy (Confirmed: App Password + Basic)
- **Confirm adoption** (`C-AUTH-APP-BASIC`): `Credentials::Basic` (Stalwart account email + **App Password**). App Password can be revoked/expired independently without using a master password. Configuration injection, no code required.
- **Not used**: OAuth/OIDC `Bearer` (increases complexity when there is no OIDC requirement, do not select); master password Basic.
- **Security**: The password is only saved in the configuration or key manager; wrapped with `secrecy::SecretString` during runtime, the log will never print plain text (see §7.2).
- For runtime injection methods, see deployment.md (`C-NO-SECRET-IN-IMAGE`).
- **Single Account** (`REQ-SINGLE-ACCOUNT`): This instance only connects to one Stalwart account; multiple accounts = deploy multiple bot instances.

### 3.2 Mailbox operation adaptation layer (JMAP ↔ Bot semantics)
It is recommended to encapsulate a layer of domain semantics in the `domain::jmap` module and only expose business verbs to the upper layer:

| Bot verb | Wrapper function | Internal JMAP |
|---|---|---|
| `list_folders()` | → `Vec<Folder>` | `mailbox_query` + `mailbox_get` (cache role→id) |
| `list_emails(folder, page)` | → `Vec<EmailMetadata>` | `email_query`(anchor paging) + `email_get` does not pass `properties` (take the server's default full value, including `From`; the list page must also display the sender) |
| `read_email(id, want_body)` | → `EmailBody { text, html, attachments }` | `email_get`([BodyStructure, BodyValues, BlobIds]); **Multiple part original text** (`REQ-JMAP-RAW-MULTIPART`): Filter "parts with `part_id` and `bodyValue`" in order of `text_body` The last **splicing** is `text`; if there is no available part → return **clear error** (do not silently return an empty string). Attachments use `Blob/get` |
| `send_email(to, subject, body, attachments)` | → `EmailId` | ① `email_import`/`email_set` Create draft ② `email_submission_set`(onSend) |
| `set_flag(id, keyword)` | → () | `email_set` keywords |
| `search_emails(account_id, query, limit)` | → `Result<Vec<SearchResult>, JmapError>` | `email_query` (`Filter::text`, `limit` capped at 100) gets the ID + `SearchSnippet/get` gets the highlight; when the latter does not support (`unknownMethod`/timeout) **downgrade** returns empty snippets Instead of reporting an error, the caller renders a pure ID list |

> Design points: `anchor`+`position` paging of `email_query` is JMAP standard practice and is more stable than traditional offset; `sinceState` + `changes` is used for incremental synchronization to avoid repeated pulling of the entire volume.

---

## 4. Telegram channel implementation selection

Telegram channel is self-developed and implemented using `reqwest` in `src/channel.rs` (`ARCH-DEPS-STAGE4`), **without introducing third-party Bot framework**.
Evaluated `teloxide` but **not adopted**: This project only uses a few calls of the Bot API, and it is not worth introducing the two layers of abstraction of dptree scheduling and built-in session middleware; `throttle` and Redis session capabilities are partially implemented using `src/channel.rs` retries and external Redis TTL.
Candidate comparison (`teloxide` / `grammers` / old `telegram-bot`), 6 reasons why we originally preferred `teloxide`, feature set plan and `teloxide-core` downgrade plan, all recorded in `docs/retired.md`.

---

## 5. Overall architecture and data flow

### 5.1 High-level topology
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
> Deployment form = **General HTTPS-only Docker, no long connection** (deployment.md `C-NO-LONG-CONN`/`C-HTTPS-INBOUND`):
> Telegram is Webhook only; JMAP real-time is Push callback only; slow tasks are asynchronously stepped into Redis Streams; reconciliation is triggered by **External HTTPS Cron**.
> EventSource/SSE and long polling are non-targets (`NG-POLLING-SSE`/`NG-LONG-POLLING`).
> Channel decoupling (see §5.2): The domain layer interacts with the channel layer through domain Command/Notification, and no specific channel SDK type appears in the domain layer; DingTalk/Feishu only retains extension bits and does not implement them in advance.

### 5.2 Channel abstraction and multi-channel expansion strategy
- **Status quo**: The first (and currently only) channel is Telegram. The goal is to retain the ability to expand DingTalk/Feishu in parallel, but not to achieve it ahead of time.
- **Principle**: Decoupling the domain layer and channel layer - Mail/JMAP/AI/intention state machine ** must not rely on Telegram type**.
- **Abstract (not over-engineered)**: No empty traits are reserved. `src/channel.rs` had three zero implementations
  `#[expect(dead_code)]` Placeholder trait (`Channel` / `Notifier` / `MessageAdapter`, comment readme
  "Stable ID + Phase 0 placeholder"), **Deleted** (see `docs/retired.md` for registration) - Actual Telegram outbound
  `channel::telegram::TelegramClient::send_text` (self-developed by `reqwest`), by `worker.rs`
  `MetadataWorker` and `notify.rs` are held directly and are never passed through these three traits. The command is parsed in
  `parse_intent` (`src/worker.rs`), rendering logic is scattered in `src/worker.rs` / `src/notify.rs`.
  What is passed between the domain and the channel is the domain `Notification` (`src/domain.rs`), and the channel only adapts at the edge.
- **Constraints**:
  - The public interface of `jmap` / `llm` / intent state machine / `notify::core` only accepts/returns domain types;
  - Specific channel SDK types must not appear in the domain layer (the channel layer does not currently introduce third-party Bot frameworks, see §4);
  - Add new channel = new adapter + assembly, do not change the domain layer. When abstraction is really needed, add it with the implementation and leave no empty traits.
- **What not to do**: Do not define a polymorphic configuration registry, do not abstract the "channel capability matrix" in advance, do not build a plugins mechanism; evolve on demand (YAGNI).

### 5.3 Running model (short request, no long connection)
- Single binary `message-weave`, `#[tokio::main]`.
- On startup:
  1. Load configuration (`config::Config`).
  2. Construct `JmapService` (`Client::connect` completes session parsing, account_id caching, mailbox role→id mapping warm-up); sinceState is restored from external Redis (`MOD-SINCESTATE`).
  3. Start the **HTTP portal** (axum, single port `PORT`): `/webhook/tg`, `/push/jmap`, `/reconcile`, `/healthz`, `/ready`; three of the write paths first pass the `SAF-AUTH-*` portal authentication (fail-closed, §7.3), `/healthz`, `/ready` For the public probe (`SAF-PROBE-PUBLIC`); `/ready` has done end-to-end detection (configuration + Redis + outbound read-only detection JMAP session `GET` and TG `getMe`, each 3s, parallel, about 3s at worst).
  4. Start **Redis Streams worker** (background task) to consume Push events → `Email/changes` → Notification → Send to TG → Push sinceState → XACK.
  5. **Does not hold any long connections and does not build self-timers** (`C-NO-LONG-CONN`); reconciliation is triggered by **external HTTPS Cron** `/reconcile` (`FLOW-RECONCILE`).
- Notification sending and command processing share `Arc<JmapService>`, and internal `tokio::sync::RwLock` protects the variable cache; cross-request status always falls to external Redis (`C-REDIS-ONLY-STATE`).
- **Multiple instances and load balancing (deployment form, `ARCH-LB-WORKER`)**: Since the state is all external Redis (`C-REDIS-ONLY-STATE`) and the delivery is idempotent (`MOD-DEDUP`), **the same image can be instantiated across multiple serverless platforms**, free Cloudflare Worker is used as the only entrance and failover (`C-LB-SINGLE-REG-URL`); `/reconcile` Use Redis to lock a single instance (`SAF-RECONCILE-LOCK`), and Streams to automatically amortize using the same consumer group (`MOD-STREAMS-GROUP`). See deployment.md §10 for details. **Field/channel logic does not need to be changed. **
- **Production redline (`C-NO-DB` / `C-NO-LOCAL-WRITE` / `C-LOG-STDOUT-ONLY` / `SAF-LOG-PURITY` / `C-NO-STATEFUL-RECOVERY`, see deployment.md §0 / §8.2)**:
  - **Production uses no database** (`C-NO-DB`): No SQLite/Postgres/MySQL/embedded database; Redis is the only production state store.
  - **Disable local file/directory writing** (`C-NO-LOCAL-WRITE`): no log files, no data files, no temporary cache, no local volumes mounted.
  - **Log only writes to stdout/stderr** (`C-LOG-STDOUT-ONLY`): The container/platform is responsible for collecting and placing disk; disable the file log backend.
  - **Log and Redis write content constraints** (`SAF-LOG-PURITY`): Only structured events, counts, timestamps, desensitized summaries; **Prohibited** key original text, JMAP email body, AI request/response, attachment content.
  - **It is prohibited to rely on in-process status for production recovery** (`C-NO-STATEFUL-RECOVERY`): Restart and continuation (deduplication, sinceState, Streams breakpoints, circuit breaker counts, sessions) are all implemented by external Redis + JMAP reconciliation; the in-process cache is only for performance optimization, and the loss must be safe and reentrant.
- Graceful exit: signal + `CancellationToken`, flush Redis Streams pending entries before exiting (**Do not write local files**, `C-NO-LOCAL-WRITE`).
- **Current status**: Configuration guidance, HTTP entry and write entry authentication completed (`R1`/`SAF-AUTH-*`, fail-closed); JMAP session, Email/changes, PushSubscription create/update, Redis Streams worker and `/reconcile` have been connected. `/reconcile` and the Push closed loop still need to complete end-to-end acceptance in the real Stalwart, Redis, and Telegram environments. You cannot claim that the production link has been verified based on local access control alone.

### 5.4 Data flow: new email push (critical path, FLOW-NEW-MAIL)
```
Stalwart JMAP Push → POST /push/jmap (StateChange{Email/EmailDelivery: new_state})
  → 校验 pushSubscriptionId + verificationCode → 幂等去重(MOD-DEDUP) → 入 Redis Streams(MOD-STREAMS) → 立即 2xx ACK
  → worker: XREADGROUP → 用 sinceState(存 Redis, MOD-SINCESTATE) 调 Email/changes → 取 created[] 的 id
  → read_email 封装 email_get（Subject, Preview, From, TextBody, BodyValues, Size, ReceivedAt, HasAttachment）→ EmailMetadata
  → notify::core 组装领域 Notification（仅元数据 + 行内按钮意图，绝不含正文）
  → channel::telegram::TelegramClient::send_notification(chat, notification)（内部渲染文本后经 send_text 发出）
  → 更新 sinceState（写外部 Redis）→ XACK
  → 若 Redis 丢失：由外部 Cron 对账(FLOW-RECONCILE) 重建游标并补发
```
> Key: `Email/changes` lists the new email id in `created` to avoid full `email_query`; `sinceState` is stored in external Redis (`C-REDIS-ONLY-STATE`). When Redis is lost, it will be reconstructed from JMAP by reconciliation, and the **fact source is in JMAP**.
> Notifications only contain sender/subject/time (+number of attachments), the text never appears in the notification (see `docs/charter.md §3` Security Boundary `SAF-NOTIFY-META`).
> No long connection: Push is a short request callback, reconciliation is triggered by **external HTTPS Cron** (`C-NO-LONG-CONN`), and is not a self-sustaining timer in the container.

### 5.5 Data flow: command `/read <seq>` (long mail branch)
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

### 5.6 Conversation FSM

There is currently **no FSM**. `parse_intent` in `src/worker.rs` parses the command line into an `Intent`; the AI consent state is a single boolean plus an expiry in Redis (key and TTL in the AI consent section of `docs/reference.md`), not a multi-step confirmation state.

The invariant that still holds: an AI summary requires the user's explicit consent first — **without a consent state the LLM is never called**. Once consent is live the summary pulls the full email body and sends it to the LLM (`read_email` requests `TextBody` + `BodyValues` via `fetch_text_body_values(true)`, see `src/domain/jmap/client.rs`), but the body is not sent back to Telegram — outbound sends only the summary, or falls back to the first 300 characters of the body if the LLM call fails (`fallback` in `src/worker.rs`). See also the doc comment on `MetadataWorker`: "body text never reaches Telegram".

The 5-state FSM that was designed (`Idle` / `AwaitClarify` / `AwaitConfirm` / `Analyzing` / `AwaitFallback`), with its state-transition table and channel-neutrality notes, is in `docs/retired.md`.

### 5.7 Email search (`/search`, `bfe0fd8` implemented)

`/search <keyword>`, plus the Chinese prefixes `搜索` / `查找` / `检索` (**prefix match only**, so "help me search…" is not hijacked into a search), route to `Intent::Search`, which goes through `JmapService::search_emails(account_id, query, limit)` with `email_query` (`Filter::text`) for the IDs and `SearchSnippet/get` for highlighted `subject` / `preview`.

Key boundaries:
- **Snippet degradation**: `SearchSnippet/get` being unsupported (`unknownMethod`) or timing out with empty snippets instead of an error degrades rendering to a plain ID list (`（高亮片段暂不可用，以下为匹配的邮件 ID）`); no snippets are fabricated.
- **Plain text outbound**: `<mark>` tags are stripped by `strip_mark_tags` (case-insensitive, unclosed tags discarded) and then decoded in one pass by `unescape_html_entities`; tags are stripped **before** entities are decoded so that a literal `<mark>` in the mailbox body (escaped by the server as `<mark>`) is not restored to a real tag and then accidentally deleted. `subject` and `preview` are capped at 120 and 160 characters (`truncate_chars`, the same helper as text truncation).
- **Failed mapping**: JMAP side failed → `SearchReply::Retry` → `process_telegram` returns `Err(())` → coordinator 503 + `Retry-After` retry (consistent with `/reconcile`, other JMAP paths), **no panic, no error stack returned to user**.
- **Empty query**: `/search` with no keywords emits a guidance prompt (`请提供搜索关键词，例如：/search 发票`) and makes no request. No matches returns `没有找到匹配「<query>」的邮件。`.
- **Each hit carries `email_id`**, so `/summary <email_id>` jumps straight into the AI summary flow.
- **Body-level highlighting is not possible in locked versions**: jmap-client `0.4.2`’s `SearchSnippet` only models `emailId`/`subject`/`preview`, without `bodyProperties`/`parts`. Per RFC 8621 §5 the body-level `body: String[Id]` is not modeled by the crate and, with no `deny_unknown_fields`, is silently dropped by serde. Per-part body highlighting would require bypassing the crate and calling JMAP directly, which is not implemented.
- **No new environment variable**; when the configuration is missing, `/search` reports a friendly error.

---

## 6. Module division

Actual project structure (all code anchors in this document refer to this structure; see `docs/retired.md` for the historical target module splitting plan):
```
message-weave/
├── Cargo.toml                    # jmap-client 0.4.2 / redis 0.27 / reqwest 0.13；不含 teloxide
├── .env.example                  # 仅 2 个必填项的占位样例（REDIS_URL / CONFIG_ENCRYPTION_KEY），不含业务配置
├── .gitignore
├── Dockerfile                    # 仅本地开发用；生产不执行（C-DEBIAN-SLIM，边界见 deployment.md §3）
├── hoststack.yaml                # 生产部署真源（runtime/build/start/healthCheck），非负载均衡器清单，见 §5.1
├── src/
│   ├── main.rs                   # tokio main：读 PORT/REDIS_URL/CONFIG_ENCRYPTION_KEY 后启动 webhook HTTP 入口（单入口，无 CLI 子命令）
│   ├── config.rs                 # Redis 业务配置反序列化（serde → BusinessConfigWire → Config）；CONFIG_ENCRYPTION_KEY 解析
│   ├── error.rs                  # BotError 统一错误（见 §8.1）
│   ├── domain.rs                 # 领域层根（渠道中立，见 §5.2）
│   ├── domain/jmap.rs            # JmapBackend trait + MockBackend（契约测试不联网）
│   ├── domain/jmap/client.rs     # 包装 jmap_client::Client（真实只读 adapter，MOD-JMAP-CLIENT）
│   ├── state.rs                  # Redis 读写封装：配置/开关/会话/TTL 键（C-REDIS-ONLY-STATE）
│   ├── channel.rs                # 渠道层：TelegramClient（reqwest 自研出站）；领域类型在 src/domain.rs
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

Testing: **No integrated test directory**, and no stand-alone util tool module. All Rust tests are `#[cfg(test)]` unit tests within each module; the only test on the front end is `web/config.test.mjs`.

### 6.1 Module Responsibility Matrix

> **Current dependencies**: `jmap-client 0.4.2`, `redis 0.27`, `reqwest 0.13` **have been introduced** (`ARCH-DEPS-STAGE1` / `ARCH-DEPS-STAGE4`, the version is subject to `Cargo.toml`). `teloxide` has been evaluated to be **not introduced**, and the Telegram channel is self-developed using `reqwest` (§4, see `docs/retired.md` for details). Each row in the table below is a real module.

| Modules | Dependencies | Output | Testability | Current Status |
|---|---|---|---|---|
| `config` | secrecy; ring (`session_digest` SHA-256) | `Config` structure (7 fields, all with real readers) | Pure function, easy to test | Implemented (Redis business configuration serde deserialization + `validate_nonblank` fail-closed; startup env is read directly by `main.rs`, no figment/TOML) |
| `error` | — | `BotError` (§8.1, 4 variants) | Single test | Implemented |
| `domain` + `domain::jmap::client` | **jmap-client 0.4.2** | JMAP read-only semantics; `client` = real read-only adapter (`MOD-JMAP-CLIENT`) | mock JMAP response + `#[ignore]` real machine test | Code has been implemented, waiting for real Stalwart end-to-end verification (`cargo test -- --ignored jmap::`) |
| `state` | redis 0.27 | Redis read and write: config/switch/session/TTL key (`C-REDIS-ONLY-STATE`) | Redis mock | implemented |
| `channel` | reqwest 0.13; jmap-client | `TelegramClient` (`send_text` / `send_notification`), dual channels of email and TG (event→field `Notification`→rendering) | mock HTTP | Implemented (`src/channel.rs`, self-developed by `reqwest`, no third-party Bot framework) |
| `worker` | — | `parse_intent` → `Intent` (6 types, including `Search(query)`: `/search` + `SearchSnippet/get` highlight rendering), command routing and AI authorization determination | Table-driven pure single test | Implemented |
| `ai` | reqwest 0.13 | `LlmClient` / `summarize` | mock OpenAI compatible endpoint | implemented |
| `notify` | axum; redis | HTTP authentication, global switch, `/push/jmap` enqueue, `Email/changes` reconciliation and cursor submission | webhook verification failure path coverage 403 | The real reconciliation path has been covered; the real Stalwart environment is still required for end-to-end acceptance |
| `web` | axum | `/config` static page + `include_str!` embedding + CSP | Front-end covered by `web/config.test.mjs` | Implemented |
| `debug` | reqwest 0.13; redis 0.27 | Remote diagnostic surface `/debug/*` (`src/debug.rs`): 6 read-only probes `/debug/ping`, `/debug/config`, `/debug/redis`, `/debug/jmap`, `/debug/telegram`, `/debug/worker` + `POST /debug/notify` (sends a test message through the production outbound path, no independent implementation) | Only `DEBUG_ENABLED` (or `--debug`) + `DEBUG_TOKEN` mounts the route when both factors are present; the module itself is compiled unconditionally | Implemented, does not enter the Worker whitelist (`SAF-DEBUG-ORIGIN-ONLY`) |

> Note: The "Current Status" column is **module level** (the module has been implemented), which does not mean that the behavior is complete. Behavioral gaps are not included in this list: `/search`, Telegram 429 backoff, and multi-instance re-delivery windows have all been implemented. `SAF-DEBUG-ALLOWLIST` (`POST /debug/notify` does not intercept any `chat_id` when the list is empty) has never been counted as a behavioral gap and was moved from `docs/opengaps.md` on 2026-09-29: its exposure in production deployments is provided by `SAF-DEBUG-GATE` two-factor mount with Worker routing safelist Hold on, **the two doors are sufficient on their own**, and there is no need for a third layer of lists, so there is no gap. This behavior and residual risks (direct connection to the backend origin can bypass both layers) are described in `docs/deployment.md` §2.1 and `docs/design.md` §7.6. The `/search` path of the `worker` module has been implemented with `bfe0fd8`.

---

## 7. Configuration and Security

### 7.1 Historical configuration reading (migrated to Redis)

> **Migration Target (`C-REDIS-ONLY-STATE`)**: Only `REDIS_URL` is retained for the production boot environment. Empty Redis
> First-time configuration must be done through a one-time bootstrap/admin session authenticated by `CONFIG_ENCRYPTION_KEY`;
> Unauthorized write access must not be provided. This key is only used for constant time comparison and does not echo, record, or write business configuration;
> The Redis ACL password is only responsible for the Redis connection itself, not any HTTP authentication credentials. After bootstrap is successful
> The administrator session hash and TTL are saved in Redis and can be restored after restarting; the business token is loaded from Redis at startup.
> Key GET will never be echoed; after bootstrap/administrator PUT is successful, the client will be built and replaced atomically, and subsequent requests will be immediate.
> Use new configuration, keep old instance on failure. This migration replaces the following stage 0 environment variable list. The stage 0 list is only for historical purposes.
> Compatibility instructions.

> **Actual implementation** (`ARCH-CONFIG-ENV`): process startup read-only **2** process-level credentials plus listening port (`src/main.rs:45` / `:54` / `:63`,
> Add optional `DEBUG_TOKEN` in `:107`), directly `std::env::var`, **do not use figment, do not use TOML/configuration file**.
> **Business configuration is no longer read from environment variables**: The fields in the following table are all loaded by the `config` layer using serde deserialization through the Redis business configuration.
> And do fail-closed verification through `validate_business_wire` / `validate_nonblank`.
> Historically existing `Config::from_env()` full env parsing path **removed** (see `docs/retired.md` for registration),
> Therefore **the current code reads zero variable names in the following table**; the `RUN_MODE` variable itself has also been deleted entirely (the identifier is no longer in the code),
> `webhook` and `reconcile` already share the same set of routing tables, `NG-SERVER-MODE` is retained as a design non-target in `docs/charter.md` §8.

> ⚠️ **The following table is the complete variable table of business configuration fields** (the variable names are named after the original environment variables to facilitate comparison with the deployment document, but there is no longer any reading method in the code**). **The current production deployment only requires `REDIS_URL` + `CONFIG_ENCRYPTION_KEY` two startup variables** (`src/main.rs`); all other business fields are written by `PUT /api/business-config` through Redis business configuration (`C-REDIS-ONLY-STATE`). "Required" in the table refers to the fail-closed constraint (`validate_nonblank`, blank means rejection) of **Redis business configuration**, which is no longer required for the environment variable boot path.

| Variable | Required | Default | Description |
|---|---|---|---|
| `PORT` | No | `8080` | Single listening port (`C-NO-TCP-EXPOSE`) |
| `BOT_TOKEN` | Yes | — | Telegram Bot Token(`SecretString`) |
| `TG_WEBHOOK_SECRET` | Yes | — | `/webhook/tg` Authentication: Request header `X-Telegram-Bot-Api-Secret-Token` (`SAF-AUTH-TG-WEBHOOK`; `SecretString`) |
| `CHAT_ALLOWLIST` | Yes | — | Comma separated integer chat ids; **Whitelist Hard Constraint** (`SAF-CHAT-ALLOWLIST`) |
| `JMAP_SESSION_URL` | Yes | — | Stalwart JMAP session URL (`REQ-JMAP-SESSION-URL`): You can fill in the **service base address** `https://host[:port]` or **complete** `…/.well-known/jmap`; the code is normalized to origin/base and then submitted to `jmap-client` (it is automatically appended `.well-known/jmap`), **paths must not be repeated**. HTTPS only, **URL embedded credentials prohibited** (`SAF-JMAP-URL`) |
| `JMAP_USERNAME` | Yes | — | Stalwart Account (Email) |
| `JMAP_PASSWORD` | Yes | — | **App Password**(`C-AUTH-APP-BASIC`; `SecretString`) |
| Push verification | No | — | Dynamically generated by Stalwart; the backend automatically writes back through `PushSubscription/set` and saves the short-term verification status in Redis |
| `REDIS_URL` | Yes | — | External Redis (User-Managed + AOF, `C-REDIS-MANAGED-AOF`; `SecretString`) |
| `RECONCILE_TOKEN` | Yes | — | `/reconcile` Authentication: `Authorization: Bearer` (`SAF-AUTH-RECONCILE`; `SecretString`). `/reconcile` route is always mounted, so **required** |
| `ACCOUNT_ID` | No | Empty | Single account (`REQ-SINGLE-ACCOUNT`); if left blank, the session main account will be used |
| `LLM_ENABLED` | No | `false` | Do not verify `LLM_*` on shutdown (`REQ-LLM-OPENAI-COMPAT`) |
| `LLM_ALLOW_NET` | No | `false` | AI outgoing network switch (`REQ-AI-EXTERNAL-CONSENT`) |
| `LLM_API_KEY` / `LLM_BASE_URL` / `LLM_MODEL` | Required when `LLM_ENABLED=true` | — | OpenAI-compatible |
| `LLM_MAX_RETRIES` | No | — | Fuse threshold (`REQ-AI-FUSE`). **Non** environment variables: managed by Redis running parameters (`OutboundConfig.max_retries`, fall back to default, see `docs/reference.md` §6.1) |
| `LLM_SUMMARY_TARGET_CHARS` | No (**constant**) | `300` | Summary target word count (`REQ-LONG-EMAIL`). **Non** configurable items: The `config` layer is hard-coded to 300 in the two constructors and does not enter the business configuration wire |

> Currently `config.rs` provides the above fields (including `TelegramConfig` / `JmapConfig` / `LlmConfig`) from Redis business configuration; process-level startup variables are read directly by `src/main.rs` without going through the `config` layer. The introduction of TOML/figment in the future will require separate decisions; the current documentation does not assume that the configuration file exists.
> **The entrance authentication key (`TG_WEBHOOK_SECRET`/`RECONCILE_TOKEN`) must be valid after the business configuration is completed** (fail-closed, `SAF-AUTH-*`). The verification code of JMAP Push is dynamically generated by Stalwart after the subscription is created and does not belong to the business configuration. When the startup boot variables are missing, the service enters configuration boot mode and remains accessible to SPA; business success will not be faked due to lack of startup variables, nor will entrance authentication be bypassed.
> **JMAP session URL normalization (`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`)**: `JMAP_SESSION_URL` accepts both the **service base address** (`https://mail.example.com`) and the **full session URL** (`https://mail.example.com/.well-known/jmap`). The code is unified and normalized to origin/base before calling `jmap_client::Client::connect` **——`jmap-client` will automatically append `/.well-known/jmap`, so **there will be no duplicate paths** (such as `…/.well-known/jmap/.well-known/jmap`). Constraints: **HTTPS only** (http denied); **Disallow URL embedded username/password** (credentials only via `JMAP_USERNAME`/`JMAP_PASSWORD`, `C-AUTH-APP-BASIC`); deny dangerous queries. **Redirect whitelist**: `jmap-client` **denies all redirections** by default, and Stalwart will jump `/.well-known/jmap` 307 to `/jmap/session`, so the origin host itself in the **configuration** is added to the trust list when connecting, and other hosts rewritten by the server are still rejected (`follow_redirects([trusted_redirect_host(base)])`). Known upstream risks: Stalwart's 307 `Location` will write `user:pass` into the URL, and the client will return the URL with credentials - so the transmission error string will not be logged additionally. (Status: D-G1-1 **Code implemented and verified with `cargo test -- --ignored jmap::` with a real Stalwart instance**.)

### 7.2 Key Management
- **It is prohibited** to write token/password into the warehouse or any configuration file (this project configuration does not enter the warehouse, `ARCH-CONFIG-ENV`).
- Only 2 process-level variables are read during startup (`REDIS_URL`, `CONFIG_ENCRYPTION_KEY`, directly `std::env::var` from `src/main.rs`; if any one is missing, it will be downgraded to read-only setup mode); the business configuration is deserialized from Redis through the `config` layer (**None** `${VAR}` interpolation/figment/config files, `ARCH-CONFIG-ENV`).
- Use `secrecy::SecretString` to wrap it during runtime, and `Debug` to implement `***`.
- Log filtering: `tracing` field layer shielding `Authorization`/`password`/`token`.
- For the runtime secret injection method (env / `*_FILE` / orchestrator secret), see deployment.md (`C-NO-SECRET-IN-IMAGE`).

### 7.3 Access control
- **Entry authentication (hard constraints `SAF-AUTH-RECONCILE`/`SAF-AUTH-TG-WEBHOOK`/`SAF-AUTH-JMAP-PUSH`)**: The three write paths must be authenticated first, **fail-closed**——
  - `/reconcile`: Request header `Authorization: Bearer <RECONCILE_TOKEN>`;
  - `/webhook/tg`: Request header `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET`;
  - `/push/jmap`: Verify Redis short-term status according to `pushSubscriptionId`; StateChange is released only after the first verification is successfully written back by JMAP `PushSubscription/set`.
  The comparison uses the **constant time** algorithm (`subtle`, anti-timing side channel); verification failure will always be `401` and **will not produce any side effects/status changes** before the authentication is passed. After the business configuration is completed, the credentials for Webhook, Push, Reconcile and Push registration interfaces must be valid; when the startup boot variable is missing, the configuration boot mode will be entered and authentication will not be bypassed (§7.1).
- **Health probe (`SAF-PROBE-PUBLIC`)**: `/healthz` (`ARCH-HEALTHZ`) and `/ready` are **public probes** - no authentication, only returns health status, **does not contain any sensitive information** (does not echo configuration/key/internal error details).
  - `/healthz` = liveness (process survival), long-term stable semantics.
  - `/ready` does end-to-end probing: configuration integrity + Redis reachability + outbound read-only probing (`GET {jmap_origin}/.well-known/jmap`, Basic authentication with configuration; `GET https://api.telegram.org/bot<token>/getMe`; each `PROBE_TIMEOUT` = 3000ms, **Parallel** (`tokio::join!`), worst-case scenario 3s); all four pass `200` and readiness report (`{"status":"ready","configured":...,"jmap":...,"telegram":...}`, where `jmap`/`telegram` is the real probe result), any one fails `503` (standard error envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`). The probe is read-only, read-only configuration status, and will not trigger business side effects such as email synchronization, nor will it echo token or third-party response content; `refresh_business_config` only reads Redis, and has no state to write. Therefore `/ready` requires the outbound egress to the JMAP host and `api.telegram.org:443` to be reachable (if the egress requires a proxy, `/ready` is not available, see deployment.md).
- **chat whitelist (hard constraint `SAF-CHAT-ALLOWLIST`)**: `CHAT_ALLOWLIST` is a **required** configuration; any inbound event (action triggered by TG command/callback) must verify `chat.id ∈ CHAT_ALLOWLIST` before **making any JMAP call, AI call or status change**. If it is not in the whitelist, **directly reject and terminate** (prevent token can be called by anyone after being leaked). Phase 0 has completed the `CHAT_ALLOWLIST` parsing skeleton; the forced rejection logic has been implemented with Telegram channel access (`telegram_webhook` of `src/notify.rs` verifies the whitelist before any JMAP/AI/status operation, and terminates if rejected).
- **Command Minimization**: Only necessary commands are exposed; writing operations such as sending emails must be confirmed twice (sending emails is not currently implemented, see §10.3).
- **Rate**: No local token bucket is built on the outbound side; Telegram's outbound sending is retried according to the Redis operating parameter `max_retries` (see `docs/reference.md` §6.1 for default values and hard upper limits); currently only Redis current limiting is done for Push verification code writing (`ratelimit:push-verify:*`). 429 under the 30 msg/s limit of the Telegram server **Automatic backoff in `parameters.retry_after` seconds** (`channel.rs`, `f4cae00`): `retry_after_ms` is truncated to the 60s budget limit after parsing, and the exponential backoff is used when this field is missing or non-numeric `backoff_delay_ms` (starting from 250ms, capped 4s), the overall retry budget is 60s. There is still no local token bucket current limit - if the budget is exceeded, a failure will be returned directly and the upstream will try again.

### 7.3.1 Configuration Management API
- `GET /` provides a SPA embedded in the Rust binary; `/assets/config.js` and `/assets/styles.css` provide page resources. The service does not read or write local files while running (`C-NO-LOCAL-WRITE`).
- `GET /api/status` publicly returns `{ "ready": boolean, "mode": "configured" | "configuration-setup", "missing": string[], "version": string }`, which only lists the missing environment variable names. When `REDIS_URL` or `CONFIG_ENCRYPTION_KEY` is missing, SPA only displays the configuration boot status and missing variables; the management session authorization area is displayed only after the service status is confirmed to be ready=true. `version` is the `BUILD_VERSION` (`<git-sha or nogit>+<UTC build time>`) baked by `build.rs` during compilation. The SPA no longer renders it, so the API is the only way to confirm a deployment landed.
- `POST /api/admin/session` accepts `Authorization: Bearer <CONFIG_ENCRYPTION_KEY>` and returns `{ "session": "<opaque>", "expires_in": 1800 }`; the admin session only saves the digest in Redis and expires after 1,800 seconds. `POST /api/admin/session/revoke` revokes the current session and returns `204` successfully.
- The admin page only saves the session in JavaScript memory. Request settings `credentials: omit`, `cache: no-store`, without using cookies, localStorage or sessionStorage. The admin API accepts a valid admin session; the compatibility path also accepts `WORKER_TOKEN`. Worker transparently transmits the authentication header (`SAF-LB-PASSTHRU`) as it is.
- `GET /api/config` and `PUT /api/config` only read and write non-sensitive running parameters:
  ```json
  {
    "jmap_timeout_ms": 15000,
    "telegram_timeout_ms": 10000,
    "llm_timeout_ms": 30000,
    "max_retries": 3
  }
  ```
- The three timeout units are all milliseconds; the default value, value range and hard upper limit of `max_retries` are only authoritative with `docs/reference.md` §6.1 (the table below and the example response body above are only for illustration, and the values are not repeated). `max_retries` represents the maximum number of retries beyond the first request. When there is no configuration in Redis, GET returns the default value object; PUT successfully returns the same object after saving and persists to Redis (`C-REDIS-ONLY-STATE`).
- `GET /api/business-config` reads back saved configuration for management SPA pre-populated form: returns `{ "configured", "revision", "values", "secrets_present" }`, where `values` is the value of 10 non-key fields and `secrets_present` is 6 Boolean existence of a key (`bot_token`, `jmap_password`, `telegram_webhook_secret`, `reconcile_token`, `worker_token`, `llm_api_key`) - **Never return the key value itself** (`SAF-NO-SECRET-ECHO`). Not yet saved, staleness returns `200` + `configured: false`, `revision: 0`, empty `values` and an existence boolean of all false. SPA therefore has only one code path and does not need to distinguish between first-time configuration and modified configuration.
- `PUT /api/business-config` accepts `BusinessConfigPatch` (complete or partial), **only replaces the fields that appear in the submission body, and the rest inherits the stored values** (`BusinessConfigPatch::apply`); Push verification does not belong to Wire, is dynamically generated by Stalwart and written back by the backend. Verification is the only write gate, and the verification object is the merged configuration rather than the commit itself: patches are never verified individually, and existing complete wires are never discarded. `validate_business_wire` runs first, and returns the real `422` when rejected; if the merge result is legal, the configuration **must** be persisted (the merged Wire is persisted, not the request body), and then the client is built and the worker is switched to run. Therefore, when the dependency is not connected, `200` + `{ "persisted": true, "runtime_applied": false, "warnings": [{"component", "step", "detail"}] }` is returned instead of `503` - the configuration has been saved and the fault is reported truthfully instead of being silently swallowed by an unreachable JMAP. `runtime_applied` is only `true` when the reload submission is successful; in both cases, the `x-business-config-revision` header is returned; the request body can carry `revision` (that is, the `u64` returned by `GET`) as a control field - it does not participate in `apply`, does not enter Wire, and is not used as a saved setting. It is only used to determine whether this submission is based on the latest version. See the next article for the semantics of `409`. The response never echoes configuration or keys. The key is "replaced only after submission": the **omitted** key inherits the existing value, so the form backfilled by `GET` only needs to submit the fields to be changed; but the backend does not distinguish between "unsubmitted" and "submitted empty string", and explicitly submitting `""` will actually clear the key - "leave blank and keep" is a client contract implemented by SPA by discarding blank key fields before submission, not a server-side guarantee. `chat_allowlist` must be a non-empty list, empty arrays will be rejected by `validate_business_wire` (422). Incremental semantics only hold after **existing configuration**: if there is no existing configuration, there is no fallback value, and the patch must come with all required fields, otherwise `422` will be returned (the entry created from scratch is `POST /api/bootstrap`).
- `POST /api/business-config/preflight` performs exactly the same validation and client build on the submission body, returning component-by-component conclusions, **without writing, without touching the running worker**: `{ "persisted": false, "validation": { "ok", "errors" }, "components": { "jmap": {...}, "llm": ... } }`. Different from PUT, its submission body must be **complete** `BusinessConfigWire` (no patch merging), so SPA must first merge the value read back by `GET` into the local form before pre-verification. `components` is `null` when the verification has failed (rejected Wire does not need to probe the client again); when `llm_enabled`/`llm_allow_net` is false, `llm` is `null` instead of false `ok`. Authentication is consistent with the management session gate (`config_authorized`) and is deliberately not hung after `DEBUG_TOKEN`.
- `POST /api/bootstrap` also accepts **complete** `BusinessConfigWire` (without patch merging), and shares the above semantics of "separation of persistence and error reporting": verification failure is `422`, and build failure after persistence is still `200` + warnings. Because it does not merge, it is the only patch path that can create business configurations from scratch (PUT returns `422` when the existing configuration is missing and the patch is incomplete, see the previous article).
- The running parameter errors of `PUT /api/config` use `401` (Unauthorized), `400` (JSON Invalid), `422` (Scope Error), `503` (Redis Unavailable); when GET read fails, `503` also indicates that Redis is unavailable. Business PUT uses `401` (unauthorized), `400` (JSON is invalid), `422` (the patch cannot be parsed, the saved configuration cannot be decrypted, the merge result verification fails, and the patch is incomplete when saving for the first time), `409` (the submitted `revision` has expired), `500` (merge result serialization failed) and `503` (Redis is unreachable, writing failed or reload submission failed); business GET uses `401`, `400`, `422` (the saved configuration cannot be decrypted) and `503` (Redis is unreachable) - note that `Invalid` is 422, not 503, and `Unreachable` is 503. **Business PUT has `409`**: `revision` is a **control field** in the request body** (not a header) - SPA records `revision` when reading the configuration, and each subsequent incremental submission will bring back the same value; the saved revision and the requested revision are not equal and return `409 conflict`, neither writing nor incrementing the revision, SPA receives `409` The saved value will be read back again and allowed to be changed by the operator, so the old page will not silently overwrite the new save. The request body without `revision` maintains last-write-wins, which is compatible with callers that never read revisions. This comparison is **compare first and then write**, and is non-atomic: if two requests compete at the same moment, both will pass, and the later one will overwrite the first one. The cost of optimistic locking without Lua scripts is this residual window, and the patch only takes the fields actually changed by the operator, so the window impact is limited. The remaining occurrences of `409 conflict` are the competition of three locks (`reconcile` and `acquire_lock` of `register_push` / `register_worker_token` return false), which have nothing to do with business configuration writing. Dependent client build failure does not return an error status code, but `200` + warnings (see above). The remaining errors do not echo sensitive data. The management page displays a `503` status when Redis/session initialization is not completed; running parameters are disabled from being saved before re-reading successfully (`C-REDIS-ONLY-STATE`). When LLM is enabled, the business configuration requires an HTTPS Base URL, a non-empty API key, and a model name.

### 7.4 TLS (code/dependent behavior)
- The TLS provider is determined by the dependent feature: **jmap-client 0.4.2 contains `aws_lc_rs` by default (and introduces `rustls`)**, and `default-features = true` is not "rustls default". **The actual `default-features = false, features = ["async","rustls"]`** of this project (`aws_lc_rs`/`websockets` is not enabled), and will ultimately be subject to `Cargo.toml`.
- Certificate verification is **mandatory by default** and can be turned off only for testing.
- For operation and maintenance details such as the system CA trust store in the image, internal CA injection, inbound webhook TLS termination, etc., see deployment.md.

### 7.5 Production red line: no database / no local writing / standard output log (deployment.md §0/§8.2)

This section summarizes the absolute red lines across code and deployment that you must not introduce any "seemingly convenient" local state into your code:

| Redline ID | Meaning | Violation Example (Prohibited) |
|---|---|---|
| `C-NO-DB` | Production does not use any database; Redis is the only production state storage | Add SQLite deduplication table, add Postgres session |
| `C-NO-LOCAL-WRITE` | Disable writing of local files/directories; do not mount local volumes | Write logs to local files, write temporary directories, and place session cursors into local status files |
| `C-LOG-STDOUT-ONLY` | The log only writes stdout/stderr; the platform is responsible for collection | Introducing `tracing-appender`, `rolling-file`, `FileAppender` |
| `SAF-LOG-PURITY` | Log and Redis writing content is limited to structured events, counts, timestamps, desensitized summaries | Log printing JMAP text, AI prompt/completion, key original text, attachment content |
| `C-NO-STATEFUL-RECOVERY` | Production recovery does not rely on in-process state | Use `static Mutex<HashSet>` to store dedup and use memory LRU to store sinceState as the only recovery source |

**Verification**: No local path constants / `std::fs` non-test calls / file log backends appear in the static inspection code; during runtime, the container `/proc/mounts` and `docker inspect` are used to confirm that there is no local volume mounting; see deployment.md §8.2 for the deployment checklist.

### 7.6 Why is the remote joint debugging interface absolutely closed by default?

The only reason for this surface to exist is to shorten the production troubleshooting path: when you cannot log in to the container and can only rely on external requests to observe the system, someone needs to be able to detect the JMAP/Telegram connectivity and current business configuration without releasing the version. The price is that its entry softness must be higher than the three write paths - in addition to read-only detection, it also retains a **real outbound send**, and reuses the business whitelist instead of an independent whitelist. Therefore, the design choice is "closed by default" instead of "enabled by default and blocked by the load balancer": the process will only mount this set of routes (`SAF-DEBUG-GATE`) when "`DEBUG_ENABLED` is true or the command line has `--debug`"** and **a non-empty `DEBUG_TOKEN` is set; when the route is missing, the route does not exist at all in the router, and the request falls to axum general `404`, not "Exists but 401" - the latter would leak the route's existence. There is no third state: there is no "open without configuration" fallback, and there is no configuration item to set it to be enabled by default. `DEBUG_ENABLED` uses environment variables instead of startup commands so that the switch can be changed once from the platform console without releasing the version - the startup command remains static. Add another layer of location constraints: it is not in the load balancer's safe routing whitelist. Even if the backend is opened incorrectly, the platform entrance will be rejected by fail-closed. The only reachable path is to directly connect to the backend origin. These four things—two-factor mounting, 404 instead of 401, no default enabled fallback, and unreachable load balancer—together form the architectural decision of “exposure is zero by default.” See deployment.md §2.1 for the enablement method, per-endpoint status code, and deployment confirmation checklist.

---

## 8. Error handling and observability

### 8.1 Unified error enumeration

Current implementation (full text of `src/error.rs`, 4 variants):

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

The design target form once included six additional variants: `Jmap(#[from] jmap_client::Error)`, `Telegram(#[from] teloxide::errors::RequestError)`, `Storage(#[from] redis::RedisError)`, `RateLimited`, `Unauthorized(i64)`, `Llm(LlmErr)`; `Telegram` comes with `teloxide` They were abandoned without adoption, and the rest have not yet been implemented. For a complete list, see `docs/retired.md`.

### 8.2 Error → User Message Mapping
| Low-level errors | Bot behavior |
|---|---|
| `Transport` / `ServerUnavailable` / `RateLimit` | Exponential backoff retry (`util::retry`), exceeding the threshold gives the user "temporarily unavailable, try again later" |
| `Forbidden` / `Unauthorized chat` | Silent rejection; log alert |
| `Method(InvalidArguments)` | Reply to "Wrong parameter + correct usage" |
| `Set(creation/update/destroy)` | Reply to the specific record-level failure reason |
| `StateMismatch` | Reset sinceState → Fill in full once (anti-lost/anti-repair) |
| `Llm(*)` | See §12.7 (Break/Fallback/Logo) |

### 8.3 Observability
- `tracing` (direct dependency) + `tracing-subscriber` (fmt + EnvFilter).
- `tracing` (direct dependency) + `tracing-subscriber` (fmt + EnvFilter). **Currently only recording startup events**: `src/main.rs` 5 `info!`/`warn!` in total (missing startup variables, downgrade to setup mode, startup banner, debug endpoint enabled, JMAP service unavailable, downgrade × 2). Request-level events (Push callbacks, Reconcile pulls, channel pushes, LLM elapsed times) **not implemented yet** - there are no tracing calls in `src/` except `main.rs`, and no named spans (no `#[instrument]` / `span!`).
- Metrics (optional `metrics` crate): Number of Push callback arrivals, deduplication hit rate, Redis Streams backlog depth (pending), number of DLQ items, number of reconciliation items, JMAP request delay, push failure rate. **Currently not connected to any metrics backend** (`metrics` is not in `Cargo.toml`).
- Graceful exit: **Currently not implemented** - There is no signal processing in `src/` (no `tokio::signal` / `ctrl_c`), and there is no flush logic for Streams pending entries; the process is terminated when the container stops. Redis side `C-NO-STATEFUL-RECOVERY` ensures reconstruction from Redis after restart and does not rely on in-process state.
- **Reliability goals and strategies** (Streams ACK/retry, idempotent deduplication, Push retry, reconciliation recovery, indicators/alarms, **≥99.9% notification availability and boundaries**) see deployment.md §6.4/§6.5 (`NFR-NOTIFY-SLA`).

---

## 9. Test strategy

### 9.1 Levels

| Layers | Location | Content |
|---|---|---|
| Unit | `#[cfg(test)] mod tests` in `src/*.rs` | `config` parsing, `worker` command routing, `ai` HTTPS verification, `state` Redis encapsulation, `channel` outbound, `notify` verification and deduplication, `main` startup branch |
| Contract | `src/domain/jmap.rs` | `JmapBackend` trait + `MockBackend`: validate domain verbs with preset data, not connected to the Internet |
| Integration | `src/channel.rs` test module | Use in-process `tokio::net::TcpListener` to start mock HTTP service, verify the path, status code and retry of outbound requests |
| Real (manual) | `src/domain/jmap/client.rs` | Marked with `#[ignore]`, you need to explicitly configure the JMAP test server before running `cargo test -- --ignored` |

**No integration test directory**; the only test on the front end is `web/config.test.mjs` (Node native assertion).

### 9.2 mock strategy
- `JmapService` holds the trait `JmapBackend` (`list_emails` / `read_email` / `send_email` / `changes` etc.), the production implementation wrapper `jmap_client::Client` (`src/domain/jmap/client.rs`), and the test `MockBackend` (`src/domain/jmap.rs`).
- Telegram outbound does not introduce any framework mock: `src/channel.rs` starts an in-process `TcpListener`, uses `AtomicUsize` to record the number of requests, and verifies the URL, status code and timeout/retry behavior.
- `src/ai.rs` asserts HTTPS enforcement (`llm_requires_https`) by constructing the configuration of `LlmClient`; summary logic validates on mock OpenAI compatible endpoint.
- The decoupling of channels and domains relies on type constraints (channel SDK types do not appear in the public interface of `domain.rs`) and does not rely on mock channels.

### 9.3 Key invariant testing
- `sinceState` is stored in external Redis: resumes the transfer from the correct cursor after simulating restart; restores the cursor from JMAP through reconciliation (`reconcile`, `FLOW-RECONCILE`) after simulating Redis clearing.
- Notification deduplication: the same `email_id` will not be pushed repeatedly.
- Text escaping: Message text containing `<script>` is escaped in HTML mode.
- **Security boundary assertion (hard, see `docs/charter.md §3`)**:
  - VIEW/View original text path: mock AI endpoint zero request (LLM client not called);
  - New email notification: There is no body content in the message (assertion leak);
  - Long emails: if the text is > 4000 characters, the full text will not be sent;
  - AI call prefix: LLM zero call before confirmation;
  - The analysis results are not saved to disk: no new disk/Redis writing path is added after processing;
  - AI fails 3 times → circuit breaker confirmation → fallback with "AI unavailable" logo.
  - **Entry Authentication (`SAF-AUTH-*`)**: `/reconcile`, `/webhook/tg`, `/push/jmap` returns `401` under **missing or wrong** credentials, and asserts **no side effects** (no Redis writes, no JMAP/AI calls) when authentication fails; correct credentials are allowed.
  - **Health Probe (`SAF-PROBE-PUBLIC`)**: `/healthz` returns `200` for process alive; `/ready` returns `200/503` for readiness - checks configuration integrity + Redis reachability + outbound read-only probe (`GET {jmap_origin}/.well-known/jmap` with Basic authentication, `GET https://api.telegram.org/bot<token>/getMe`, 3s each, parallel, about 3s at worst), any failure will return the standard error envelope (`service_unavailable` + `Retry-After: 30`), all will return the readiness report JSON; the response body does not contain sensitive information. Uptime Kuma is monitored by status code (`/ready` expects 200) and is not affected by changes in the response body.
  - **Channel decoupling**: No channel SDK types appear in the public interfaces of domain modules (`src/domain.rs` / `src/ai.rs` / `src/worker.rs`); channel assembly is centralized in `src/channel.rs`.
  - **JMAP session URL (`REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`)**: both the base address and the complete `…/.well-known/jmap` are accepted and the normalized results are consistent**, the URL passed to `Client::connect` does not contain duplicate `/.well-known/jmap`**; `http://` Rejected; **Embedded credentials (`https://user:pass@host`) are rejected**; Dangerous queries are rejected; Redirects only trust the origin host in the configuration (Stalwart will 307 jump to `/jmap/session`).
  - **JMAP multi-part original text (`REQ-JMAP-RAW-MULTIPART`)**: `read_email` splices the parts with `part_id` and `bodyValue`" for the multi-part text in the order of `text_body`; constructing the "no available part" use case assertion returns **clear error** (non-empty string).

---

## 10. Phased implementation plan

> Prerequisite: `rustup` first installs the tool chain (stable). CI and release flows are in deployment.md.

### 10.0 Phase 0: Scaffolding and HTTPS entry skeleton (completed, `GATE-P0` passed)

> **Current Actual Boundaries**: Single-port axum portal provides `/webhook/tg`, `/push/jmap`, `/reconcile`, `/healthz`, `/ready`. The entry authentication of the three write paths has been fail-closed (`R1`/`SAF-AUTH-*`); `/reconcile` has used JMAP `Email/changes` paging, Redis `state:jmap:since` and Redis solo flight lock, and the cursor will be advanced only after all events are successfully queued; `/healthz` is for liveness, `/ready` does end-to-end detection (configuration + Redis + Outbound read-only detection, about 3s at worst), but does not cover real message delivery acceptance.

- **Actual dependencies** (Cargo.toml current, `ARCH-DEPS-STAGE0`/`ARCH-DEPS-STAGE1` + `ARCH-DEPS-STAGE4`): `axum 0.8` (single HTTPS entrance), `async-trait`, `secrecy`, `subtle` (constant time authentication comparison), `serde`, `serde_json`, `thiserror`, `tokio`, `tracing`, `tracing-subscriber`, `url` (`JMAP_SESSION_URL` normalized parsing), `jmap-client =0.4.2` (`default-features = false, features = ["async","rustls"]`), `redis 0.27` (Redis XPING/PING liveness detection, `ARCH-STATE-REDIS`), `reqwest 0.13` (JMAP/TG HTTP client); dev-dependencies: `tower 0.5` (routing test). `teloxide` is not introduced (`ARCH-DEPS-STAGE4`): Telegram channel is implemented in `src/channel.rs` using reqwest self-development.
- **Not yet introduced** (Documents must not claim to have been used): `teloxide` and other Telegram Bot frameworks (Telegram outbound is sent directly by `src/channel.rs` and `reqwest`, and the evaluation record is in `docs/retired.md`). **FIGMENT NOT USED**: Configured for manual `std::env` parsing (`ARCH-CONFIG-ENV`, §7.1).
- **Stage 1 dependency status** (`ARCH-DEPS-STAGE1`): `jmap-client` **Introduced** (currently `=0.4.2`, `default-features = false, features = ["async","rustls"]`, version and features **subject to `Cargo.toml`**). ⚠️ Its default features `["async","websockets","aws_lc_rs"]` **includes WebSocket stack**, so the default features must be turned off and `websockets` is not selected; JMAP only uses **HTTPS short requests** (Core/Mail) to comply with `C-NO-LONG-CONN` (no WS/SSE/long polling).
- `ACCOUNT_ID` **optional** (`REQ-SINGLE-ACCOUNT`): **leave blank → take the default/main account** of the JMAP session; explicit values are used after verification; multiple accounts = multiple bot instances.
- axum adopts **0.8** (`ARCH-AXUM-08`); if subsequent review decides to adjust the version, Cargo.toml will prevail and this section will be synchronized.
- Currently SSE/WebSocket/long polling/SQLite/local volumes (`C-NO-LONG-CONN`/`NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`) are not implemented; entrance authentication (`R1`/`SAF-AUTH-*`) is implemented as a fail-closed hard access control, and the Push, Streams worker and `/reconcile` business paths after authentication have been implemented.

**Phase 0 P0 Gate (`GATE-P0`) - Passed. It was 45 passed / 0 failed / 1 ignored when it was first frozen, and has continued to grow in subsequent rounds ④–⑥. The current baseline is 93 passed / 0 failed / 4 ignored (2026-09-28 retest; see the `TEST_BASELINE` line of `docs/charter.md` for the authoritative value). The following are the criteria at the time of stage 0, which are retained for historical records: **
1. `cargo fmt --check` passes (no format difference).
2. `cargo clippy --all-targets -- -D warnings` passes (zero warnings; crate-level `allow` is disabled).
3. `cargo test` passed (including routing/configuration minimum test).
4. The above three items are passed in the Debian `rust:1-slim-bookworm` container** (`C-DEBIAN-SLIM`).
5. The configuration is read as env-only (no figment/TOML); `SecretString` wraps the secret and `Debug` does not leak it; `JMAP_PASSWORD` security accessor is reserved.
6. `RUN_MODE` **does not** participate in route dispatch: `webhook` and `reconcile` share the same set of routing tables, `reconcile` is provided as an independent HTTP endpoint `POST /reconcile`. `RUN_MODE` only reads and verifies the value at startup, **currently does not affect any runtime behavior**, and leaves the differential side effects to be mounted here in subsequent stages (see the running mode description in §12.2).

> **Evolved criteria after phase 0 (current caliber, replacing the specific implementation descriptions in Articles 5 and 6 above)**:
> - Configuration reading is narrowed to **2 process-level variables** (`REDIS_URL`, `CONFIG_ENCRYPTION_KEY`) read directly by `src/main.rs`;
> Business configuration is changed to be deserialized by Redis through the `config` layer, `Config::from_env()` and all legacy env parsers have been deleted (§7.1).
> - `RUN_MODE` **The variable itself has been removed**, the identifier is no longer in the code (registered in `docs/retired.md`); section 6
> The conclusion of "not participating in route dispatch" is still true, but there is no need for variable carrying.
> - **Security accessor** `jmap_password()` for `JMAP_PASSWORD` has been removed; `jmap_password` is retained as a business configuration field name.
7. `.gitignore` exists (excluding `target/`, etc.; **do not** initialize git without authorization).
8. All new code has necessary comments referencing stable IDs; a single `.rs` ≤ 500 lines.

**Run Monitor (`GATE-UPTIME-KUMA`):**
- Use Uptime Kuma HTTP(s) Monitor to check `/healthz` (process survival) and `/ready` (configuration/Redis/upstream reachable readiness), respectively expecting HTTP 200; `/ready` takes about 3s at worst, and the probe timeout needs to be set to ≥10s; `/ready` returns `503` and standard error envelope (`service_unavailable` + `Retry-After: 30`), Uptime Kuma is still determined according to the status code and is not affected by changes in the response body.
- `/healthz` is pure liveness (unconditional `200`); `/ready` checks configuration integrity + Redis reachability + outbound read-only detection (JMAP session `GET`, TG `getMe`, each 3s, parallel, about 3s at worst), the probe is read-only, does not echo tokens or third-party responses, does not trigger business side effects, and the report body does not contain sensitive information; because `/ready` is the heaviest link (possibly 3s), the platform side should give priority to using load balancer-aggregated `/healthz-worker` as a survival detection to avoid high-frequency outbound requests.
- No Prometheus, Exporter or additional indicator ports are introduced; the real Stalwart/Telegram end-to-end link still needs to be jointly debugged separately.

**Production red line (throughout all stages)**:
- **No Database** (`C-NO-DB`): Redis is the only production state store; no SQLite/Postgres/MySQL/embedded databases are introduced.
- **No local file writing** (`C-NO-LOCAL-WRITE`): no log files, no data files, no temporary cache, no local volumes mounted.
- **Log only writes to stdout/stderr** (`C-LOG-STDOUT-ONLY`): The platform is responsible for collection; disable the file log backend.
- **Log/Redis Content Constraints** (`SAF-LOG-PURITY`): Only structured events, counts, timestamps, masked summaries; prohibited keys, JMAP email bodies, AI request/response, attachment content.
- **No in-process recovery** (`C-NO-STATEFUL-RECOVERY`): Redis + JMAP reconciliation (`FLOW-RECONCILE`) will always be used after restart and continuation; the in-process cache is only optimized for performance, and it can be re-entered safely if it is lost.

**Stage 1 Pushing the Boundary (`BOUND-STAGE1`):**
- Only enter phase 1 after passing all `GATE-P0` (JMAP read only).
- **R1 (real portal authentication) has been implemented** (`SAF-AUTH-RECONCILE`/`SAF-AUTH-TG-WEBHOOK`/`SAF-AUTH-JMAP-PUSH`): `/reconcile` verification `Authorization: Bearer RECONCILE_TOKEN`, `/webhook/tg` verification `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET`, `/push/jmap` Verify Redis short-term verification status by subscription ID; constant time comparison, failure `401` and no side effects. When the startup boot variable is missing, the service enters the configuration boot mode without forging business success; the corresponding entry is enabled after the business configuration is completed.
- **Still need to note**: Local CI access control does not mean that the real Stalwart, Redis, and Telegram links have been accepted; real end-to-end testing and external monitoring configuration should be completed before public network deployment.
- Phase 1 only does JMAP read-only (`list_folders`/`list_emails`/`read_email`), and does not introduce sending/push/AI.

### 10.1 Phase 1: JMAP Read Only (1.5d)

> Prerequisite: `GATE-P0` all passed (`BOUND-STAGE1`).

- Depends on `jmap-client 0.4.2` (**Introduced**, `ARCH-DEPS-STAGE1`; version/features is subject to `Cargo.toml`, **Disable WebSocket feature**); `config.rs` reuses the existing Redis business configuration skeleton (does not introduce an independent sample configuration file, see `ARCH-CONFIG-ENV`).
- `domain::jmap::client` = real read-only adapter (`MOD-JMAP-CLIENT`, **G1/D-G1-1 code implemented and verified with a real Stalwart instance by `cargo test --ignored jmap::` (session connection → folder enumeration → mailing list → read by id)**): implemented = wrapper `Client::connect` (Basic authentication `C-AUTH-APP-BASIC`), **URL normalization** (`JMAP_SESSION_URL` accepts the service base address or the complete `…/.well-known/jmap`, normalizes it to origin/base and then passes it in to avoid duplicate paths; only HTTPS, embedded credentials are prohibited and query, `REQ-JMAP-SESSION-URL`/`SAF-JMAP-URL`), **Redirect trust** (only trust the origin host in the configuration, Stalwart will 307 jump to `/jmap/session`, otherwise `jmap-client` will reject all redirects by default), **account selection**: `ACCOUNT_ID` Leave blank for session The main account is used after explicit value verification (`REQ-SINGLE-ACCOUNT`).
- `domain::jmap::JmapService::list_folders / list_emails / read_email` (query/get; `list_emails` contains `limit` boundary; `received_at` parsing; `read_email` multi-part splicing, see `REQ-JMAP-RAW-MULTIPART`).
- **R1 entrance authentication is ready**: `/reconcile`/`/webhook/tg`/`/push/jmap` and `/api/push/register` authentication (`SAF-AUTH-*`) has been implemented and passed the single test; subsequent changes must not relax or bypass the authentication.
- `#[ignore]` Real test (`GATE-G1-JMAP-READONLY`): driven by **environment variables**, run the command `cargo test -- --ignored jmap::` (`--ignored` is a libtest parameter and must be placed after `--`); only compile without execution using `cargo test --no-run` (compile the test target containing `#[ignore]`). **Clearly skipping when the environment is missing and not leaking any keys**; CI does not run real machine use cases by default, and mock tests continue to be retained. **Whether the real machine use case passes or not must be based on the actual `--ignored` run - it is not allowed to claim that "the real machine passes" without running it. **
- Acceptance: `cargo test` (mock) is all green; `cargo test -- --ignored jmap::` (when there is a real machine environment) can connect to the real server to list folders/emails and read the original text (it cannot be claimed to have passed before it is actually run).

### 10.2 Phase 2: Channel Adaptation (Completed)
- `src/channel.rs` implements `TelegramClient` (`send_text` / `send_notification`, `reqwest` self-developed); domain type `Notification` is in `src/domain.rs`.
- Telegram assembly uses `reqwest` to send directly to `https://api.telegram.org/bot{token}/sendMessage` (**teloxide is not introduced**; see `docs/retired.md` for evaluation records).
- Inbound: `POST /webhook/tg` Verify secret token → Press `update_id` to deduplicate (`dedup:tg:{update_id}`) → Enter Redis Streams → 2xx; Outbound is the only Bot API endpoint used by Telegram.
- Intent routing: `parse_intent` of `src/worker.rs` parses 6 types of intentions - `Help` / `Consent { ttl, label }` / `Summary(email_id)` / `Search(query)` / `Query` / `Unknown`, all using natural language trigger words (see AI authorization words in `docs/reference.md` Authorization state section). `/search` See §2.3 and §5.7 for details; Chinese search terms only perform **prefix matching** (`search/find/retrieval` + `/search`) to avoid overriding authorization and summary intent.
- Rendered inside adapter: field Notification → TG Markdown/HTML + escaping.
- Use external Redis for session storage (`C-REDIS-ONLY-STATE`; **not using SQLite**).
- Acceptance: In the form of local webhook, the test client can get a reply when sending a message; the domain module does not import any channel SDK type.

### 10.3 Phase 3: Sending Email + Status Management (**Not implemented**)
- `send_email` (draft + submission_set) **Currently not implemented**: `JmapBackend` only has read-only verbs.
- The sending process was originally planned to use a multi-step FSM to collect to/subject/body, which was never implemented (see the Dialogue FSM section of `docs/retired.md`).
- `/flag /unseen` Keyword flag **unimplemented**.
- Phase 3 is an **unimplemented scope**, not a gap: `docs/opengaps.md` only registers matters that are "not yet completed/yet to be verified". Future stages that are not scheduled for implementation are not included in this list.

### 10.4 Phase 3.5: LLM facade + rollback (1.5d, not executed)
Not implemented, see `docs/retired.md`.

### 10.5 Implemented capabilities: real-time push (Push callback + Streams worker + external Cron reconciliation)
`src/notify.rs` is a single tiled file of nearly 2000 lines, with no submodules (`notify::push_handler` / `notify::worker` / `notify::reconcile` / `mod_streams` / `mod_sincestate` do not exist in the following table):
- `register_push` (`POST /api/push/register`): Accepts an explicit HTTPS callback URL and creates a subscription; writes back `verificationCode` with JMAP `PushSubscription/set` when receiving a Stalwart push.
- `jmap_push` (`POST /push/jmap`): Verify subscription ID + verificationCode → remove duplicates → enter Redis Streams → 2xx.
- `worker` (`POST /worker`): Redis Streams consume → `Email/changes` increment → notify → advance sinceState → XACK.
- `reconcile` (`POST /reconcile`): External HTTPS Cron trigger (`FLOW-RECONCILE`); also serves as cursor reconstruction after Redis loss. **No CLI subcommand form**.
- sinceState writes external Redis (`state.rs`, `C-REDIS-ONLY-STATE`; not SQLite/file `NG-SQLITE-PERSIST`/`NG-LOCAL-VOLUME`).
- Target acceptance: Send a test email to Stalwart. After Push callback + worker, Telegram receives the push in ~ seconds; it will not be resent after restarting; it will be reissued by reconciliation after clearing Redis. The current code gate has passed, but end-to-end verification still needs to be completed in real Stalwart, Redis and Telegram environments, and the goals here cannot be regarded as verified facts.

### 10.5.1 Redis Streams event guarantee boundary (FLOW run semantics)

The Push event is consumed by Streams and delivered to Telegram. Its key path delivery semantics are as follows:

**The only authoritative source for keys and TTL is `docs/reference.md` (section Redis Keys and TTL). **The tables will not be repeated in this section - repeated tables
Will be modified: `push:subscription:{id}` was previously marked as 7d, and `push:registration:{...}` was marked as 360s registration solo lock.
Both are wrong, and the real 360s solo lock `lock:push-register:{sha256(callback_url)}` was missing the entire line. has now converged to
Single authority table, this section only retains four "whys" that affect design judgment:

- **Reconciliation lock TTL 300s, deliberately larger than the upper limit of 120s for a single page**, to avoid lock expiration during the lock period, causing the same account to enter reconciliation repeatedly
  (`REQ-RECONCILE-IDEMPOTENCY`, `SAF-RECONCILE-LOCK`).
- **push registration solo lock TTL 360s, deliberately larger than the upper limit of 300s for a single outbound request**, otherwise a slow registration will cause repeated requests
  (`SAF-AUTH-JMAP-PUSH`).
- **Reconciliation deduplication 24h is intentionally designed**: the same email will only be notified once within 24h (`MOD-DEDUP`, see 11.5 of this article for the scope).
- **If the configuration key is missing, it will be regarded as failed (fail-closed)** and the business path will not be entered.

**Delivery process** (`notify::worker`, one XREADGROUP batch ≤10 items)

1. Read batch → `process` one by one → XACK.
2. Write `delivery:inflight` (60s) for lease during processing; write `delivery:committed` (7d) after success.
3. Entries that crash within the `inflight` lease window are recycled and retried by another instance through XAUTOCLAIM (the idle threshold is derived from the running timeout configuration, see `docs/reference.md` §6.3 for the formula and upper and lower limits) - at most repeated and not lost.
4. `retry_or_dlq` (`state.rs`): If the retry count (`max_attempts.max(1)`) does not reach the upper limit, stay in the source stream and try again; when the upper limit is reached, use **a single Lua script** to atomically `INCR`+`XADD` (into DLQ) + `XACK` (source stream confirmation), ensuring that there will be no "source has ACKed but is neither in the source nor in the DLQ" gap.

**Alarm Boundary (current implementation, see §8.3 Monitoring Conventions)**

- Use Uptime Kuma HTTP(s) Monitor without introducing Prometheus/exporter.
- Indicators such as the number of DLQs, inflight backlog depth, and number of reconciliations have not yet been automatically reported (§8.3 is marked as optional). Current operation and maintenance needs to be viewed directly through Redis: `XLEN messageweave:dlq:*`, `XPENDING`, etc.
- If you need automated alerts, it is recommended to add health checks on `/ready` or the reconciliation portal in Uptime Kuma, and manually review the DLQ depth.

**Known boundaries (closed, not gapped; `docs/opengaps.md` currently has no outstanding items)**

- Multi-instance repeated delivery window (** has been closed, `6c99ce5`**): XAUTOCLAIM idle threshold is no longer scaled by a fixed value, but is derived from the running timeout configuration - `(max_retries + 1) × (jmap + telegram + llm timeout) × 2` is the upper limit of a single item, multiplied by the batch size, the lower limit is 300s, and the upper limit is 6h (see `docs/reference.md` §6.3). The early claim window is closed under both single-instance and multi-instance deployments; single instances are not affected, and multi-instances can be repeated at most and will not be lost.

**Audit Opinion → Closing (2026-09-26)**

- [Repair-2] When the enqueue failed, dedup released best-effort, which may have caused a 24h silent loss event → Fixed to `claim_dedup_and_enqueue` (Lua atoms: `SET NX EX` must succeed before `XADD`), there is no intermediate failure window between claim and enqueue.
- [Should be revised-1] `Email/changes` relies on `newState` for continued transmission, `jmap-client 0.4.2` does not have `upToId` → has been changed to "Same as `sinceState`, double `maxChanges` one by one to expand the window (upper limit 4096), only advance `new_state` when the window cannot be expanded" to avoid missing batches when advancing by page; `newState` The semantics have been confirmed to be load-bearing (`docs/opengaps.md` §2), and the callback verification round-trip and backlog emptying in the real Stalwart environment follow the path of baseline → increment `/changes` → `newState` (`docs/deployment.md` §4.1, §6.3.1).
- The rest of the low-risk items have been closed: the empty value of the unknown stream is changed to `Err` (fail-closed, enter retry/DLQ); `push:disable` is cleared by `forget_push_subscription`; the lower limit of `SET NX EX` TTL is tightened to `.max(1)`; XAUTOCLAIM idle threshold is scaled according to the batch size; malformed stream entries without payload are determined by `ack_malformed` is moved out of PEL via `XACK`; CSPRNG changes the owner-token to "time + PID + counter" and no longer uses constants.
- Unscheduled (Phase 5 "Search + Search Snippet") ** Closed (`bfe0fd8`)**: `/search` uses `email_query`(`Filter::text`) + `SearchSnippet/get`. For the upper limit of highlight degradation and truncation, see §2.3 and §5.7; text-level highlighting cannot be done in the locked version (see §2.3 remarks). Stage 5 Backlog cleared.

### 10.6 Total estimated time
~10.5 man-days (excluding waiting for user confirmation and real joint debugging and troubleshooting).

---

## 11. Archive of historical issues and decisions (product/architecture category, all have conclusions)

> ⚠️ Deployment/platform decisions have been **all confirmed** (single account / App Password+Basic / Redis hosting + AOF / platform HTTPS URL / external Cron reconciliation / `Q-DEP-A` platform URL and certificate configurator / `Q-DEP-B` scheduler selection), which have been filed in the Confirmed Decisions section of `docs/deployment.md` and will not be repeated in this article.

### 11.1 Authentication method (confirmed)
- **Confirmed**: **App Password + Basic** (`C-AUTH-APP-BASIC`). No master password, no OAuth2 Bearer (no OIDC required).
- Affects: `Credentials::Basic` construct; no need for OAuth client/token auto-refresh module.

### 11.2 Real-time channel (decided, see deployment.md)
- Channel = **JMAP Push HTTPS callback + external Cron reconciliation and draining** (`C-NO-LONG-CONN`/`C-HTTPS-INBOUND`); EventSource/SSE/WebSocket are all **non-target** (`NG-POLLING-SSE`). Push registration is triggered explicitly through protected `POST /api/push/register`; external Cron must call `/reconcile` (incremental enqueue) and `/worker` (drain concurrent notification) at the same time - **Only adjusting reconciliation will make the notification never sent** (deployment.md §6.3.1).
- Stalwart side access has been verified in the real environment: the built-in role has the `PushSubscription` permission, and registration, verification and round-trip and real callback delivery are all passed. The match between the number of callback retries and the idempotent key TTL/reconciliation interval is an operation and maintenance parameter item, not an acceptance threshold: `docs/deployment.md` §5 provides retry suggestions, §6.3.1 provides a value for the reconciliation interval, and §7 explains that the idempotent key TTL must cover the TG rate-limit fallback upper limit. `docs/opengaps.md` currently has no outstanding items.

### 11.3 Deployment form (confirmed, architecture related)
- **Confirmed**: **Single account implementation** (`REQ-SINGLE-ACCOUNT`); multiple accounts temporarily use **multiple bot instances** (respective tokens/configurations), **no multiple accounts single instance** (so no need for chat→account routing and `JmapService` pooling). 2026-09-28 Users have made this decision, and the boundaries and future changes to be made are registered in `docs/opengaps.md` §3.
- Deployment shape = **webhook-only + universal HTTPS-only Docker + external Cron reconciliation** (deployment.md `C-NO-LONG-CONN`/`NG-LONG-POLLING`/`NFR-RECONCILE-INTERVAL`).
- **Multi-instance LB/HA (confirmed, `ARCH-LB-WORKER`)**: You can optionally deploy the same image on multiple serverless platforms, share the same Redis, and use free Cloudflare Worker as the only entrance and failover; the Worker code is located in the subdirectory [`cloudflare-worker/`](../cloudflare-worker/) (not a native Rust binary). For deployment, see `docs/deployment.md` for Worker Deployment section. The trust model is **transparent transmission** (the backend still fails-closed verification, `SAF-LB-PASSTHRU`), the backends share the same set of secrets** (`C-LB-SHARED-SECRETS`), `/reconcile` **Redis lock** single instance (`SAF-RECONCILE-LOCK`), and the Worker provides **aggregated health view** (`MOD-HEALTH-AGG`). **Redis single point of failure is not within the scope of this solution** (`NFR-HA-MULTI-INSTANCE`, solved externally by the user). Both active-active and active-standby are available. See the Multi-Instance Deployment section of `docs/deployment.md` for details.

### 11.4 Early questions answered by code (no longer pending confirmation)
The following questions were listed in the form of Q6-Q30 during the design phase. The answers were given when the code was implemented, so they are no longer "items to be confirmed". Only the conclusions are recorded here:
- **Message format** (Q6): Outbound only has `send_text` (`src/channel.rs:114`), and its payload structure `SendMessage` (`src/channel.rs:60`) has only two fields `chat_id` + `text`, **no `parse_mode`** - Telegram renders it as plain text after receiving it, without using HTML or MarkdownV2.
- **Long emails/original text sent directly** (Q7, Q29): There is no policy of "do not send the full text if the text exceeds 4000 characters". `read_email` pulls the full text and sends it to LLM; when AI fails, it falls back to the first 300 characters and **no "truncated" mark** (see §12.3/§12.5).
- **Attachments** (Q8, Q28): Completely unimplemented. The domain model only has `has_attachment: bool`, outbound only `sendMessage`, no `send_document` and no `Blob/get` (see §12.3).
- **Push Scope** (Q9): Pull all `changes` for reconciliation, no folder/sender/keyword filtering is done on the Bot side, and there is no Sieve dependency (Q11 therefore has no impact).
- **Summary Aggregation/Timed Aggregation** (Q10): Not implemented, only `/reconcile`.
- **Multiple Sending Identities** (Q12): `Identity` is not used, single account (`REQ-SINGLE-ACCOUNT`).
- **unsafe** (Q13): There is no `unsafe` in `src/`, but `#![forbid(unsafe_code)]` is not added either.
- **Monitoring** (Q15): Implemented as `/healthz` + `/ready` two HTTP probes, without introducing Prometheus/Exporter; see deployment.md for the running platform configuration.
- **LLM Provider/Network License** (Q25, Q26, Q30): `LLM_BASE_URL` is specified by the deployer (only verifies https); `LLM_ENABLED` and `LLM_ALLOW_NET` **default are `false`**, both must be true at the same time to construct the client, otherwise the `llm` field is `None` (`Option<Arc<LlmClient>>`, no `noop()` implementation exists in the warehouse), no detection (see §12.2).
- **Break Cooling/Threshold** (Q27): Not applicable - the fuse itself is not implemented (see §12.4).

There are no pending decisions regarding deployment/platform decisions: `Q-DEP-A` (who configures the platform URL/domain name and certificate, is determined by the deployment environment at the time of release) and `Q-DEP-B` (which scheduler is used for external Cron, does not limit the implementation) have both been decided and are archived in the Confirmed Decisions section of `docs/deployment.md` and will not be repeated in this article.

---

## 12. AI auxiliary capabilities: architecture, confirmation threshold, failure fallback

> **Seven needs confirmed by users (this section is designed accordingly, and all subsequent expressions shall be subject to this)**:
> 1. **AI will only access the text after being explicitly requested and confirmed** (`REQ-AI-CONFIRM` / `REQ-AI-EXTERNAL-CONSENT`): Only when the user explicitly initiates the "analyze/summarize/translate" intention** and confirms it again**, the text is allowed to be fed to LLM; **Only when the user explicitly allows it, the email text is sent to the external AI** (default is not sent out); other scenarios (including viewing the original text)** are ignored AI**, taken directly from JMAP.
> 2. **View original text always JMAP direct access** (`REQ-VIEW-DIRECT`): `Intent::Query` and any "read text" action go to `JmapService::read_email`, not LLM.
> 3. **Long emails are prohibited from directly sending the complete original text** (`REQ-LONG-EMAIL`): When the text exceeds the threshold (default 4000 characters, see §12.3, adjustable), Bot **does not send the full text directly**, but prompts "The text is longer, please view it on a computer" or "Select AI summary"; the AI summary target is about **300 words**, **no hard mathematical truncation on the code side** (by LLM Naturally generated ~300 words).
> 4. **Analysis/summary results are not persistent** (`REQ-ANALYSIS-EPHEMERAL`): no cache, no Redis, no disk retention; every request is taken and discarded (stateless and naturally friendly).
> 5. **Attachments are pulled on demand (retained)** (`REQ-ATTACH-ONDEMAND`): No preloading; `Blob/get` is only triggered when the user clicks to download, single file ≤50MB is sent, and the limit is exceeded by the `download_url` link.
> 6. **Non-AI fallback (reserved) that requires user confirmation after AI fails about 3 times** (`REQ-AI-FUSE`): 3 consecutive failures → circuit breaker + popup confirmation to the user; select "Close for 60s" to directly follow the rule template + original text direct path during the cooling period; select "Restore AI" to do a detection.
> 7. **OpenAI-compatible environment variables (reserved)** (`REQ-LLM-OPENAI-COMPAT`): `LLM_API_KEY / LLM_BASE_URL / LLM_MODEL` etc.
>
> This section only does design and does not write business code.

### 12.1 Module division (current implementation)
There is only **one file** for LLM related code:
```
src/ai.rs   # LlmClient（唯一实现）：summarize() 打 OpenAI 兼容 /chat/completions
```
The configuration is loaded in `LlmConfig` in `src/config.rs`, and the runtime parameters are in `OutboundConfig` in `src/state.rs` (delivered by `RuntimeConfigProvider`).
The LLM capability only exists in the `src/ai.rs` file; **no** configuration/fallback/policy/audit layering - circuit breaker, rule fallback, and audit span are all not implemented (see §12.4–12.7).
`summarize()` has only **one** call site: the `/summary` authorized summary branch of `worker.rs`. New email notification **does not call LLM** - `send_notification` only renders the `From:` / `Subject:` / `Received:` three lines of metadata, and the body is never sent out (see docs/reference.md §5.4).

### 12.2 LLM variable list (current implementation)

> The `LLM_*` names in the following table are no longer environment variables**: `Config::from_env()` After deletion, the code reads zero from them,
> All loaded via Redis business configuration (§7.1). The "default" in the table is the fallback value in the `config` layer constructor.
| Variable | Type | Default | Purpose |
|---|---|---|---|
| `LLM_ENABLED` | bool | `false` | Master switch; off by default, no verification when not enabled `LLM_API_KEY`/`LLM_BASE_URL`/`LLM_MODEL` |
| `LLM_ALLOW_NET` | bool | `false` | Runtime outbound permission; `LlmClient` is constructed only if `LLM_ENABLED && LLM_ALLOW_NET` are both true, otherwise the `llm` field is `None` (`src/main.rs:135-141`) |
| `LLM_API_KEY` | string | Can be omitted | Bearer token; required only when `LLM_ENABLED=true` |
| `LLM_BASE_URL` | URL | Can be omitted | Required only when enabled, and must be `https`, otherwise `AiError::InvalidEndpoint` |
| `LLM_MODEL` | string | Can be omitted | Required only when enabled; transparently passed to `model` in `/chat/completions` |
| `LLM_SUMMARY_TARGET_CHARS` | int | `300` | `LlmClient::max_chars`, perform character truncation on the returned summary text (**there is truncation on the code side**) |
| `max_retries` (running parameters) | int | See `docs/reference.md` §6.1 | Issued by `OutboundConfig` (`RuntimeConfigProvider`), see `docs/reference.md` §6.1 for the hard upper limit; LLM is shared with Telegram outbound |
| `llm_timeout_ms` (running parameter) | int | See `docs/reference.md` §6.1 | Issued by `OutboundConfig`; unit milliseconds, for the lower limit and value range, see `docs/reference.md` §6.1 |

**None** `LLM_TEMPERATURE` / `LLM_MAX_TOKENS` / `LLM_TIMEOUT_SECS` / `LLM_MAX_RETRIES`: The request body only has `model` / `messages` / `max_tokens` (= `max_chars * 2`), no temperature, and no timeout switch independent of `llm_timeout_ms`. The API key is only read from the configuration, without entering the source code or logging (`SAF-LOG-PURITY`). For the runtime secret injection method, see deployment.md (`C-NO-SECRET-IN-IMAGE`).

### 12.3 Text acquisition strategy and long email processing (requirement 1/2/3/5)
- **View original text = JMAP direct access**: `Intent::Query` goes through `JmapService::read_email` with any "read text" action, **without going through LLM**; LLM is not on the viewing path.
- **Preliminary threshold for AI to contact the text**: Only when the user initiates a clear "analysis/summarization/translation" intention** and confirms it again** (such as clicking the `[AI Summary]` button / executing `/summarize`), the text will be input as LLM. Unconfirmed → LLM cannot see the text.
- **No "long message protection" logic**: no 4000 character threshold, no `Preview` type, no `[continue to view original]` button. The digest path after authorization is to hand over the **full text** to LLM. When LLM fails, it will fall back to the first 300 characters, and there will be no truncation mark in the whole process.
- The fallback text **does not have any annotation**: `fallback()` is `body.chars().take(300)` and returns directly without appending the words "Truncated" or "AI Unavailable".
- **The attachment function is not implemented at all**: `src/domain/jmap.rs` clearly annotates "no mutation, attachment, AI, or streaming APIs", `read_email` only returns the body text; the only attachment information in the domain model is `has_attachment: bool`. Telegram only calls `sendMessage` outbound, **does not exist** `send_document` / `Blob/get` streaming download / download button.
- The input for AI summary is only the email body text, and there is no attachment channel.

### 12.3.1 AI authorization period and Redis TTL (`REQ-AI-CONSENT`)
- AI authorization must have a duration explicitly selected by the user; optionally temporary once (one authorization window, 3600 seconds), today (86400 seconds), 7 days (604800 seconds), or until revoked (up to 365 days, or 31536000 seconds). Trigger words include matches according to the original text (`temporary`/`once`/`today`/`7 days`/`until I undo`/`long-term`, and `/ai on`, `/ai yes`, `/ai off`), and English aliases are not supported.
- Redis's consent key only saves the expiration Unix time for the chat id, and uses exactly the same number of `SET EX` seconds as the selected period; does not use the implicit 30-day default, and does not save the body or summary.
- Expiration is guaranteed by Redis TTL and expiration verification when reading. The digest request returns to metadata mode and prompts "Authorization has expired"; the user can re-select the expiration date before authorizing again. `/ai off` deletes the key immediately.

### 12.4 Failure detection (current implementation)
`LlmClient::summarize` (`src/ai.rs`) **Only retries, no circuit breaker**:
- Timeout `llm_timeout_ms`, number of retries `max_retries` (see `docs/reference.md` §6.1 for unit, default value, lower limit and hard upper limit; both items share the same configuration with Telegram outbound).
- Only retry the last failure: 429 and 5xx will be retried; **other 4xx (including 401/403) will return an error immediately and will not be retried**.
- Retries exhausted or all timed out → `Err(AiError::Response)`; URL parsing failed or non-https → `Err(AiError::InvalidEndpoint)`; JSON deserialization failed or missing `choices[0].message.content` → `Err(AiError::Response)`.
- `AiError` has only three variants: `InvalidEndpoint` / `Request` / `Response`. **None** Subdivision type for 429/401/Timeout.

### 12.5 Non-AI fallback (current implementation)
`fallback()` of `worker.rs` just takes the first 300 characters of the text, nothing more:
- The AI authorization is valid and the LLM call fails → Silently replace it with these 300 characters, **without prompting the user**, and without adding the "AI Unavailable" mark.
- AI is not authorized → Do not send summary, reply with `Sender — Subject` + "Authorization has expired or has not been authorized yet".
- Therefore, there is no fuse half-open state, no inline-button to confirm rollback, no 60s cooling period, and no fuse count written to Redis (there are no LLM-related status keys on Redis).

### 12.6 Stateless/Containerized Adaptation
- Summary **No persistence, no caching**: `worker.rs` calls LLM immediately every time, and the result is that it does not write to Redis and does not write to disk. Consistent with `C-REDIS-ONLY-STATE`.
- The log does not record prompt plain text and API key (`SAF-LOG-PURITY`) - **but the current log does not record any LLM call events** (see §8.3).
- The only valid running parameters are `llm_timeout_ms`, `max_retries`, and `LLM_SUMMARY_TARGET_CHARS` (corresponding to `LlmClient::max_chars`, which truncates the summary output). **Does not exist** The `LLM_MAX_RETRIES` environment variable.
- The request body only contains `model` / `messages` / `max_tokens` (`max_tokens = max_chars * 2`); **no temperature, no system prompt** - the body is sent directly as the only user message.

### 12.7 Error handling and observability (continued from §8)
**Not implemented** `BotError::Llm` variant (`BotError` only has `Config`/`Io`/`Json`/`State` four), LLM errors are swallowed in place within `worker.rs` and downgraded to fallback text, not propagated upward.
**Not implemented** `tracing` span for `llm.call` (no prompt hash, response length, latency record). These two items are documented in `docs/retired.md`, along with 12.4's circuit breaker/half-open design, 12.5's rules rollback, and "AI not available" logo.

### "Phase 3.5" in the 12.8 plan has not been implemented
The original plan was to insert "Phase 3.5: LLM Facade + Fallback (1.5d)" between Phases 3 and 4, delivering the three modules of `llm::client` / `llm::policy` / `llm::fallback`, plus a 4000-character long email threshold, "AI Unavailable" logo and 3 circuit breakers + Redis share count.
**This plan did not perform as designed**: LLM capabilities ended up in a single file `src/ai.rs` (see §12.1), with no facade, no policy, no fallback module, and no logo and threshold branches. Retired parts are documented in `docs/retired.md`.
The original set of Q25-Q30 design questions in this section have also been invalidated - their respective conclusions have been given when the code is implemented, and the answers can be found in §11.4.

---

## Attachment: Research basis (can be reviewed)
- `stalwartlabs/jmap-client` main Branches (the paths are relative to the root of the warehouse): `stalwartlabs/jmap-client/src/lib.rs` (URI/Method/DataType/Error), `stalwartlabs/jmap-client/src/client.rs` (Authentication/Connection/event _source), `stalwartlabs/jmap-client/src/email/`, `stalwartlabs/jmap-client/src/email_submission/helpers.rs`, `stalwartlabs/jmap-client /src/event_source/`, `stalwartlabs/jmap-client/src/push_subscription/` (create/verify/update_types/destroy), `stalwartlabs/jmap-client /src/core/error.rs`, `stalwartlabs/jmap-client/Cargo.toml`, `stalwartlabs/jmap-client/README.md`, `stalwartlabs/jmap-client/examples/`.
- crates.io: `jmap-client` metadata.
- `stalwartlabs/mail-server` main branch: `stalwartlabs/mail-server/crates/common/src/auth/credential.rs` (Password/AppPassword/ApiKey), `stalwartlabs/mail-server/crates/http/src/auth/authenticate.rs` (AccessScope Permission tailoring), `stalwartlabs/mail-server/crates/jmap/src/push/`, `stalwartlabs/mail-server/api/v1/openapi.yml` (`securitySchemes`: basicAuth/bearerAuth/liveToken 60s).
- Project directory `/home/okabe/Repo/messageweave/` (see §10.0 and `../AGENTS.md §3.3` for toolchain requirements).
- For deployment/platform related research basis (lambda_runtime/worker/aws-sdk, etc.), see deployment.md.
