# Stalwart-bot 部署方案（通用 HTTPS-only Docker）

> 部署目标：**通用 Docker 容器平台**（任何能跑 Docker 的 VPS / 云主机 / k8s / 自托管容器平台），
> **不绑定** Cloud Run / Lambda / CF Workers / Deno Deploy 等任何具体平台（`NG-SERVERLESS-BIND`）。
>
> 保持 Docker health / PORT 等通用 HTTP 约定（`C-PORT`）。
> 部署形态 = **短请求模型**：Telegram Webhook + Stalwart JMAP Push HTTPS 回调 + 外部 HTTPS Cron 对账。
> 硬约束见 [§0](#0-硬约束)；非目标见 [§1](#1-非目标non-goals)；
> 架构 / 产品行为以 [design.md](design.md) 为准，本文只写部署/运维。

---

## 0. 硬约束

> 下述约束均为**不可违反**项（对应 AGENTS.md 硬边界）。跨文档引用一律用稳定 ID，索引见 AGENTS.md「跨文档引用索引表」。

| ID | 约束 |
|---|---|
| **C-DOCKER** | 必须是 Docker 容器；禁止非 Docker 直装 |
| **C-DEBIAN-SLIM** | Debian 系 slim，禁止 Alpine/musl |
| **C-NO-SECRET-IN-IMAGE** | secrets 不进镜像（构建 ARG/ENV 不得含 token/密码） |
| **C-RUSTLS** | rustls + rustls-native-roots，不依赖系统 OpenSSL |
| **C-HTTPS-INBOUND** | 只提供 HTTPS 入站：容器内仅监听明文 HTTP，TLS 由平台入口/反向代理统一终止；无代理场景才允许容器内自服务 TLS |
| **C-HTTPS-URL** | **公网 HTTPS 入口由平台提供**：平台/反代给 bot 一个公网 HTTPS URL（如 `https://bot.example.com`），Telegram Webhook 与 Stalwart Push 回调都指向该 URL 的路径；bot 自身不申请证书、不监听 443 |
| **C-NO-TCP-EXPOSE** | 不依赖 TCP 端口暴露：全容器**单监听端口 `PORT`（默认 8080）**；对外只走 HTTP 路由（webhook / push / reconcile / health）；不暴露任何附加 TCP 端口（如 admin 端口） |
| **C-NO-LONG-CONN** | 不使用任何长连接：无 JMAP EventSource/SSE、无 WebSocket、无 Telegram 长轮询。实时性仅由 **Push HTTPS 回调 + 外部 HTTPS Cron 对账** 保证 |
| **C-REDIS-ONLY-STATE** | 无状态形态只依赖**外部 Redis**：会话、去重、Redis Streams、熔断计数、**sinceState 游标** 全部走 Redis；**不用 SQLite、不用本地卷**（`NG-SQLITE-PERSIST` / `NG-LOCAL-VOLUME`） |
| **C-REDIS-MANAGED-AOF** | Redis 由**用户托管**（自建或托管服务）并**开启 AOF 持久化**：Bot 不自建、不管理 Redis 进程；AOF 保证 Streams 队列 / 去重表 / sinceState 重启不丢 |
| **C-NO-DB** | **生产不使用任何数据库**：无 SQLite/Postgres/MySQL/嵌入式数据库；Redis 是唯一的状态存储（`C-REDIS-ONLY-STATE`）。应用不得自带、不自建、不连接第二个数据库实例 |
| **C-NO-LOCAL-WRITE** | **禁止本地文件/目录写入**：无日志文件、无数据文件、无临时缓存、不挂载本地卷（`NG-LOCAL-VOLUME`/`NG-SQLITE-PERSIST`） |
| **C-LOG-STDOUT-ONLY** | **日志只写 stdout/stderr**（容器平台/运行时负责采集落盘）；禁用 `rolling-file`、`FileAppender` 等文件日志后端；日志中**不得**出现密钥、邮件正文、AI 请求/响应内容（`SAF-LOG-PURITY`） |
| **SAF-LOG-PURITY** | 日志/Redis 写入内容**仅限**：结构化事件 ID、状态机转移、计数、时间戳、脱敏后的请求摘要（request id、状态码、耗时）；禁止 `password`/`token`/`secret`/`Authorization` 原文、JMAP 正文、LLM prompt/completion、附件内容 |
| **C-NO-STATEFUL-RECOVERY** | **禁止依赖进程内状态做生产恢复**：任何"重启续跑"（去重、sinceState、Streams 断点、熔断计数、会话）一律由外部 Redis + JMAP 对账（`FLOW-RECONCILE`/`C-REDIS-ONLY-STATE`）实现；进程内缓存仅为性能优化，丢失必须安全可重入 |

---

## 1. 非目标（Non-Goals）

> 以下均为**历史早期参考**，**非目标、不再支持**。不得在实现中复活为运行模式。

| ID | 非目标 | 说明 |
|---|---|---|
| **NG-SERVER-MODE** | `RUN_MODE=server` 常驻运行模式 | 早期 EventSource+长轮询 常驻设计，已删除 |
| **NG-POLLING-SSE** | JMAP EventSource/SSE 长连接订阅 | 与 `C-NO-LONG-CONN` 冲突；实时通道只用 Push 回调 |
| **NG-LONG-POLLING** | Telegram 长轮询运行模式 | 与 `C-NO-LONG-CONN` 冲突；只用 Webhook |
| **NG-SQLITE-PERSIST** | SQLite 会话 / sinceState 本地持久化 | 状态只走外部 Redis（`C-REDIS-ONLY-STATE`） |
| **NG-LOCAL-VOLUME** | `/app/data` 等本地卷持久化 | 无本地持久层（`C-REDIS-ONLY-STATE`） |
| **NG-SERVERLESS-BIND** | 绑定 Cloud Run / Lambda / CF Workers / Deno 等具体平台 | 部署目标是通用 Docker 容器平台；不绑定平台 |

---

## 2. Debian Slim 选型

（保留既有内容：debian:bookworm-slim 的运行基础、非 root 用户、时区、CA 证书等通用约定）

---

## 3. Rust 多阶段构建与运行

### 3.1 构建阶段
（保留既有 rust:bookworm 构建 + 缓存分层约定；产物为单个静态编译二进制 `stalwart-bot`）

> **实现现状**（阶段0 + 阶段1，`ARCH-STAGE0`/`ARCH-DEPS-STAGE0`/`ARCH-DEPS-STAGE1`）：自动依赖含 `axum 0.8`（单端口入口）/`serde`/`serde_json`/`thiserror`/`secrecy`/`subtle`（常数时间比较）/`url`/`tokio`/`tracing` 等；**`jmap-client =0.4.2` 已引入**（阶段1，`default-features=false, features=["async","rustls"]`，**禁用 WebSocket feature**，`C-NO-LONG-CONN`）；三条入口路由的鉴权已 fail-closed 落地（`R1`/`SAF-AUTH-*`），**JMAP 只读 adapter（G1/D-G1-1）代码已实现、待真实 `cargo test -- --ignored jmap::` 验证**，其余业务体仍为**占位**；`teloxide`(阶段2)/`redis`(阶段3/4)/`reqwest`(阶段3.5) 尚未引入；配置为环境变量手工解析（`ARCH-CONFIG-ENV`，无 figment/TOML）。CI 门禁见 §8（`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`，在 Debian 容器内，`GATE-P0`）。

### 3.2 运行阶段（收敛为单端口）
- `EXPOSE 8080`（遵循 `C-NO-TCP-EXPOSE`，不再暴露 9191 admin 端口）
- `ENV PORT=8080`、`RUN_MODE=webhook`（默认）
- 健康检查走同一 HTTP 监听（见 §7），不依赖独立端口
- **不声明 VOLUME**，无 `/app/data` 本地卷（`NG-LOCAL-VOLUME`）

---

## 4. 运行时用户 / 证书 / TLS 与公网 HTTPS 入口

- 非 root 用户运行（保留既有约定）
- **公网 HTTPS 入口（`C-HTTPS-URL` 已确认）**：运维在平台上给 bot 配一个公网 HTTPS URL（如 `https://bot.example.com`），平台 ingress/反代把 `https://…/webhook/tg`、/push/jmap、/reconcile 路由到容器 `PORT`。**bot 自身不申请证书、不监听 443**；证书由平台/反代管理（`C-HTTPS-INBOUND`）。
- 需要在 Stalwart 与 Telegram 两侧登记这个公网 URL：Telegram `setWebhook` 指向 `/webhook/tg`；Stalwart PushSubscription 的 `url` 指向 `/push/jmap`。
- **多实例 LB/HA 时登记的是 Worker URL 而非各后端 URL**（`C-LB-SINGLE-REG-URL`，§10）：Telegram / Stalwart / Cron 只认 Worker 的稳定域名；后端平台入口不对外登记。
- Secrets 运行期注入（环境变量/容器平台 secret），见 §5（同文件内章节链接）

---

## 5. Secret 管理与环境变量

> 迁移后生产环境仅需 `REDIS_URL` 与 `CONFIG_ENCRYPTION_KEY`；下表中的业务环境变量是历史阶段说明，不应再注入生产容器。业务密钥通过受保护的 `/api/bootstrap` 或管理员 PUT 写入 Redis，并在成功后热重建客户端。

（保留既有 Secrets 注入约定：禁入镜像、`secrecy` 包裹、日志屏蔽；新增运行模式相关变量）

| 变量 | 必填 | 说明 |
|---|---|---|
| `BOT_TOKEN` | ✅ | Telegram Bot Token |
| `TELEGRAM_CHAT_ID` | ✅ | Worker 元数据通知目标 chat id（与入站白名单分离） |
| `JMAP_SESSION_URL` | ✅ | Stalwart JMAP session URL（`REQ-JMAP-SESSION-URL`）：填**服务基地址**（`https://mail.example.com`）或**完整** `…/.well-known/jmap` 均可；代码归一化为 origin/base 后再交 `jmap-client`，**不产生重复路径**。仅 HTTPS；**禁止 URL 内嵌凭据**（`SAF-JMAP-URL`） |
| `JMAP_USERNAME` / `JMAP_PASSWORD` | ✅ | **Stalwart 认证 = App Password + Basic**（已确认 `C-AUTH-APP-BASIC`）：账号填邮箱，密码填在 Stalwart 生成的**应用专用密码**（可独立吊销/设到期）；不用主密码、不用 OAuth |
| `CHAT_ALLOWLIST` | ✅ | 聊天白名单（**硬约束 `SAF-CHAT-ALLOWLIST`**）：逗号分隔整数 chat id；处理任何事件前先校验，非白名单直接拒绝 |
| `REDIS_URL` | ✅ | **外部 Redis（用户托管 + AOF）**（`C-REDIS-ONLY-STATE`/`C-REDIS-MANAGED-AOF`）：session / dedup / Streams / fuse / sinceState 全部在此；Redis 进程不在本 compose 内 |
| `PORT` | 默认 8080 | 单监听端口（`C-NO-TCP-EXPOSE`） |
| `RUN_MODE` | 默认 `webhook` | `webhook` / `reconcile` 二选一（`NG-SERVER-MODE` 已删除） |
| `RECONCILE_TOKEN` | ✅ | `/reconcile` 的 `Authorization: Bearer <token>` 承载令牌（`SAF-AUTH-RECONCILE`）。因 `/reconcile` 路由**始终挂载**，此变量为**必填**（`SecretString`） |
| `WORKER_TOKEN` | ✅ | `/worker` 的有界处理令牌；管理 API 兼容接受该 Bearer 值，SPA 使用短期 Redis admin session |
| `TG_WEBHOOK_SECRET` | ✅ | `/webhook/tg` 校验请求头 `X-Telegram-Bot-Api-Secret-Token`（`SAF-AUTH-TG-WEBHOOK`）。须与 Telegram `setWebhook` 的 `secret_token` **完全一致**（`SecretString`） |
| `JMAP_PUSH_VERIFICATION` | ✅ | `/push/jmap` 校验请求体 JSON 字段 `verificationCode`（`SAF-AUTH-JMAP-PUSH`）。须与 Stalwart PushSubscription 的 verification code **完全一致**（`SecretString`） |
| `LLM_*` | 可选 | OpenAI-compatible 环境变量（design.md `REQ-LLM-OPENAI-COMPAT`）；**仅当用户明确允许时才把邮件正文外发 AI**（`REQ-AI-EXTERNAL-CONSENT`） |

AI 授权期限由用户选择（临时一次、1小时、今天、7天或直到撤销），Redis 仅保存 chat id、授权状态和带 TTL 的到期时间；不会保存正文或摘要。到期后摘要请求回到元数据模式并提示重新授权。
| `ACCOUNT_ID` | 默认空 | **单账户**（`REQ-SINGLE-ACCOUNT`）：留空则取 session 主账户；多账户 = 部署多个 bot 实例（各自独立 token/配置），不做多账户单实例 |

> **三个入口鉴权变量均为必填（fail-closed，`SAF-AUTH-*`）**：`RECONCILE_TOKEN` / `TG_WEBHOOK_SECRET` / `JMAP_PUSH_VERIFICATION` 缺失即**启动失败**（`Config::from_env()` 报缺失），不存在"未配置则放行"的降级路径。三个入口分别校验：`/reconcile`（Bearer 头）、`/webhook/tg`（secret 头）、`/push/jmap`（Body `verificationCode`）；校验失败一律返回 `401`，且在鉴权通过前**不产生任何副作用**。`/healthz` 与 `/ready` 为公开探针，不含敏感信息。
> 示例占位见仓库根目录 [`.env.example`](../.env.example)（仅占位符，**严禁**放入真实密钥）。

> **配置管理页面**：服务根路径 `/` 提供嵌入 Rust 二进制的 SPA。输入 Redis URL 的 ACL 密码后，`POST /api/admin/session` 签发 900 秒 admin session；页面只在内存中保存 opaque session。运行参数通过 `/api/config` 读取和保存；完整业务配置通过 `PUT /api/business-config` 替换并热加载。业务配置 API 不提供 GET，密钥不会回显；每次完整替换都需重新输入必填密钥。配置保存在外部 Redis（`C-REDIS-ONLY-STATE`），静态资源编译时随二进制打包，无运行期本地文件。

---

## 6. 运行模式与短请求模型

### 6.0 Redis-only 配置迁移与 bootstrap 威胁模型（`C-REDIS-ONLY-STATE`）

生产进程接受 `REDIS_URL` 与唯一额外启动密钥 `CONFIG_ENCRYPTION_KEY`（32 字节随机
高熵 hex，仅应用运行时持有）；其中 Redis ACL 认证是唯一 bootstrap 信任根；业务
密钥和运行参数不再从环境变量读取。空 Redis 仅提供配置页面及一次性的 bootstrap
会话：请求必须以 Redis ACL 密码作为 Bearer 凭据，服务端只做常数时间比较，绝不在
响应、日志或配置值中回显该密码。bootstrap 使用原子 `SET NX` 写入完整业务配置，
竞争请求只有一个成功；无密码 Redis 或认证失败时拒绝初始化。

该 ACL 身份必须具备 `config:business`、`config:outbound` 及 admin-session 键的读写权限，
因为它同时是应用 Redis 连接凭据与空库 bootstrap 根信任。bootstrap 完成后公网业务配置
接口只接受 900 秒 admin session；持有 ACL 密码者仍等同根信任，必须按生产 Redis 凭据
同等级保护，不得把该身份误配置为禁止配置键访问。

成功 bootstrap 后，后续配置读写只能使用 Redis 中保存的管理员会话（短 TTL，注销
或配置更新时失效）；业务端点使用 Redis 配置中下发的独立令牌。管理员会话只保存
不可逆哈希和过期时间，重启后按 Redis TTL 恢复，进程不保存本地状态。GET 永不回显
任何密钥，配置缺失或业务依赖未就绪时保持 HTTP/SPA 可用但业务路由返回未就绪。

`config:business` 使用 AES-256-GCM（版本、随机 nonce、认证 tag 均在密文 envelope 中）
写入 Redis；应用启动时使用 `CONFIG_ENCRYPTION_KEY` 解密，密钥永不写 Redis、日志、响应
或浏览器存储。该保护只防 Redis 内容泄露，不替代 `rediss`、VPN 或受信网络对 `redis://`
窃听/篡改的防护；公网 Redis 必须使用 TLS/VPN/隧道。

bootstrap 或管理员 PUT 修改 `config:business` 后，服务会先校验并构建全部 JMAP/Telegram/LLM
客户端，再原子替换 worker；构建失败保留旧配置与任务。后续请求即时使用新快照，多实例在
请求边界按 revision 有界检查并尝试刷新；失败时保留旧实例，避免忙循环。

TLS 必须由 Cloudflare 或受信任反向代理终结；代理到容器的链路只能位于受控私网，
并应校验可信转发头后才允许 bootstrap。容器端口不得直接暴露公网。SPA 仅在当前页面
内存保存 opaque admin session，不使用 localStorage、sessionStorage 或 Cookie 持久化。

> 仅两种运行模式；无长连接（`C-NO-LONG-CONN`）。

| RUN_MODE | 命令 | 说明 |
|---|---|---|
| `webhook`（默认） | `stalwart-bot webhook`（或单一入口按 env 分派） | 单进程：TG Webhook handler + JMAP Push 回调 handler + Redis Streams worker + `/reconcile` + health |
| `reconcile` | `stalwart-bot reconcile` | 一次性对账补差后 exit，由外部调度器/容器任务触发 |

### 6.1 入口路由（单端口 `PORT`，全部经平台 HTTPS URL 入站）
```
# 平台公网 URL: https://bot.example.com  → 反代 → 容器 127.0.0.1:8080
GET  /                  业务配置与运行参数管理 SPA
GET  /assets/config.js  SPA 脚本；GET /assets/styles.css  SPA 样式
POST /api/admin/session       Bearer Redis ACL 密码；成功返回 900 秒 admin session
POST /api/admin/session/revoke Bearer admin session；成功返回 204
GET|PUT /api/config           Bearer admin session 或 WORKER_TOKEN；Redis 错误返回 503
PUT /api/business-config      Bearer admin session 或 WORKER_TOKEN；完整替换，成功返回 204
POST /webhook/tg      TG Webhook → [鉴权 SAF-AUTH-TG-WEBHOOK: 头 X-Telegram-Bot-Api-Secret-Token]
                      → 快速 2xx ACK → 幂等去重(MOD-DEDUP) → 命令处理 → 同步回复（快路径）
POST /push/jmap       Stalwart Push 回调 → [鉴权 SAF-AUTH-JMAP-PUSH: Body verificationCode]
                      → 幂等去重(MOD-DEDUP) → 入 Redis Streams(MOD-STREAMS) → 立即 2xx ACK（慢任务异步）
POST /reconcile      外部 HTTPS Cron 触发 → [鉴权 SAF-AUTH-RECONCILE: Authorization: Bearer RECONCILE_TOKEN]
                      → 对账补差 FLOW-RECONCILE
GET  /healthz        liveness（ARCH-HEALTHZ：进程存活；公开探针 SAF-PROBE-PUBLIC，无鉴权、无敏感信息）
GET  /ready          ⚠️ 阶段0 为**占位 200**（ARCH-READY-PLACEHOLDER）——**不执行** Redis PING / JMAP session / TG getMe；真实 readiness 属后续门禁 GATE-READY-DEPS（公开探针，无鉴权、无敏感信息）
```
> **三入口鉴权（fail-closed，`SAF-AUTH-*`）**：三条写路径必须先通过鉴权，**失败返回 `401` 且不产生副作用**；比较使用常数时间（`subtle`，防时序侧信道）。secret 未配置 → 启动失败，**无"缺省放行"**。
> **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz`、`/ready` 仅返回健康状态、**不含敏感信息**；**`/ready` 现阶段不检查任何依赖**，不得据此判断 Redis/JMAP/TG 可用性（见 §7 与 design.md §10.0 `GATE-READY-DEPS`）。
> **"公网 HTTPS 入口"是什么**（`C-HTTPS-URL`）：平台给 bot 一个公网 HTTPS 域名，外部（Telegram / Stalwart / 调度器）通过它访问上面这些路径；容器只处理明文 HTTP，TLS 由平台终止。运维只需在平台上配置域名/证书并确保 4 条路径可达。

### 6.2 Redis Streams / worker（MOD-STREAMS）
- Push 回调只做：校验 → 去重 → 入队 → ACK；不阻塞
- worker（同容器后台 task 或独立 worker 容器，2 选 1 均由 compose/编排决定）：
  `XREADGROUP → Email/changes → 推送 TG → 推进 sinceState → XACK`
- 消费组 at-least-once：未 ACK 消息自动重投；处理幂等（MOD-DEDUP 二次兜底）

### 6.3 外部 HTTPS Cron 对账（FLOW-RECONCILE）
- **"外部 Cron"是什么**（已确认）：bot **不自建定时器、不持有调度**（`C-NO-LONG-CONN`，无 `tokio-cron`）。
  由**外部调度器**周期性发起 `POST https://<平台URL>/reconcile`（带 `RECONCILE_TOKEN`），触发一次对账。
- **建议间隔 5–10 分钟**（`NFR-RECONCILE-INTERVAL`）：兼顾"少延迟"与"低开销"；这是可用性的兜底频率。
- 可用调度器（任选其一，均为外部）：系统 crontab+curl / k8s CronJob / GitHub Actions scheduled / 第三方 cron 服务。
- 对账逻辑：用 `sinceState` 调 `Email/changes` 拉增量 → 与已处理 email_id 求差 → 补发通知 → 推进 `sinceState`。
- **Redis 丢失恢复**：sinceState 存 Redis（`MOD-SINCESTATE`，AOF 持久化 `C-REDIS-MANAGED-AOF`）；即便 Redis 全丢，对账扫描 JMAP（`Email/query` 最近 N 封 + changes）也能重建游标并补发——**事实源在 JMAP，Redis 只是加速层**。

### 6.4 可靠性策略（Reliability）

> 目标：**至少 99.9% 通知可用性**（`NFR-NOTIFY-SLA`），允许少量延迟（不追求秒级保证）。下述机制共同保证"不丢、少重、可恢复"。

| 机制 | ID | 做法 |
|---|---|---|
| **Streams ACK / 重试** | `MOD-STREAMS` | worker 用消费组 `XREADGROUP`；处理成功才 `XACK`；未 ACK 消息在被认领后重投（at-least-once）。处理失败时**不 ACK**，自然重试；设最大投递次数上限，超限转死信（`XADD` 到 `dlq`）并告警，避免毒丸阻塞 |
| **幂等去重** | `MOD-DEDUP` | 以 `(account, email_id)` 为幂等键，`SET NX`（TTL 覆盖重投窗口）。重复投递直接跳过，保证 at-least-once 下**不重复通知**。TG 侧同理用 `update_id` |
| **Push 重试** | `FLOW-NEW-MAIL` | `/push/jmap` 校验/入队后**立即 2xx**；若入队失败（Redis 抖动）返回非 2xx，让 Stalwart 按自身策略重试；配合对账兜底 |
| **对账恢复** | `FLOW-RECONCILE` | 外部 Cron 每 5–10 分钟拉 `Email/changes` 补差，覆盖 Push 丢失/Redis 抖动/冷启动窗口；同时用于 Redis 全丢后的游标重建 |
| **指标 / 告警** | — | 导出指标：Push 到达数、去重命中率、Streams 积压深度（pending）、DLQ 条数、对账补发条数、JMAP 延迟、TG 推送失败率。对"积压持续增长 / DLQ 非空 / 对账补发异常升高"告警。**注**：`/ready` 现为占位 200（`ARCH-READY-PLACEHOLDER`），**尚不反映依赖健康**；待 `GATE-READY-DEPS` 落地后才可用于依赖告警 |

### 6.5 99.9% 可用性目标与边界（NFR-NOTIFY-SLA）

- **定义**：在外部依赖（Stalwart / Telegram / Redis）可用的前提下，通知链路的可用性目标 ≥ **99.9%**（约每月 ≤43 分钟不可用）。
- **允许的延迟**：用户已确认**接受少量通知延迟**——正常情况下 Push 回调应为秒级；依赖抖动/需对账兜底时，延迟上界为**对账间隔（≤10 分钟）**。
- **非目标边界**：99.9% 是**通知可用性**目标，不含 Stalwart/Telegram/Redis 自身故障时间；三者任一长时间不可用属外部依赖故障，不计入本 bot 的可用性预算（但 bot 应在恢复后经对账自动补齐）。
- **降级行为**：Redis 不可用时，去重/会话退化为进程内短窗口、Push 回调返回非 2xx 交 Stalwart 重试；恢复后由对账补齐。**不因 Redis 抖动而漏发最终通知**（以对账为准）。

---

## 7. 健康检查（通用 HTTP 约定）

- `GET /healthz` → liveness（`ARCH-HEALTHZ`；进程存活，长期语义）
- `GET /ready` → **阶段0 为占位 `200`**（`ARCH-READY-PLACEHOLDER`），**不做** Redis PING / JMAP session / TG getMe。真实 readiness 检查（依赖不可用 → `503`）为**后续门禁 `GATE-READY-DEPS`**，随 `redis` 接入且 JMAP 只读 adapter 可用后实现。
- 两者均为**公开探针**（`SAF-PROBE-PUBLIC`）：无鉴权、仅返回健康状态、**不含敏感信息**。
- ⚠️ 在 `GATE-READY-DEPS` 落地前，**不得**把 `/ready` 描述/当作"依赖健康检查"，编排探针亦不得依赖其探测依赖。
- Docker `HEALTHCHECK` 指向同一监听端口（app 内置 `health` 子命令或 wget 同端口），**不依赖独立端口**（`C-NO-TCP-EXPOSE`）
- 仅保留通用平台映射（compose `HEALTHCHECK`、k8s probe）；不写 Cloud Run/Fly 等特指内容（`NG-SERVERLESS-BIND`）

---

## 8. CI 发布与镜像验收

### 8.0 代码门禁（`GATE-P0`，进入阶段1前必须通过）
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

以上三条**在 Debian `rust:1-slim-bookworm` 容器内执行**（`C-DEBIAN-SLIM`）；详见 design.md §10.0。

（其余 CI：镜像构建 → 镜像扫描 → 非 root → health 端点到容器验证；compose 示例收敛为单端口 + 外部 Redis URL + 无 volume）

### 8.1 docker-compose 示例（收敛）
```yaml
services:
  bot:
    image: stalwart-bot:latest
    ports:
      - "8080:8080"        # 平台 ingress/反代映射 HTTPS → 8080（C-HTTPS-INBOUND）
    environment:
      PORT: "8080"
      RUN_MODE: webhook
      # BOT_TOKEN / JMAP_* / CHAT_ALLOWLIST / REDIS_URL
      # RECONCILE_TOKEN / TG_WEBHOOK_SECRET / JMAP_PUSH_VERIFICATION（三入口鉴权，均为必填 SAF-AUTH-*）
    healthcheck: { test: ["CMD", "stalwart-bot", "health", "--addr", "127.0.0.1:8080"], interval: 30s }
    # REDIS_URL 指向外部、已认证的 Redis；不在此 compose 中运行 Redis。
```

> 注意：本示例对接 Compose；真实部署可去掉 `ports` 直接挂到平台 ingress（HTTPS-only）。

### 8.2 镜像与部署验证检查项（红线自检）

以下检查在每次发布前/CI 中执行，用于固化 §0 新增红线：

| # | 检查项 | 关联 ID | 命令/方法 |
|---|---|---|---|
| 1 | **镜像内无数据库引擎**：不装/不含 SQLite、Postgres、MySQL 二进制或数据文件 | `C-NO-DB` | `docker run --rm <image> sh -c 'command -v sqlite3 psql mysql 2>/dev/null; [ -z "$(find / -maxdepth 4 -name "*.db" -o -name "*.sqlite*" 2>/dev/null)" ]'` 应为空 |
| 2 | **容器内不出现本地可写挂载**：compose/k8s 不声明 `volumes:` 用于数据/日志目录 | `C-NO-LOCAL-WRITE` / `NG-LOCAL-VOLUME` | 人工/CI lint 检查 compose 文件、k8s manifest 不含本地 volume 声明 |
| 3 | **日志仅 stdout/stderr**：应用启动参数不得含 file/rolling-file 后端；tracing 配置为 `stdout` | `C-LOG-STDOUT-ONLY` | 检查代码：`grep -rEn 'rolling\|FileAppender\|RollingFileAppender' src/` 应为空；`RUST_LOG` 目标不含 `file:` |
| 4 | **日志/Redis 无敏感数据**：日志字段白名单 + CI 扫描 | `SAF-LOG-PURITY` | 代码评审 + 静态扫描（如 `gitleaks` 扫描 `Redis` 写入调用，验证 `SETEX`/`SET` 参数无密钥/正文/AI 内容） |
| 5 | **重启恢复不依赖进程内状态**：所有"续跑"逻辑必须读 Redis；进程内缓存仅性能优化 | `C-NO-STATEFUL-RECOVERY` | 代码评审：`grep -rn 'static mut\|lazy_static' src/` 不得用于生产恢复路径 |
| 6 | **Redis 由外部提供**：容器不运行 `redis-server`；compose 不启动 Redis 服务 | `C-REDIS-MANAGED-AOF` | `docker run --rm <image> sh -c 'command -v redis-server && exit 1 || exit 0'` 应为 0 |

> 任一检查失败，禁止发布。

---

## 9. 已确认决策与少量待办

### 9.1 已确认决策（不再作为待确认问题）

| ID | 决策 |
|---|---|
| `REQ-SINGLE-ACCOUNT` | **单账户实现**；多账户暂用**多个 bot 实例**（各自 token/配置），不做多账户单实例 |
| `C-AUTH-APP-BASIC` | Stalwart 认证 = **App Password + Basic**（不用主密码、不用 OAuth） |
| `REQ-AI-EXTERNAL-CONSENT` | **仅当用户明确允许时**才把邮件正文发往外部 AI（默认不外发） |
| `C-REDIS-MANAGED-AOF` | Redis 由**用户托管**并**开启 AOF 持久化**（Bot 不自建/不管理 Redis） |
| `C-HTTPS-URL` | **公网 HTTPS 入口由平台提供**（平台给 bot 一个 HTTPS URL；bot 不持证书、不监听 443） |
| `NFR-RECONCILE-INTERVAL` | **外部 Cron 定期 HTTPS POST `/reconcile`，建议 5–10 分钟**；容器不自建定时器 |
| `NFR-NOTIFY-SLA` | **允许少量通知延迟；通知可用性目标 ≥ 99.9%**（边界见 §6.5） |
| `ARCH-LB-WORKER` | **多实例高可用**：免费 Cloudflare Worker 作为**唯一对外入口 + 故障转移**，后端为 2+ 个不同 serverless 平台的同镜像实例，共享同一 Redis（详见 §10） |
| `SAF-LB-PASSTHRU` | 信任模型 = **透传（A）**：Worker 不改写鉴权信息，**后端必须继续 fail-closed 校验**（小平台无防火墙/ACL，"后端不对公网暴露"不可行） |
| `SAF-RECONCILE-LOCK` | `/reconcile` **不扇出**：用 **Redis 锁**保证同一时刻仅一个实例执行 |
| `MOD-HEALTH-AGG` | Worker 暴露**聚合健康视图**，供外部监控 |
| `NFR-HA-MULTI-INSTANCE` | 双活或主备**均可**；**Redis 单点故障不在本方案范围**（用户外部解决，短暂不可用可接受） |
| `C-NO-DB` | **生产不使用任何数据库**：Redis 是唯一状态存储；应用不自建/不连接第二个数据库 |
| `C-NO-LOCAL-WRITE` | **禁止本地文件/目录写入**：无日志文件、无数据文件、无临时缓存、不挂载本地卷 |
| `C-LOG-STDOUT-ONLY` | **日志只写 stdout/stderr**（平台负责采集落盘）；禁用文件日志后端 |
| `SAF-LOG-PURITY` | **日志与 Redis 写入不得包含**：密钥、邮件正文、AI 请求/响应、附件内容；仅允许结构化事件、计数、时间戳、脱敏摘要 |
| `C-NO-STATEFUL-RECOVERY` | **禁止依赖进程内状态做生产恢复**：重启恢复一律走 Redis + JMAP 对账（`FLOW-RECONCILE`）；进程内缓存仅为性能优化，丢失必须安全可重入 |

### 9.2 仍需确认（部署相关，仅此）

| # | 待确认 | 关联 ID |
|---|---|---|
| **Q-DEP-A** | 平台 URL / 域名与证书由谁配置（平台自动证书 or 自管反代），以及 4 条路径的可达性验证方式 | `C-HTTPS-URL` |
| **Q-DEP-B** | 外部调度器具体选型（系统 crontab / k8s CronJob / CI scheduled / 第三方 cron）—— 仅影响运维方式，不改变架构 | `NFR-RECONCILE-INTERVAL` |

> 其余产品/架构问题见 design.md；不再有平台特定部署问题（`NG-SERVERLESS-BIND`）。

---

## 10. 多实例高可用与 Worker 前置负载均衡（`ARCH-LB-WORKER`）

> 目标：在**多个 serverless 平台各部署一份相同镜像**（各自不同的 HTTPS 入口），最前面用一个**免费 Cloudflare Worker** 作为**唯一对外入口 + 故障转移**；所有实例共享**同一个外部 Redis**。双活（round-robin）或主备（active-standby）**均可**。业务代码无需改动——之所以可行，是因为既有设计已满足前提（见下）。

### 10.1 拓扑（`ARCH-LB-WORKER`）

```
Telegram setWebhook ─┐
Stalwart PushSub ────┼─▶ https://lb.<you>.workers.dev      ← 唯一登记 URL（C-LB-SINGLE-REG-URL）
外部 Cron ───────────┘        │  Cloudflare Worker（免费）
                              ├─▶ https://a.<platform1>/…  ─┐
                              └─▶ https://b.<platform2>/…  ─┼─▶ 同一个外部 Redis（C-REDIS-ONLY-STATE）
                                 （同一镜像、同一配置、同一组 secret）─┘
```

- **只登记 Worker 的稳定 URL**（`C-LB-SINGLE-REG-URL`）：Telegram `setWebhook`、Stalwart `PushSubscription.url`、外部 Cron **都指向 Worker**（`https://lb.example.com/webhook/tg`、`/push/jmap`、`/reconcile`）。后端易变的平台入口**不对外登记**。
- 后端 = 与 §3 相同的镜像、相同 env、相同 Redis；**不引入长连接**（Worker 纯请求-响应，`C-NO-LONG-CONN`）。

### 10.2 为什么可直接支持多实例（既有前提）

| 前提 | ID | 作用 |
|---|---|---|
| 状态仅外部 Redis | `C-REDIS-ONLY-STATE` | 任意实例可服务任意请求，**无需粘性会话** |
| 幂等去重 | `MOD-DEDUP` | 重复/重试投递**不会重复通知**，故"多实例 + 重试"安全 |
| 对账补差 | `FLOW-RECONCILE` | 兜住 Push 丢失 / cold-start 窗口 |

> 因此 LB/HA 是**纯运维拓扑**变化；领域逻辑与渠道层无需改动。

### 10.3 信任模型 = **透传（A）**（`SAF-LB-PASSTHRU`）

- Worker **原样转发** header / body / `verificationCode`，**不做鉴权改写**。
- **后端必须继续 fail-closed 校验**三条入口（`SAF-AUTH-*`）——**不可省**。原因：小平台通常**不提供防火墙/ACL**，后端 HTTPS 入口可能被公网直连；因此"仅靠 Worker 防护"不成立（方案 B「后端不对公网暴露」在本场景**不可行**）。
- **多实例必须共享同一组 secret**（`C-LB-SHARED-SECRETS`）：`TG_WEBHOOK_SECRET` / `JMAP_PUSH_VERIFICATION` / `RECONCILE_TOKEN` 在所有实例上**完全一致**，否则请求落到不同实例会随机 `401`（对端发来的值只有一个）。

### 10.4 路由与故障转移

- **路由 safelist**（`C-LB-SINGLE-REG-URL`）：Worker 只透传 `GET /`、SPA 静态资源、`GET|PUT /api/config`、`PUT /api/business-config`、`POST /api/admin/session`、`POST /api/admin/session/revoke`、`POST /webhook/tg`、`POST /push/jmap`、`POST /reconcile`、`GET /ready`；**未知路径 404、method 不符 405**，不透传至后端。所有管理 API 的 Bearer 鉴权由后端执行（`SAF-LB-PASSTHRU`）。
- **健康聚合（`MOD-HEALTH-AGG`）**：Worker 自行承载 `GET /healthz`，按 TTL 缓存（默认 30s，`LB_HEALTH_TTL_MS` 可调）探测各后端 `/healthz`，返回 `{status, available, total, backends:[{origin,up,status}]}`；≥1 后端 up → 200，全 down → 503。`/ready` 仍**透传**给后端，由 `GATE-READY-DEPS` 承担真实依赖探测。
- **故障转移（`proxyWithFailover`）**：每次请求最多 `min(LB_MAX_ATTEMPTS, origins.length)` 次尝试；**仅**超时（AbortError）或 5xx 触发换下一个 origin；4xx/2xx/3xx 直接返回；默认 `LB_MAX_ATTEMPTS=2`（首次 + 1 次故障转移）。
- **随机分摊**：起点 origin 按 `Math.random` 随机化，实现双活；单 origin 配置时退化为确定性。
- **全失败兜底**：返回 `503 All Backends Unavailable`，交由 Telegram / Stalwart 自动重投（**不丢消息**）。
- **超时预算**（`LB_REQUEST_TIMEOUT_MS`，默认 10s）> 最坏 cold start。
- **重复兜底**：Worker 重试 + Telegram 重投造成的重复由 `MOD-DEDUP` 吸收；**幂等键 TTL 必须覆盖** Worker 重试与 Telegram 重投窗口。
- **不代理 Redis/JMAP**（`C-NO-DB` / `C-REDIS-ONLY-STATE`）：Worker 只做请求转发，后端各连自己的 Redis；Worker 不做数据库侧检查。
- **无长连接**（`C-NO-LONG-CONN`）：纯请求-响应，body 一次性 `arrayBuffer` 回灌；无 WS/SSE/长轮询。

### 10.5 各入口差异（重要）

| 入口 | 是否可扇出/重复 | 处理 |
|---|---|---|
| `/webhook/tg` | 可安全重复（`update_id` 去重） | Worker 选一实例；重试安全 |
| `/push/jmap` | 可安全重复（`(account,email_id)` 去重） | Worker 选一实例 |
| `/reconcile` | ❌ **不可扇出** | **Redis 锁**（`SAF-RECONCILE-LOCK`）保证**同一时刻仅一个实例执行**，避免重复对账 |
| Streams worker | 用**相同消费组名**（`MOD-STREAMS-GROUP`） | Redis `XREADGROUP` **自动分摊**给多实例；at-least-once 下同一消息不会重复处理 |

### 10.6 聚合健康视图（`MOD-HEALTH-AGG`）

- Worker 额外暴露**聚合端点**，报告各后端存活数与整体可用性，供外部监控/告警使用；比单实例探针更有意义。

### 10.7 边界与不在本方案范围

- **Redis 单点故障不在本方案范围**（`NFR-HA-MULTI-INSTANCE`）：由用户在外部解决；其短暂不可用**可接受**（Redis 全丢可由 `FLOW-RECONCILE` 从 JMAP 重建 sinceState；dedup/会话丢失只导致**少量重复通知**，符合 `NFR-NOTIFY-SLA`）。
- **Cloudflare Worker 免费额度**：预期足够（requests/day、subrequest、CPU/超时上限需上线后实测确认）。
- 不引入长连接、不引入平台特定绑定（`C-NO-LONG-CONN`/`NG-SERVERLESS-BIND`）。

### 10.8 部署 Cloudflare Worker（`ARCH-LB-WORKER` 具体步骤）

代码位于仓库子目录 [`cloudflare-worker/`](../cloudflare-worker/)，**非本 Rust 二进制的一部分**；Worker 只做请求转发，不运行 Redis/JMAP，不引入数据库（`C-NO-DB`）。

**前置**
- 已按 §3 部署 ≥1 个后端镜像（多实例则部署 2+ 个），各后端 `PORT`/路由同 §6.1。
- 后端各实例 env 必须共享同一组 secret（`C-LB-SHARED-SECRETS`）。
- 已注册 Cloudflare 账号与 Workers 计划。

**配置**（所有值通过 `wrangler secret` 注入，**禁止**写入 `wrangler.toml` 或 `git` 明文）

```bash
cd cloudflare-worker
npm install --no-audit --no-fund
npx wrangler secret put BACKEND_ORIGINS_JSON   # JSON 数组，见下方示例
# 可选（带默认值）
npx wrangler secret put LB_REQUEST_TIMEOUT_MS   # 默认 10000ms
npx wrangler secret put LB_MAX_ATTEMPTS         # 默认 2（首次 + 1 次故障转移）
npx wrangler secret put LB_HEALTH_TTL_MS        # 默认 30000ms
```

`BACKEND_ORIGINS_JSON` 示例（**仅允许 `https://` origin**，其它会被 Worker 启动即 503，`C-HTTPS-INBOUND`）：

```json
[
  {"url":"https://stalwart-bot-a.example.com","weight":100},
  {"url":"https://stalwart-bot-b.example.com","weight":100}
]
```

**部署与验证**

```bash
cd cloudflare-worker
npx wrangler deploy --env production
# 验证 LB 健康聚合
curl https://<your-worker>.workers.dev/healthz
```

**Telegram / Stalwart / Cron 登记的唯一 URL** 改为 `https://<your-worker>.workers.dev/webhook/tg` 等（`C-LB-SINGLE-REG-URL`）；**各后端平台的 HTTPS 入口不再对外登记**（小平台入口可能被公网直连，故**后端鉴权不可省**，`SAF-LB-PASSTHRU`）。

**验证（发布前必做）**
1. `cd cloudflare-worker && npm test`（Node 单测，代理逻辑 + 健康聚合 + 超时/失败重试 + safelist）。
2. `npx wrangler deploy --env production` 后 `curl /healthz` 返回 200 且 `available ≥ 1`。
3. `curl -X POST https://<worker>/webhook/tg -H "Content-Type: application/json" -d '{"test":1}'` 应得到后端返回（非 503）。
4. `curl -X POST https://<worker>/unknown` 应得到 404；`curl -X GET https://<worker>/reconcile` 应得到 405。
5. 日志仅出现 `method/path/origin/失败类别`，**绝不**含 header/body/secret（`SAF-LOG-PURITY`）。

**边界说明**
- Worker **未**实现 `/reconcile` 的 Redis 锁（`SAF-RECONCILE-LOCK`）——该锁由**后端**在 `/reconcile` 处理器内部执行；Worker 只做透传。详见 `SAF-RECONCILE-LOCK` 条目。
- Worker **不代理** Redis、不检查数据库侧可用性（`C-NO-DB` / `C-REDIS-ONLY-STATE`）。
- Worker **不落地** JMAP/Telegram 的 secret（`TELEGRAM_WEBHOOK_SECRET`/`JMAP_PUSH_VERIFICATION` 等属于**后端**容器，见 `deployment.md §5` 与 `.env.example`）。

---

## 附：稳定 ID 引用索引

> 跨文档稳定 ID 的**唯一权威索引表**在 `AGENTS.md` §7「跨文档引用索引表」。
> 本文使用到的 ID（`C-*` / `NG-*` / `MOD-*` / `FLOW-*` 等）均在其中登记定义点位置与一句话说明；内容搬家时**只改 AGENTS.md 索引表**的"文件/锚点"列，所有引用本身零改动。
