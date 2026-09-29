# MessageWeave 部署方案（通用 HTTPS-only Docker）

> 部署目标：**通用 Docker 容器平台**（任何能跑 Docker 的 VPS / 云主机 / k8s / 自托管容器平台），
> **不绑定** Cloud Run / Lambda / CF Workers / Deno Deploy 等任何具体平台（`NG-SERVERLESS-BIND`）。
>
> 保持 Docker health / PORT 等通用 HTTP 约定（`C-PORT`）。
> 部署形态 = **短请求模型**：Telegram Webhook + Stalwart JMAP Push HTTPS 回调 + 外部 HTTPS Cron 对账。
> 硬约束见 [§0](#0-硬约束)；非目标见 [§1](#1-非目标non-goals)；
> 架构 / 产品行为以 [design.md](design.md) 为准，本文只写部署/运维。

---

## 0. 硬约束

> 下述约束均为**不可违反**项（对应 `docs/charter.md §3` 安全边界）。跨文档引用一律用稳定 ID，索引见 `docs/charter.md §8`「稳定 ID 注册表」。

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
| **NG-SERVER-MODE** | 常驻长连接运行模式（历史名 `RUN_MODE=server`） | 早期 EventSource+长轮询 常驻设计，已删除；`RUN_MODE` 变量本身也已随 `Config::from_env()` 一并删除，代码中已无该标识符 |
| **NG-POLLING-SSE** | JMAP EventSource/SSE 长连接订阅 | 与 `C-NO-LONG-CONN` 冲突；实时通道只用 Push 回调 |
| **NG-LONG-POLLING** | Telegram 长轮询运行模式 | 与 `C-NO-LONG-CONN` 冲突；只用 Webhook |
| **NG-SQLITE-PERSIST** | SQLite 会话 / sinceState 本地持久化 | 状态只走外部 Redis（`C-REDIS-ONLY-STATE`） |
| **NG-LOCAL-VOLUME** | `/app/data` 等本地卷持久化 | 无本地持久层（`C-REDIS-ONLY-STATE`） |
| **NG-SERVERLESS-BIND** | 绑定 Cloud Run / Lambda / CF Workers / Deno 等具体平台 | 部署目标是通用 Docker 容器平台；不绑定平台 |

---

## 2. Debian Slim 选型

（保留既有内容：debian:bookworm-slim 的运行基础、非 root 用户、时区、CA 证书等通用约定）

### 2.1 可选远程联调面 `/debug/*`（`SAF-DEBUG-GATE`）

生产入口默认**绝对关闭**：`/debug/*` 是一套只读探测 + 单条出站通知的远程联调面，只有部署者主动开启才会出现。

**双因子启用条件（`SAF-DEBUG-GATE`，两者必须同时成立）**

1. 进程命令行必须带 `--debug`（读取于 `src/main.rs:82`）。
2. `DEBUG_TOKEN` 环境变量必须存在且非空（读取于 `src/main.rs:82-89`）。

缺一即不挂载：`debug_router()` 本身会构造出全部 7 条路由（`src/debug.rs:72-80`），但主入口**只在双因子成立时才合并它**（`src/notify.rs:1321-1322`）。所以未开启时 `/debug/*` 路由根本不存在，请求走 axum 兜底返回普通 `404 not found`——**不是** 401，也不会泄露「此路径存在」。开启成功时打一条 WARN 日志标记该面已打开（`src/main.rs:90-92`，`SAF-LOG-PURITY`：只记开启状态，从不记录 token 值）。

**鉴权**

7 条路由全部要求 `Authorization: Bearer <DEBUG_TOKEN>`，统一走 `debug_authorized`（`src/debug.rs:45`），它委托生产同款 `worker_authorized`（`src/notify.rs:446`），因此令牌是**常数时间比较**（`src/notify.rs:1136`）。失败回 `401 unauthorized`，且不设 `Retry-After`（`src/debug.rs:52-54`）。

**7 条路由与预期状态码**（注册于 `src/debug.rs:72-80`，路径为字面量，无常量抽取）

| 路由 | 成功 | 失败 |
|---|---|---|
| `GET /debug/ping` | `200 {"ok":true}` | 401 |
| `GET /debug/config` | `200` 见下段 | 401 |
| `GET /debug/redis` | `200 {"reachable":true,"global_enabled":…}` | 401；Redis 探活失败**仍为 200** `{"reachable":false,"detail":"redis_probe_failed"}`，不返回 503 |
| `GET /debug/jmap` | `200 {"ok":true}` | 401；探针失败**仍为 200** `{"ok":false,"detail":…}`（与 `/ready` 共用同一探针） |
| `GET /debug/telegram` | `200 {"ok":true}` | 401；同上，探针失败返回 200 + `ok:false` |
| `GET /debug/worker` | `200`（`revision`、`reconcile_cursor`、`outbound` 预算） | 401 |
| `POST /debug/notify` | `200 {"ok":true,"result":{…}}` | 401；业务配置未加载或无出站客户端 → `503 service_unavailable` + `Retry-After: 30`（`src/debug.rs:56-58`）；`chat_id` 不在白名单 → `403 chat_not_allowed`（`src/debug.rs:60-62`）；Telegram 发送失败 → `502 telegram_send_failed`（`src/debug.rs:64-66`） |

`/debug/config` 的响应字段（`src/debug.rs:110-148`）：恒有 `revision`、`setup_missing`、`business_configured`、`allowlist_size`；业务配置存在时再加 `jmap.{session_url,username,account_id}`、`telegram.{chat_id,webhook_secret_configured}`、`worker.{worker_token_configured,reconcile_token_configured}`、`llm.{enabled,allow_net,api_key_configured,base_url,model}`、`outbound.{jmap_timeout_ms,telegram_timeout_ms,llm_timeout_ms,max_retries}`；配置缺失时只回 `business_configured:false` + `allowlist_size`。

方法不匹配先于鉴权判定（例如 `GET /debug/notify` 返回 `405`）。

**三条约束，部署时务必确认**

- **绝不回显凭据值**：`debug_config`（`src/debug.rs:94`）对每个 Secret 字段只输出 `*_configured` **布尔**——JMAP 密码、bot token、`worker_token`、`reconcile_token`、LLM `api_key` 一律不落响应体（`SAF-DEBUG-AUTH`）。注意这是「不含凭据」，不是「全脱敏」：**非密文的身份与预算字段是明文返回的**（JMAP session URL 与 username、Telegram `chat_id`、LLM `base_url`/`model`、各类超时与重试数），所以该面仍只能放在可信网络上。
- **`/debug/notify` 受 chat 白名单约束，但空白名单不拦截**：它复用生产同一份白名单快照（`src/notify.rs:1142`），判定条件是「白名单**非空**且 `chat_id` 不在其中」才回 `403 chat_not_allowed`（`src/debug.rs:232-235`）。因此已配置白名单时无法绕过业务侧发送限制；若白名单未配置（为空）则此判定不生效，`chat_id` 可任意指定——所以启用本面时应确认业务白名单已真正配置。`text` 缺省为固定联调文案，并按 1024 字符截断（`src/debug.rs:240-245`）。
- **不在 Worker 白名单内，只能直连 origin**：网关的 15 条安全路由（`cloudflare-worker/src/backends.js:9-25`）不含任何 `/debug/*`，Worker 对未白名单路径返回 `404 route not forwarded: /debug/...`（`cloudflare-worker/src/index.js:81-82`）。因此 `/debug/*` 只能通过直连后端 origin 访问；若必须经代理，请自行在代理层加鉴权，不要让公网可达。

> **建议**：生产环境不开启。需要远程联调时临时开启、用一次性 token，联调结束立即移除 `--debug` 与 `DEBUG_TOKEN` 后重启；长期暴露面走 §5 的 Secret 管理流程单独审批。

---

## 3. Rust 多阶段构建与运行

> **边界：本章与仓库根目录的 `Dockerfile` 是「仅本地开发 / 本地容器调试」路径，
> 生产不执行它。** 线上走 §5.1 的 `hoststack.yaml` + `runtime: rust`，由 HostStack 自带
> agent 在 `rust:slim-trixie` 构建、拷进 `debian:trixie-slim` runner 运行，完全不经过
> 本 Dockerfile（连 Debian 版本都不同）。因此本章的 `ENTRYPOINT`（tini）、
> `EXPOSE 8080`、内置诊断工具在生产容器中一律不生效。生产启动命令的真源是
> `hoststack.yaml` 的 `start.command`。本地用 `docker build` + `docker run` 验证行为时
> 才走本章。

### 3.1 构建阶段
（保留既有 rust:bookworm 构建 + 缓存分层约定；产物为单个静态编译二进制 `message-weave`）

> **实现现状**：单端口 axum 入口、`jmap-client =0.4.2` 只读 adapter、Redis Streams、Telegram Webhook、JMAP Push 入队和 `/reconcile` 已接入；三条入口路由鉴权 fail-closed 落地（`R1`/`SAF-AUTH-*`）。本地 Debian 容器门禁已通过；真实 Stalwart、Redis、Telegram 的端到端链路仍需部署后验证。配置引导模式会在启动变量缺失时保持 HTTP/SPA 可访问，不伪造业务成功。

### 3.2 运行阶段（收敛为单端口）
- `EXPOSE 8080`（遵循 `C-NO-TCP-EXPOSE`，不再暴露 9191 admin 端口）
- `ENV PORT=8080`（可选，缺省即 8080；`REDIS_URL` 与 `CONFIG_ENCRYPTION_KEY` 必须由运行期注入，不写入镜像）
- 健康检查走同一 HTTP 监听（见 §7），不依赖独立端口
- **不声明 VOLUME**，无 `/app/data` 本地卷（`NG-LOCAL-VOLUME`）

---

## 4. 运行时用户 / 证书 / TLS 与公网 HTTPS 入口

- 非 root 用户运行（保留既有约定）
- **公网 HTTPS 入口（`C-HTTPS-URL` 已确认）**：运维在平台上给 bot 配一个公网 HTTPS URL（如 `https://bot.example.com`），平台 ingress/反代把 `https://…/webhook/tg`、`/push/jmap`、`/reconcile` 路由到容器 `PORT`。**bot 自身不申请证书、不监听 443**；证书由平台/反代管理（`C-HTTPS-INBOUND`）。
- 需要在 Stalwart 与 Telegram 两侧使用这个公网 URL：Telegram `setWebhook` 指向 `/webhook/tg`；管理员调用 `POST /api/push/register` 并提交该 URL 的 `/push/jmap` 路径，后端负责执行 `PushSubscription/set create`。服务不会自动猜测平台公网域名。
- **出站 egress 是 `/ready` 的硬依赖**：就绪探针向配置的 JMAP session host 与 `api.telegram.org:443` 发起只读 `GET`。该 egress 不通时 `/ready` 会长期 `503`，即使配置与 Redis 都正常。若 egress 必须经 HTTP(S) 代理则**无法使用**：应用内 `reqwest` 以 `default-features = false` 编译（`Cargo.toml:12`），未启用 `proxy` feature，**不解析 `HTTPS_PROXY`/`HTTP_PROXY`**；这种情况下请用网关聚合的 `/healthz`（无条件 200）作为存活监控，`/ready` 仅作人工排查。
- **多实例 LB/HA 时登记的是 Worker URL 而非各后端 URL**（`C-LB-SINGLE-REG-URL`，§10）：Telegram / Stalwart / Cron 只认 Worker 的稳定域名；后端平台入口不对外登记。
- Secrets 运行期注入（环境变量/容器平台 secret），见 §5（同文件内章节链接）

---

## 5. Secret 管理与环境变量

### 5.1 HostStack 原生 Rust 部署

HostStack 使用仓库根目录 `hoststack.yaml`：`runtime: rust` 的 agent 在
`rust:slim-trixie` 里执行 `install.command`（`cargo fetch --locked`）与
`build.command`（`cargo build --release --locked`），把产物拷进
`debian:trixie-slim` 的 runner 容器，以 `start.command`（`./target/release/message-weave`）
启动，监听单个 HTTP 端口，并以 `/healthz` 做 interval 30 秒、timeout 5 秒的健康检查。
`install:` **是**合法的 schema 键——`install` / `build` / `start` 是三条独立命令，
依赖获取不写进 `build.command`。服务命令里**不带** `--debug`——远程联调开关只应临时加上
（改法见 `hoststack.yaml` 的注释，双因子见 §2.1）。`REDIS_URL` 与
`CONFIG_ENCRYPTION_KEY` 必须配置为 HostStack Secret；不得将密钥值写入 YAML、镜像、日志
或代码仓库。**本仓库的 Dockerfile 不由该路径执行**——runner 镜像是 HostStack 自带的
`debian:trixie-slim`，不是本文件的 `debian:bookworm-slim`，见 §3 与 §8.1 的说明。

**配置优先级（三层，从 HostStack agent 源码实测确认）**：部署时 agent 先把控制台
services 表里已存的值填进 payload，再用 `hoststack.yaml` 声明的值覆盖，最后才用运行时
框架默认值补缺。源码判定条件是 `if (svcConfig.install?.command)` 才覆盖，且没有
`&& !payload.x` 之类的短路——**YAML 声明了就以 YAML 为准、无条件覆盖控制台值；YAML
省略的字段则沿用控制台已存的那个值**。框架默认值只有在前两者都为空时才填，而 agent 的
注释明确写着 *"trigger-payload always populates payload.x from the DB"*，所以对已建好的
服务这条默认值分支实际上是**死代码**。这也意味着删掉本文件**不会**退回安全的默认值——
它直接采用控制台现存的那个值。

**因此目前不能删**。控制台里现存的 Start Command 是 `./target/release/app`，
这是 HostStack 为 Rust runtime 自带的默认值、与本仓库包名无关：`Cargo.toml` 只声明
一个 `[[bin]]`（名字 `message-weave`，路径 `src/main.rs`），仓库里既没有 `app` 这个
二进制目标也没有对应源文件。直接删掉 `hoststack.yaml` 后，下一次部署会以
`./target/release/app` 启动，进程在绑定端口前就以 `not found` 退出。
线上实测印证：控制台 UI 显示 `./target/release/app`，而实际 PID 1 是
`./target/release/message-weave`——说明现在生效的是 YAML，不是控制台。

> **决策记录**：最初倾向是"从控制台 UI 管理更方便，把 `hoststack.yaml` 拿掉"。
> 但按上面的优先级，**这个前提不成立**——直接删 YAML 服务起不来。**当前决定：保留
> `hoststack.yaml`。** 保留的代价是双写（在控制台改命令不会生效，必须改 YAML）；
> 保留的理由是部署命令进入 git 历史、可 review 可回滚，而控制台值散在平台侧、改动无痕迹。
>
> **这个双写不是我们多此一举，命令字段从设计上就不回写控制台**：agent 源码里只有
> `yaml_port_declared` 与 `yaml_disk_declared` 两个回写事件，**命令没有**——每次部署都是
> 拿 YAML 临时覆盖 services 行里的旧值。`port` 与 `disk` 之所以要回写，是因为
> autoscaling 与 sleep-wake 直接读那行；命令只有部署路径自己读，所以不值得持久化。
> 结论是控制台对命令字段永远是过期缓存，**不存在"以 UI 为准"的稳态**——这也是 HostStack
> 把它叫 Infrastructure-as-Code 的原因（源码原文：*hoststack.yaml is
> Infrastructure-as-Code: when the repo declares a command, it wins over whatever happens
> to be cached on the services row*）。
>
> **关于这个文件的来历**：它不是 HostStack 生成的，也没有官方模板可抄——agent 镜像里
> 既没有 `init` 类命令也没有内嵌的模板 YAML，只负责在仓库根读取 `hoststack.yaml` 或
> `hoststack.yml`。本文件是 `a571033` 手写的，所以"官方推荐怎么写"没有标准答案，只有
> schema（见上一条的未知键行为）。
>
> **若日后确要移除**，必须**先在控制台把三条命令改对**（install = `cargo fetch --locked`、
> build = `cargo build --release --locked`、start = `./target/release/message-weave`），
> 再删本文件并重新部署一次验证；同时把本段连同该前提移到 `docs/retired.md` 并记下
> 移除 commit。**不要**只删文件就以为控制台值会自动变对。

**校验是宽松的，不是拒绝式的**：agent 的部署路径不应用 zod schema 校验，
`install` / `build` / `start` 等都是可选对象，**未知键会被静默丢弃而不是报错**，
拼错一个键名（如 `healthcheck` 全小写）不会让部署失败，只会让对应配置悄悄失效。
因此 `hoststack validate`（本地类型检查，不发起 API 调用）是唯一有效的防线，
应放进 CI；本机无 Node 环境时至少保留一次人工 review。

全局业务开关通过受保护的 `GET|PUT /api/enabled` 管理，持久化 Redis key 为
`config:enabled`，默认关闭且读取失败 fail-closed。关闭时 `/webhook/tg`、`/push/jmap`、
`/reconcile`、`/worker` 返回 503，不执行入队、ACK 或业务处理；SPA、`/api/status`、管理
会话和配置 API 保持可用，打开后立即生效。

> 迁移后生产环境仅需 `REDIS_URL` 与 `CONFIG_ENCRYPTION_KEY`。下表字段**已不再从环境变量读取**（`Config::from_env()` 及其助手 `required_secret`/`required_nonblank`/`env_bool` 已删除，`config.rs` 零 `std::env` 读取；登记见 `docs/retired.md`），全部经 Redis 业务配置由 `/api/bootstrap` 或管理员 PUT 写入并在成功后热重建客户端。表内 ✅ 指 **Redis 业务配置**的 fail-closed 必填（`validate_nonblank`，空白即拒绝），不代表需要注入生产容器。**不要把下表变量注入容器**：注入也不会被读取。

（保留既有 Secrets 注入约定：禁入镜像、`secrecy` 包裹、日志屏蔽）

| Redis 业务配置字段 | 必填 | 说明 |
|---|---|---|
| `BOT_TOKEN` | ✅ | Telegram Bot Token |
| `TELEGRAM_CHAT_ID` | ✅ | Worker 元数据通知目标 chat id（与入站白名单分离） |
| `JMAP_SESSION_URL` | ✅ | Stalwart JMAP session URL（`REQ-JMAP-SESSION-URL`）：填**服务基地址**（`https://mail.example.com`）或**完整** `…/.well-known/jmap` 均可；代码归一化为 origin/base 后再交 `jmap-client`，**不产生重复路径**。仅 HTTPS；**禁止 URL 内嵌凭据**（`SAF-JMAP-URL`） |
| `JMAP_USERNAME` / `JMAP_PASSWORD` | ✅ | **Stalwart 认证 = App Password + Basic**（已确认 `C-AUTH-APP-BASIC`）：账号填邮箱，密码填在 Stalwart 生成的**应用专用密码**（可独立吊销/设到期）；不用主密码、不用 OAuth。通常内置 `user` 角色已包含 PushSubscription 的读取/创建/修改/删除权限，无需手工配置；若实际调用返回 `forbidden`，再到 `/admin → Management → Directory → Accounts/Roles` 检查 |
| `CHAT_ALLOWLIST` | ✅ | 聊天白名单（**硬约束 `SAF-CHAT-ALLOWLIST`**）：逗号分隔整数 chat id；处理任何事件前先校验，非白名单直接拒绝 |
| `REDIS_URL` | ✅ | **外部 Redis（用户托管 + AOF）**（`C-REDIS-ONLY-STATE`/`C-REDIS-MANAGED-AOF`）：session / dedup / Streams / fuse / sinceState 全部在此；支持 `redis://` 与带默认 ACL 用户的 `rediss://default:<url-encoded-password>@host:6379/0`，Redis 进程不在此 compose 内 |
| `PORT` | 默认 8080 | 单监听端口（`C-NO-TCP-EXPOSE`） |
| `RECONCILE_TOKEN` | ✅ | `/reconcile` 的 `Authorization: Bearer <token>` 承载令牌（`SAF-AUTH-RECONCILE`）。因 `/reconcile` 路由**始终挂载**，此字段为**必填**（`SecretString`） |
| `WORKER_TOKEN` | ✅ | `/worker` 的有界处理令牌；管理 API 兼容接受该 Bearer 值，SPA 使用短期 Redis admin session |
| `TG_WEBHOOK_SECRET` | ✅ | `/webhook/tg` 校验请求头 `X-Telegram-Bot-Api-Secret-Token`（`SAF-AUTH-TG-WEBHOOK`）。须与 Telegram `setWebhook` 的 `secret_token` **完全一致**（`SecretString`） |
| `--debug` + `DEBUG_TOKEN` | 可选，默认关闭 | 远程联调面 `/debug/*` 的**双因子开关**（`SAF-DEBUG-GATE`）：命令行必须带 `--debug` **且** `DEBUG_TOKEN` 非空才挂载 7 条路由，缺一即路由不存在、请求走通用 `404`。令牌为 `SecretString`，常数时间比较、不进日志（`SAF-LOG-PURITY`）。生产环境不配置，见 §2.1 |
| Push registration/verification | 已接入，需显式注册 | `POST /api/push/register` 接受 HTTPS callback URL 并调用 `PushSubscription/set create`；相同 callback URL 重复请求幂等复用；`/push/jmap` 接收 Stalwart 生成的验证码并自动回写，Redis 保存订阅 ID 和短期验证状态 |
| `LLM_*` | 可选 | OpenAI-compatible 业务配置字段（`REQ-LLM-OPENAI-COMPAT`）；其中 `LLM_MAX_RETRIES` **不是**业务配置字段而是 Redis 运行参数（回落默认见 `docs/reference.md` §6.1）；**仅当用户明确允许时才把邮件正文外发 AI**（`REQ-AI-EXTERNAL-CONSENT`） |
| `ACCOUNT_ID` | 默认空 | **单账户**（`REQ-SINGLE-ACCOUNT`）：留空则取 session 主账户；多账户 = 部署多个 bot 实例（各自独立 token/配置），不做多账户单实例 |

AI 授权期限由用户选择（临时一次、今天、7天或直到撤销），Redis 仅保存 chat id、授权状态和带 TTL 的到期时间；不会保存正文或摘要。到期后摘要请求回到元数据模式并提示重新授权。

> 业务鉴权变量由 Redis `config:business` 管理，不再要求作为启动环境变量。缺少 `REDIS_URL` 或非法/缺失 `CONFIG_ENCRYPTION_KEY` 时服务仍监听并提供 SPA、`/api/status` 与探针，状态为 `configuration-setup`，不会伪造持久化成功；配置恢复后再启用业务入口鉴权。
> 示例占位见仓库根目录 [`.env.example`](../.env.example)（仅占位符，**严禁**放入真实密钥）。

> **配置管理页面**：服务根路径 `/` 提供嵌入 Rust 二进制的 SPA。输入 `CONFIG_ENCRYPTION_KEY` 后，`POST /api/admin/session` 签发 900 秒 admin session；页面只在内存中保存 opaque session。运行参数通过 `/api/config` 读取和保存；完整业务配置通过 `PUT /api/business-config` 替换并热加载。业务配置 API 不提供 GET，密钥不会回显；每次完整替换都需重新输入必填密钥。配置保存在外部 Redis（`C-REDIS-ONLY-STATE`），静态资源编译时随二进制打包，无运行期本地文件。

---

## 6. 运行模式与短请求模型

### 6.0 Redis-only 配置迁移与 bootstrap 威胁模型（`C-REDIS-ONLY-STATE`）

生产进程接受 `REDIS_URL` 与唯一额外启动密钥 `CONFIG_ENCRYPTION_KEY`（32 字节随机
高熵 hex，仅应用运行时持有）；bootstrap 与 admin 会话的认证凭据即 `CONFIG_ENCRYPTION_KEY`
本身；业务密钥和运行参数不再从环境变量读取。空 Redis 仅提供配置页面及一次性的 bootstrap
会话：请求必须以 `CONFIG_ENCRYPTION_KEY` 作为 Bearer 凭据，服务端只做常数时间比较，绝不在
响应、日志或配置值中回显该密钥。bootstrap 使用原子 `SET NX` 写入完整业务配置，
竞争请求只有一个成功；**ACL 密码缺失不再阻塞初始化**（TLS-only 托管 Redis 场景可用），
仅 `CONFIG_ENCRYPTION_KEY` 缺失或鉴权失败时拒绝。

该 ACL 身份是应用自身的 Redis 连接凭据，仍必须具备 `config:business`、
`config:outbound` 及 `config:admin_session` 键的读写权限（bootstrap 与管理员 PUT 均经此
连接写入），不得误配置为禁止配置键访问；但它不再是任何 HTTP 认证的信任根——业务配置
明文受 `CONFIG_ENCRYPTION_KEY` 做 AES-256-GCM 加密保护，仅持有 ACL 密码读取到的也是
密文。bootstrap 完成后公网业务配置接口只接受 900 秒 admin session。

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

> **单一运行形态**：进程只有单条服务命令 `./target/release/message-weave`，webhook handler、JMAP Push 回调、Redis Streams worker 与 `/reconcile` 同处一个进程；对账是一次性 HTTP 端点 `POST /reconcile`（由外部调度器/容器任务调用），**不是**独立子命令，也**没有** `RUN_MODE` 变量（该标识符已随 `Config::from_env()` 删除，见 `docs/retired.md`）。无长连接（`C-NO-LONG-CONN`）。

### 6.1 入口路由（单端口 `PORT`，全部经平台 HTTPS URL 入站）
```
# 平台公网 URL: https://bot.example.com  → 反代 → 容器 127.0.0.1:8080
GET  /                  业务配置与运行参数管理 SPA
GET  /assets/config.js  SPA 脚本；GET /assets/styles.css  SPA 样式
GET  /api/status              公开启动状态；只返回 ready/mode/missing 环境变量名
POST /api/admin/session       Bearer CONFIG_ENCRYPTION_KEY；成功返回 900 秒 admin session
POST /api/admin/session/revoke Bearer admin session；成功返回 204
GET|PUT /api/config           Bearer admin session 或 WORKER_TOKEN；Redis 错误返回 503
PUT /api/business-config      Bearer admin session 或 WORKER_TOKEN；完整替换，成功返回 204
POST /webhook/tg      TG Webhook → [鉴权 SAF-AUTH-TG-WEBHOOK: 头 X-Telegram-Bot-Api-Secret-Token]
                      → 快速 2xx ACK → 幂等去重(MOD-DEDUP) → 命令处理 → 同步回复（快路径）
POST /push/jmap       Stalwart Push 回调 → [鉴权 SAF-AUTH-JMAP-PUSH: Body pushSubscriptionId + verificationCode]
                      → 幂等去重(MOD-DEDUP) → 入 Redis Streams(MOD-STREAMS) → 立即 2xx ACK（慢任务异步）
POST /reconcile      外部 HTTPS Cron 触发 → [鉴权 SAF-AUTH-RECONCILE: Authorization: Bearer RECONCILE_TOKEN]
                      → 对账补差 FLOW-RECONCILE
GET  /healthz        liveness（ARCH-HEALTHZ：进程存活；公开探针 SAF-PROBE-PUBLIC，无鉴权、无敏感信息）
GET  /ready          公开就绪探针；检查配置完整性 + Redis 可达性 + 出站只读探测（JMAP `/.well-known/jmap` 带 Basic 认证、Telegram getMe，各 3s），四者全过返回 200+就绪报告 JSON，任一失败返回 503+标准错误 envelope（无鉴权、无敏感信息）
```
`/reconcile` 使用 Redis owner-token 单飞锁并在长任务期间续租；每次最多处理 100 页、10,000 封基线邮件或 20 秒。已完成入队的 changes 页会提交最新游标；冷启动基线超出预算时把基线状态与位置编码后持久化，下一次从断点继续，不从头扫描，也不会提前跳过未列举邮件。
> **写入口鉴权（fail-closed，`SAF-AUTH-*`）**：Webhook、Push、Reconcile 以及 Push 注册接口都必须先通过鉴权，**失败返回 `401` 且不产生副作用**；比较使用常数时间（`subtle`，防时序侧信道）。secret 未配置 → 启动失败，**无"缺省放行"**。
> **健康探针（`SAF-PROBE-PUBLIC`）**：`/healthz`、`/ready` 仅返回健康状态、**不含敏感信息**。`/healthz` 只表示进程存活；`/ready` 检查配置完整性与 Redis 可访问性，并发起只读出站探测（JMAP `/.well-known/jmap` 带 Basic 认证 与 Telegram getMe，各 3s、并行）；不触发邮件同步等业务副作用。
> **"公网 HTTPS 入口"是什么**（`C-HTTPS-URL`）：平台给 bot 一个公网 HTTPS 域名，外部（Telegram / Stalwart / 调度器）通过它访问上面这些路径；容器只处理明文 HTTP，TLS 由平台终止。运维只需在平台上配置域名/证书并确保 4 条路径可达。

### 6.2 Redis Streams / worker（MOD-STREAMS）
- Push 回调只做：校验 → 去重 → 入队 → ACK；不阻塞
- worker（同容器后台 task 或独立 worker 容器，2 选 1 均由 compose/编排决定）：
  `XREADGROUP → Email/changes → 推送 TG → 推进 sinceState → XACK`
- 消费组 at-least-once：未 ACK 消息自动重投；处理幂等（MOD-DEDUP 二次兜底）

### 6.3 外部 HTTPS Cron 对账（FLOW-RECONCILE）
- **"外部 Cron"是什么**（已确认）：bot **不自建定时器、不持有调度**（`C-NO-LONG-CONN`，无 `tokio-cron`）。由**外部调度器**周期性发起 `POST https://<平台URL>/reconcile`（带 `RECONCILE_TOKEN`）。端点执行鉴权、全局开关检查与 Redis 单飞锁，调用 JMAP `Email/changes` 分页并将事件幂等入 Streams；全部入队成功后才持久化 `state:jmap:since`，依赖失败返回 `503` 供调度器重试。
- **建议间隔 5–10 分钟**（`NFR-RECONCILE-INTERVAL`）：兼顾"少延迟"与"低开销"；这是可用性的兜底频率。
- 可用调度器（任选其一，均为外部）：系统 crontab+curl / k8s CronJob / GitHub Actions scheduled / 第三方 cron 服务。
- 对账逻辑：用 `sinceState` 调 `Email/changes` 拉增量 → 与已处理 email_id 求差 → 补发通知 → 推进 `sinceState`。
- **Redis 丢失恢复**：sinceState 存 Redis（`MOD-SINCESTATE`，AOF 持久化 `C-REDIS-MANAGED-AOF`）；即便 Redis 全丢，对账扫描 JMAP（`Email/query` 最近 N 封 + changes）也能重建游标并补发——**事实源在 JMAP，Redis 只是加速层**。

#### 6.3.1 用户侧调度示例（copy-paste 可用）

端点：`POST https://<你的平台URL>/reconcile`。鉴权：`Authorization: Bearer <RECONCILE_TOKEN>`
（`src/config.rs:147` fail-closed 必填，服务端常数时间比较，`notify.rs:228-231`）。

最小示例：

```bash
curl -sS -o /dev/null -w '%{http_code}\n' \
  -X POST https://<你的平台URL>/reconcile \
  -H "Authorization: Bearer $RECONCILE_TOKEN" \
  --max-time 60
```

调度器任选其一（均为外部，容器不自建定时器）：

```cron
# 系统 crontab，每 5 分钟
*/5 * * * * curl -fsS -m 60 -X POST https://<你的平台URL>/reconcile -H "Authorization: Bearer $RECONCILE_TOKEN" >/dev/null
```

```yaml
# Kubernetes CronJob，每 5 分钟
apiVersion: batch/v1
kind: CronJob
metadata:
  name: messageweave-reconcile
spec:
  schedule: "*/5 * * * *"
  concurrencyPolicy: Forbid
  successfulJobsHistoryLimit: 1
  failedJobsHistoryLimit: 1
  jobTemplate:
    spec:
      template:
        spec:
          restartPolicy: Never
          containers:
            - name: reconcile
              image: curlimages/curl:8
              # 用 shell 启动，才能展开 $RECONCILE_TOKEN；直接 exec curl 时 $(…) 不会被替换
              command: ["/bin/sh", "-c"]
              args:
                - 'curl -fsS -m 60 -X POST "https://<你的平台URL>/reconcile" -H "Authorization: Bearer $RECONCILE_TOKEN"'
              env:
                - name: RECONCILE_TOKEN
                  valueFrom:
                    secretKeyRef:
                      name: messageweave
                      key: reconcile-token
```

**期望状态码**

| 状态码 | 含义 | 调度器怎么办 |
|---|---|---|
| `204` | 成功，游标已推进（无响应体） | 正常，不做任何处理 |
| `401` | `RECONCILE_TOKEN` 缺失或错误 | 不要重试；检查凭据 |
| `409` | 单飞锁被占用，另有实例正在对账 | 不要重试；等下一个调度周期 |
| `503` | 业务开关关闭，或 JMAP/Redis 故障 | 按 `Retry-After: 30` 头重试，或等下一个周期 |

失败响应体统一为 `{"error": <code>, "request_id": <实例 owner id>}`，可直接落日志。

**重试建议**

- 对账逻辑本身是幂等的：重复调用只会重复走 `Email/changes` 增量并二次去重入队（`MOD-DEDUP`
  兜底），不会重复通知。所以对 `503` 可以放心重试。
- `204` 与 `409` 都是正常结果，不要把它们当失败重试——尤其别对 `409` 做紧密循环重试。
- 调度间隔按 `NFR-RECONCILE-INTERVAL` 取 **5–10 分钟**即可。Push 是主路径，对账只是兜底。
- 走 Worker 转发**无需特殊设置**：网关已为 `POST /reconcile` 单独覆盖超时与尝试次数——超时取
  `LB_RECONCILE_TIMEOUT_MS`（默认 `320000` ms，大于单飞锁租期 300 s + 心跳余量；锁续租逻辑见
  `notify.rs:240`/`252`），且 `maxAttempts=1` **绝不故障转移**（故障转移只会让第二实例立刻返回
  `409`）。其余快路径仍用全局 `LB_REQUEST_TIMEOUT_MS`（默认 `10000` ms）与 `LB_MAX_ATTEMPTS`
  （默认 `2`），不受影响。

### 6.4 可靠性策略（Reliability）

> 目标：**至少 99.9% 通知可用性**（`NFR-NOTIFY-SLA`），允许少量延迟（不追求秒级保证）。下述机制共同保证"不丢、少重、可恢复"。

| 机制 | ID | 做法 |
|---|---|---|
| **Streams ACK / 重试** | `MOD-STREAMS` | worker 用消费组 `XREADGROUP`；处理成功才 `XACK`；未 ACK 消息在被认领后重投（at-least-once）。处理失败时**不 ACK**，自然重试；设最大投递次数上限，超限转死信（`XADD` 到 `dlq`）并告警，避免毒丸阻塞 |
| **幂等去重** | `MOD-DEDUP` | 以 `(account, email_id)` 为幂等键，`SET NX`（TTL 覆盖重投窗口）。重复投递直接跳过，保证 at-least-once 下**不重复通知**。TG 侧同理用 `update_id` |
| **Push 重试** | `FLOW-NEW-MAIL` | `/push/jmap` 校验/入队后**立即 2xx**；若入队失败（Redis 抖动）返回非 2xx，让 Stalwart 按自身策略重试；配合对账兜底 |
| **对账恢复** | `FLOW-RECONCILE` | 外部 Cron 每 5–10 分钟调用 `/reconcile`；从 Redis 恢复 `state:jmap:since`，按 `Email/changes` 补差，入队成功后推进游标，失败返回 `503` |
| **运行监控 / 告警** | Uptime Kuma | 使用 HTTP(s) Monitor 检查 `/healthz`（进程存活，无条件 200）和 `/ready`（配置 / Redis / 上游可达就绪），分别期望 200；对 503、超时、TLS/DNS/路由故障告警。`/ready` 最坏约 3s（两个 3s 探针并行），探针超时设 ≥10s。应用不引入 Prometheus、Exporter 或额外指标端口。 |

### 6.5 99.9% 可用性目标与边界（NFR-NOTIFY-SLA）

- **定义**：在外部依赖（Stalwart / Telegram / Redis）可用的前提下，通知链路的可用性目标 ≥ **99.9%**（约每月 ≤43 分钟不可用）。
- **允许的延迟**：用户已确认**接受少量通知延迟**——正常情况下 Push 回调应为秒级；依赖抖动/需对账兜底时，延迟上界为**对账间隔（≤10 分钟）**。
- **非目标边界**：99.9% 是**通知可用性**目标，不含 Stalwart/Telegram/Redis 自身故障时间；三者任一长时间不可用属外部依赖故障，不计入本 bot 的可用性预算（但 bot 应在恢复后经对账自动补齐）。
- **降级行为**：Redis 不可用时，去重/会话退化为进程内短窗口、Push 回调返回非 2xx 交 Stalwart 重试；恢复后由对账补齐。**不因 Redis 抖动而漏发最终通知**（以对账为准）。

---

## 7. 健康检查（通用 HTTP 约定）

- `GET /healthz` → liveness（`ARCH-HEALTHZ`；进程存活，长期语义）
- `GET /ready` → 端到端就绪探针（`ARCH-READY-BASELINE`）：检查配置完整性 + Redis 可达性 + 出站只读探测（`GET {jmap_origin}/.well-known/jmap` 带 Basic 认证、`GET https://api.telegram.org/bot<token>/getMe`，各 3s 超时、**并行**（`tokio::join!`），最坏约 3s）；四者全过 `200`（就绪报告 JSON 含真实 `jmap`/`telegram` 字段），任一失败 `503`（标准错误 envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`）。因此**LB / ingress 的探针超时必须 > 3s（建议 ≥10s）**。Uptime Kuma 按状态码（期望 200）监控，不受响应体变化影响。
- 两者均为**公开探针**（`SAF-PROBE-PUBLIC`）：无鉴权、仅返回健康状态、**不含敏感信息**。
- `/ready` 已对 JMAP session 与 Telegram getMe 做**真实出站探测**（3s/个），但不覆盖真实消息投递验收；编排与 Uptime Kuma 可用它做端到端就绪探测，注意其最坏约 3s，LB 探测超时需 > 3s（见上）。
- Docker `HEALTHCHECK` 指向同一监听端口（`wget`/`curl` 探同端口），**不依赖独立端口**（`C-NO-TCP-EXPOSE`）。二进制没有 `health` 子命令可指，见 §8.1 的说明
- 仅保留通用平台映射（compose `HEALTHCHECK`、k8s probe）；不写 Cloud Run/Fly 等特指内容（`NG-SERVERLESS-BIND`）

---

## 8. CI 发布与镜像验收

### 8.0 代码门禁（`GATE-P0`，进入阶段1前必须通过）
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

以上三条**在 Debian `rust:1-slim-bookworm` 容器内执行**（`C-DEBIAN-SLIM`）；详见 design.md §10.0。

容器以 `--user 1000:1000` 运行，并把 `RUSTUP_HOME`/`CARGO_HOME` 挂载到仓库内持久缓存目录（`-v "$PWD/.gate-cache/rustup:$RUSTUP_HOME" -v "$PWD/.gate-cache/cargo:$CARGO_HOME"`），否则缓存写入因权限失败、工具链每次重建；`.gate-cache/` 已在 `.gitignore` 中，不得提交。门禁结果必须基于**当前仓库快照**重跑得到，不得复用先前快照的通过结果。

（其余 CI：镜像构建 → 镜像扫描 → 非 root → `/ready` 端点到容器验证；运行方式见 8.1）

### 8.1 运行方式与就绪探测

> **本节的两条部署路径不要混淆**：上面的构建/运行命令是本仓库 **Dockerfile 路径**（本地 `docker run`
> 或其他容器宿主机）；线上 HostStack 部署走 §5.1 的 `hoststack.yaml` + `runtime: rust` agent，
> 在 `rust:slim-trixie` 里构建、拷进 `debian:trixie-slim` 的 runner 容器运行，
> **完全不执行本 Dockerfile**——两者连 Debian 版本都不同（bookworm vs trixie）。
> 服务 argv 是 `./target/release/message-weave` 而不是镜像里的
> `tini -- /usr/local/bin/message-weave`，因此镜像内的诊断工具与 `ENTRYPOINT` 在线上都不生效。
> 判定当前跑的是哪条路径，看容器内 `/proc/1/cmdline` 与 `/etc/os-release`。

> **本仓库没有 `docker-compose.yml`。**此前本节给过一段 compose 片段，其中的 `healthcheck` 写成
> `message-weave health --addr 127.0.0.1:8080` —— 这个子命令不存在（`src/main.rs` 无任何 CLI
> 参数解析，二进制只起 axum 服务）。照抄那段示例必然得到：healthcheck 永久失败，或容器被判定
> healthy 却什么都不做。示例已删除，改为下面的事实约定。
>
> 镜像（`runtime` 阶段）现已内置 `curl`，因此在 **Dockerfile 部署路径**下用 `HEALTHCHECK` 探活在
> 技术上可行；HostStack 官方文档也把 Dockerfile `HEALTHCHECK` 定为把新副本加入 Traefik 池前的
> readiness 关卡。但 `runtime: rust` 的 HostStack agent 不走本 Dockerfile，该路径的 readiness 由
> `hoststack.yaml` 的 `healthCheck.path` 决定，所以仍以平台探针为准——`/ready` 是唯一同时校验
> redis/jmap/telegram 的信号，`HEALTHCHECK` 只能证明进程活着。

**运行**

```sh
cp .env.example .env           # 仅需 REDIS_URL 与 CONFIG_ENCRYPTION_KEY
docker build -t messageweave:latest .
docker run --env-file .env -p 8080:8080 messageweave:latest
```

- 单端口 8080；平台 ingress/反代映射 HTTPS → 8080（`C-HTTPS-INBOUND`）。
- runtime 阶段为 `debian:bookworm-slim`，非 root（`useradd --system`，实测 uid 999，非 1000）；
  无本地 volume、无数据库引擎（`C-NO-LOCAL-WRITE` / `C-NO-DB`）。
- 镜像内置只读诊断工具：`curl`、`procps`（`ps`/`pgrep`/`free`）、`iproute2`（`ss`/`ip`）、`jq`、
  `netcat-openbsd`（`nc`），合计实测 **19.7 MB**（全量文件系统 `du` 对比
  `debian:bookworm-slim`）。容器 rootfs 只读，这些工具**只能在构建期装入**，
  运行期无法 `apt` 安装——这也是它们写进 Dockerfile 而不是留给运维现场装的原因。
  刻意**不装** `bind9-dnsutils`（`dig`）：它连带 `libicudata.so.72` 等多级依赖要多花
  **42.7 MB**，是其余 7 个工具总和的两倍多；DNS 解析用基础镜像自带的 `getent` 即可
  （`getent hosts <h>` 出 A 记录，`getent ahostsv4` / `getent ahostsv6` 分开取）。
  **但这条只对 Dockerfile 部署路径成立**：`runtime: rust` 的 agent 在
  `rust:slim-trixie` 里构建、把产物拷进 `debian:trixie-slim` 的 runner 容器运行，
  **完全不执行本 Dockerfile**，因此这些工具在 HostStack 服务里并不存在（线上实测
  `pgrep: not found`）。该路径下改用内核接口诊断：`/proc/1/cmdline`（实际 argv）、
  `/proc/1/environ`（实际 env）、`/proc/net/tcp`、`/proc/<pid>/fd`。
  HostStack 生产部署的容器安全档（由 agent 镜像内的部署执行器源码实测确认）为
  `readonlyRootfs: true` + `dropCapabilities: true` + `noNewPrivileges: true` +
  `pidsLimit: 256`；`/tmp` 与 `/var/tmp` 是 `rw,nosuid,nodev,size=64m` 的 tmpfs——
  注意**没有** `noexec`，但重启即清空，所以不要在那里放可执行文件或期望重启后仍在的文件。
  **dev 环境与此不同**：dev 档为 `readonlyRootfs: false`、`noNewPrivileges: false`，
  rootfs 可写，同一个二进制在 dev 与生产的可写行为可能不同。
- Redis 由外部已认证实例提供，不与此服务同容器运行（`C-REDIS-EXTERNAL`）。

**就绪探测：用 `/ready`，不要用 `/healthz`**

| 端点 | 语义 | 用途 |
|---|---|---|
| `GET /healthz` | 无条件 200 | 存活探测（进程还活着） |
| `GET /ready` | 配置、Redis 或上游探针失败时 503 | 就绪探测、负载均衡摘除、Uptime Kuma |
| `GET /api/status` | 缺必需环境变量时 503 + `{"status":"configuration-setup","missing":[...]}` | 排查"起来了但没干活" |

> **由平台 ingress 探测 `/ready`。**镜像现已内置 `curl`（见 Dockerfile `runtime` 阶段），
> 所以在 Dockerfile 部署路径下容器内写 `HEALTHCHECK` 在技术上可行（HostStack 的 `runtime: rust`
> 路径不走该 Dockerfile，readiness 由 `hoststack.yaml` 的 `healthCheck.path` 决定）；但
> `/healthz` 只能证明进程活着，`/ready` 才同时校验 Redis 与上游，因此仍以平台探针为准。

**缺必需环境变量不会崩溃，但也不会干活。**缺 `REDIS_URL` 或 `CONFIG_ENCRYPTION_KEY` 时，进程
降级为只读配置路由（内存态、空 token、`NoopWorker`），容器继续应答请求但不做任何业务。排查这类
现象：看启动日志告警，或查 `GET /api/status` 返回的 `missing` 列表。

### 8.2 镜像与部署验证检查项（红线自检）

以下检查在每次发布前/CI 中执行，用于固化 §0 新增红线：

| # | 检查项 | 关联 ID | 命令/方法 |
|---|---|---|---|
| 1 | **镜像内无数据库引擎**：不装/不含 SQLite、Postgres、MySQL 二进制或数据文件 | `C-NO-DB` | `docker run --rm <image> sh -c 'command -v sqlite3 psql mysql 2>/dev/null; [ -z "$(find / -maxdepth 4 -name "*.db" -o -name "*.sqlite*" 2>/dev/null)" ]'` 应为空 |
| 2 | **容器内不出现本地可写挂载**：compose/k8s 不声明 `volumes:` 用于数据/日志目录 | `C-NO-LOCAL-WRITE` / `NG-LOCAL-VOLUME` | 人工/CI lint 检查 compose 文件、k8s manifest 不含本地 volume 声明 |
| 3 | **日志仅 stdout/stderr**：应用启动参数不得含 file/rolling-file 后端；tracing 配置为 `stdout` | `C-LOG-STDOUT-ONLY` | 检查代码：`grep -rEn 'rolling\|FileAppender\|RollingFileAppender' src/` 应为空；`RUST_LOG` 目标不含 `file:` |
| 4 | **日志/Redis 无敏感数据**：日志字段白名单 + CI 扫描 | `SAF-LOG-PURITY` | 代码评审 + 静态扫描（如 `gitleaks` 扫描 `Redis` 写入调用，验证 `SETEX`/`SET` 参数无密钥/正文/AI 内容） |
| 5 | **重启恢复不依赖进程内状态**：所有"续跑"逻辑必须读 Redis；进程内缓存仅性能优化 | `C-NO-STATEFUL-RECOVERY` | 代码评审：`grep -rn 'static mut\|lazy_static' src/` 不得用于生产恢复路径 |
| 6 | **Redis 由外部提供**：容器不运行 `redis-server`；compose 不启动 Redis 服务 | `C-REDIS-MANAGED-AOF` | `docker run --rm <image> sh -c 'command -v redis-server && exit 1 \|\| exit 0'` 应为 0 |

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
| `C-HTTPS-URL` | **公网 HTTPS 入口由平台提供**（平台给 bot 一个 HTTPS URL；bot 不持证书、不监听 443）；`Q-DEP-A` 已决策：具体平台 URL / 域名与证书配置方（平台自动证书 or 自管反代二选一）由部署环境在发布时确定，仓库文档化两条路径与 4 条路径的可达性验证方式，不替部署方做选择 |
| `NFR-RECONCILE-INTERVAL` | **外部 Cron 定期 HTTPS POST `/reconcile`，建议 5–10 分钟**；容器不自建定时器；`Q-DEP-B` 已决策：调度器不限定实现，任意 shell / cron / K8s CronJob / CI scheduled 均可，用户侧示例见 §6.3.1 |
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

> **最后一项待确认已清空**：`Q-DEP-A`（平台 URL / 域名与证书配置方）与 `Q-DEP-B`（外部调度器选型）
> 均已决策，分别合并进本表的 `C-HTTPS-URL` 与 `NFR-RECONCILE-INTERVAL` 行。两项都是上线时的运维选择，
> 不影响代码结构、不影响门禁；`docs/roadmap.md` 已不再保留「决策待定」块。
>
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

- Worker **原样转发** header / body / `pushSubscriptionId` / `verificationCode`，**不做鉴权改写**。
- **后端必须继续 fail-closed 校验**三条入口（`SAF-AUTH-*`）——**不可省**。原因：小平台通常**不提供防火墙/ACL**，后端 HTTPS 入口可能被公网直连；因此"仅靠 Worker 防护"不成立（方案 B「后端不对公网暴露」在本场景**不可行**）。
- **多实例必须共享业务鉴权配置**（`C-LB-SHARED-SECRETS`）：`TG_WEBHOOK_SECRET` / `RECONCILE_TOKEN` 在所有实例上完全一致；Push verification 状态由 Redis 共享。

### 10.4 路由与故障转移

- **路由 safelist**（`C-LB-SINGLE-REG-URL`）：Worker 只透传 **15 条**（`SAFE_ROUTES` at `backends.js:9-25` + `ROUTE_METHODS` at `index.js:45-61`）：`GET /`、`/assets/config.js`、`/assets/styles.css`、`GET /api/status`、`GET|PUT /api/config`、`PUT /api/business-config`、`POST /api/admin/session`、`POST /api/admin/session/revoke`、`GET|PUT /api/enabled`、`POST /webhook/tg`、`POST /push/jmap`、`POST /api/push/register`、`POST /api/push/disable`、`POST /reconcile`、`GET /ready`；**未知路径 404、method 不符 405**，不透传至后端。**后端另有 4 类不在 safelist**：`POST /api/bootstrap`（一次性信任引导，**不能**走 Worker 域名）、`POST /worker`（运维手工触发的 worker 端点，Bearer 鉴权）、`GET /healthz`（网关自行聚合，不转发）、`/debug/*`（7 条远程联调面，`SAF-DEBUG-GATE`，只能直连后端 origin，见 §2.1）——**这 4 类都不承载外部业务流量，因此后端实例前不需要第二道入口**。`GET|PUT /api/enabled` 属于必须透传的例外：管理 SPA 本身只部署在 Worker 域名下，业务总开关由它读取与切换（`loadEnabled` 在 `web/config.js:586` 读、开关写入在 `:958`），缺了这条白名单，SPA 里那个开关在后端直连模式下可用、经网关部署时恒 404；该路由已强制 admin-session Bearer 鉴权，暴露面与同在白名单内的 `/api/admin/session` 完全一致。`/api/status` 只返回启动状态和缺少的环境变量名；所有管理 API 的 Bearer 鉴权由后端执行（`SAF-LB-PASSTHRU`）。
- **健康聚合（`MOD-HEALTH-AGG`）**：Worker 自行承载 `GET /healthz`，按 TTL 缓存（默认 30s，`LB_HEALTH_TTL_MS` 可调）探测各后端 `/healthz`，返回 `{status, available, total, backends:[{origin,up,status}]}`；≥1 后端 up → 200，全 down → 503。`/ready` 透传给后端，做配置 + Redis + 出站只读探测（JMAP session、TG getMe，各 3s、并行，最坏约 3s），不触发业务副作用。
- **故障转移（`forwardWithFailover`）**：每次请求最多 `min(LB_MAX_ATTEMPTS, origins.length)` 次尝试；**仅**超时（AbortError）或 5xx 触发换下一个 origin；4xx/2xx/3xx 直接返回；默认 `LB_MAX_ATTEMPTS=2`（首次 + 1 次故障转移）。**例外：`POST /reconcile` 固定 `maxAttempts=1`，绝不故障转移**——它持集群级单飞锁，切实例只会立刻撞 `409`；其单请求超时取 `LB_RECONCILE_TIMEOUT_MS`（默认 `320000` ms），其余路由不受影响。
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
| `POST /api/push/register` | 幂等（相同 callback URL 复用） | 已进 safelist，可走 Worker 域名；callback URL 由调用方提供、订阅记录全存共享 Redis，落到哪个后端实例都不影响 |
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
npx wrangler secret put LB_RECONCILE_TIMEOUT_MS # 默认 320000ms，仅 POST /reconcile 生效
npx wrangler secret put LB_HEALTH_TTL_MS        # 默认 30000ms
```

`BACKEND_ORIGINS_JSON` 示例（**仅允许 `https://` origin**，其它会被 Worker 启动即 503，`C-HTTPS-INBOUND`）：

```json
[
  {"url":"https://messageweave-a.example.com","weight":100},
  {"url":"https://messageweave-b.example.com","weight":100}
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
- Worker **不落地** JMAP/Telegram 的 secret；Push verification 由后端通过 JMAP API 动态完成并只在 Redis 短期保存状态。

---

## 附：稳定 ID 引用索引

> 跨文档稳定 ID 的**唯一权威索引表**在 `docs/charter.md` §8「稳定 ID 注册表」。
> 本文使用到的 ID（`C-*` / `NG-*` / `MOD-*` / `FLOW-*` 等）均在其中登记定义点位置与一句话说明；内容搬家时**只改 charter.md 索引表**的"文件/锚点"列，所有引用本身零改动。
