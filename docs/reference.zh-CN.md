# MessageWeave — 参考

> [English version / 英文版 → reference.md](reference.md)

> **此文件是可验证事实的单一事实来源。**
> 当任何其他文件与本文件不一致时，以本文件为准。
>
> **根据设计，该文件不包含源行号。** 它们不属于
> 验证：此文件记录*哪个符号*执行*什么*，行号是
> 对同一文件的每次不相关的编辑都会重新编号。 `docs/deployment.md` 保留其
> 行号，因为该文件是操作员在事件中读取的运行手册。
>
> **维护责任。** 对 `src/config.rs` 公共 API 的任何更改，
> `src/state.rs`、`src/worker.rs` 或 `src/notify.rs`，或 **任何 Redis 键名称或 TTL**，
> 必须在同一更改中更新此文件。审阅者：拒绝涉及这些的更改
> 文件而不触及此文件。

全文使用的约定：

- **TTL** 以秒为单位。 “No EX”意味着密钥永不过期，并且在明确删除之前一直有效。
- **写入器/读取器** 命名执行操作的代码符号。符号名称是
  身份；这个文件故意不记录行号（见上面的注释），所以
  重新编号后仍然如此。

---



---

## 1. Redis 键

### 1.1 配置

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `config:business` | 无 EX | `set_business_config`；首次写入通过 `initialize_business_config` 内的 `SET NX` | `get_business_config` |
| `config:business:revision` | 无 EX | 每次配置写入做原子 `INCR`；首次初始化用 `SET 1 NX` | `business_config_revision` |
| `config:outbound` | 无 EX | `set_outbound_config` | `get_outbound_config` |
| `config:enabled` | 无 EX | `set_enabled` | `is_enabled` |
| `config:admin_session` | EX 1800（30 分钟） | `put_admin_session` | `admin_session_valid`;由 `revoke_admin_session` 清除 |

笔记：

- `config:business:revision` 是一个**原子 Redis `INCR`**，而不是读取-修改-写入。每个
  承载请求的入口点调用“refresh_business_config”，这仅
  当远程修订超出缓存的本地值时重建工作程序 - 即
  Guard 可以防止陈旧或格式错误的快照变成重建循环。
- `config:admin_session` 的 TTL 由写入时传入的 `EX 1800` 决定；处理程序也在响应正文中
  通过 `expires_in: 1800` 返回相同的有效期。
- `config:enabled` 被视为一个门；缺失或“错误”会导致业务处理中断。

### 1.2 AI 授权

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `consent:ai:{chat_id}` | EX 3600 / 86_400 / 604_800 / 31_536_000 | `set_ai_consent` | `ai_consent_until`;由 `clear_ai_consent` 清除 |

存储的值是**绝对 Unix 到期时间戳**，而不是持续时间；钥匙是“EX”
具有相同的持续时间，因此密钥会自行删除。

触发词是精确的中文文字，**没有英文别名** - 应用程序内帮助文本
本身强制执行（“worker.rs”中的应用内帮助文本）：

|输入 | TTL |标签|
|---|---|---|
| `/ai on`, `/ai yes`, `开启 ai`, `同意摘要`, `允许 ai` | 3600 | `1小时` |
| `临时`、`一次`（子字符串）| 3600 | `临时1小时` |
| `今天` | 86_400 | `今天` |
| `7天` | 604_800 | `7天` |
| `直到我撤销`, `长期`（子串）| 31_536_000（365 天）| `直到撤销（最长365天）` |
| `/ai off`, `关闭 ai`, `撤销授权`, `停止摘要` | — |撤销|

`/ai ...` 斜杠形式与整个消息完全匹配；中文短语是
子字符串匹配消息中的任何位置（都在“parse_intent”、“worker.rs”内）。

`1小时` / `临时1小时` / `直到撤销（最长365天）`是**类别标签，而不是触发词**：
单独输入标签不会带来任何好处。同意永远不会自动续订。

相同的“parse_intent”（“worker.rs”，“Intent”的变体）路由“/search <关键词>”
以及中文前缀 `搜索`、`查找`、`检索`（只做前缀匹配，绝不做子串匹配），
路由到 `Intent::Search`。搜索不授予授权，也不写任何 Redis 键。

### 1.3 投递流水线

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `stalwart:jmap`, `stalwart:telegram` |流式传输，无 EX | `State::enqueue` （一个 `XADD`），从 `telegram_webhook` 和 `jmap_push` 调用；通过 `claim_dedup_and_enqueue` (`worker.rs`) | 协调路径消费者组“stalwart-workers”、消费者“http-worker”上的“read_batch” |
| `delivery:inflight:{stream}:{id}` | EX 60 | 交付路径中由 `claim_dedup` 构建并声明 | 完成后 `release_dedup`
| `delivery:committed:{stream}:{id}` | EX 604_800 | 交付路径中由 `claim_dedup` 构建并声明 | 发送前 `dedup_exists`
| `retry:{stream}:{id}` | EX 86_400 | `retry_or_dlq` 的 Lua 脚本（同一键上的 `INCR` 然后 `EXPIRE`）|回收路径|
| `stalwart:jmap:dlq`, `stalwart:telegram:dlq` |流式传输，无 EX |相同 Lua 脚本的“XADD”；名称从工作线程中的 `retry_or_dlq` 调用站点传入（使用 `max_attempts` 3） | **代码中没有任何内容读取它** |
| `state:jmap:since` | 无（持久）| `set_reconcile_state` |每次传递之前的`get_reconcile_state`；根据 §6.5 进行编码 |

没有 `delivery:pending:{stream}` 键 — “pending”指的是 Redis Streams
待处理条目列表 (PEL)，Redis 内部维护。
上面的流名称很具体：“stalwart:jmap”和“stalwart:telegram”；
其余键中的“{stream}”是这两个之一。
该错误记录在“docs/retired.md”中。

每个事件至多交付一次：“delivery:inflight”密钥在 60 秒的窗口内领取
出站发送前，`delivery:committed`将保证延长至发送后 7 天
确认。

DLQ 是 **append-and-ack**：附加到 `stalwart:jmap:dlq` 的相同 Lua 脚本 /
`stalwart:telegram:dlq` 也是 `XACK` 是源流中的消息，因此递增，DLQ
追加和源确认是原子的，全部都在一个 Lua 脚本内。没有代码路径读取 DLQ
返回 — 重播是操作员操作，而不是服务功能。

### 1.4 幂等与速率限制

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `dedup:tg:{update_id}` |前 86_400 |使用 `telegram_webhook` 中的 `claim_dedup` 构建并声明，在失败的入队时使用 `release_dedup` 释放 | — |
| `dedup:jmap:{account}:{email}` | EX 86_400 |使用“jmap_push”中的“claim_dedup”构建并声明；在协调路径上重建相同的密钥并使用“claim_dedup_and_enqueue”（“worker.rs”）|声明— |
| `ratelimit:push-verify:{subscription}` | EX 30 | 由 `register_push` 中的 `claim_dedup` 构建并声明；`false` 的声明结果就是 `429 push_verify_rate_limited` 分支 | — |
| `state:jmap:since` | **无 EX** | `set_reconcile_state`（没有 `EX` 的 `SET`）| `get_reconcile_state` |

`state:jmap:since` 是唯一一个在没有显式声明的情况下无限期存在的状态键
删除，按设计：它是协调游标，过期将强制完全删除
下次重新启动时重新设定基线。

### 1.5 锁

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `lock:reconcile` | 300 | `reconcile` 中的 `acquire_lock` |心跳任务中的 `renew_lock`（90 秒） |
| `lock:push-register:{sha256(callback_url)}` | 360 | `register_push` 中的 `acquire_lock` | `register_push` 每个退出路径上的 `release_lock`

**为什么注册锁是 360 秒而不是 300 秒。** 锁的寿命必须是最长的
配置 JMAP 请求超时（300 秒），否则缓慢的注册可能会导致
重复。来源评论：

> 锁的寿命必须超过配置的 300 秒最大 JMAP 请求超时时间；这可以防止
> 由于接受重复项而创建缓慢。

### 1.6 推送订阅状态

| 键 | TTL | 写入方 | 读取方 |
|---|---|---|---|
| `push:subscription:{id}` | EX 300（调用方提供，最小 1）| `remember_push_subscription`，从 `register_push` 调用 | `push_subscription_verified` |
| `push:subscription:id` | 无 EX | `remember_push_subscription_id` | 当前订阅查询 |
| `push:subscription:{id}:status` | 900 / 300 / 86_400（强制最小 1）| `set_push_subscription_status`；验证请求时为 `pending`，成功时为 `verified`，禁用时为 `disabled` | **无** |
| `push:registration:{sha256(callback_url)}` | EX 604_800 | `remember_push_subscription_for_callback`，从 `register_push` 调用 | `get_push_subscription_for_callback`;通过 `remove_push_subscription_for_callback` 删除 |
| `push:registration:current` |前 604_800 | `set_push_registration_current`，从 `register_push` 调用 | `get_push_registration_current`;通过 `clear_push_registration_current` 清除，`disable_push` 经 `clear_push_registration_current_if` 清除 |
| `push:orphan:{subscription_id}` |前 604_800 | `record_push_orphan` |孤儿扫荡|

**三个不同的键经常被混淆。在编辑之前请阅读本文。**

- `push:registration:{sha256(callback_url)}` 是从回调 URL 到
  订阅 ID 的**映射**，TTL 7 天。它不是一把锁。
- `push:registration:current` 存的是**当前存活订阅的回调 URL 原文**，同样 7 天 TTL。它在新订阅
  创建之后最后写入。用一个不同的回调 URL 注册时，后端会在注册锁内先销毁上一个订阅、再创建
  新订阅，因此旧 origin 不再收到推送；`disable_push` 只在被注销的正是当前地址时才清它。
  其后缀是字面量 `current`，不会与 per-URL key 的 `session_digest` 十六进制后缀相撞。
  它既不是映射也不是锁。
- `lock:push-register:{sha256(callback_url)}` 是**单飞注册锁**，
  TTL 360 秒。它不是映射。

前两者 TTL 相同但是用途无关的独立键。锁与 per-URL 映射使用相同的摘要，但完全是不同的键。

**注释：`push:subscription:{id}:status` 设计为只写。**
来源对“state.rs”有明确的注释：

> `push:subscription:{id}:status` 目前是只写的

它是一个用于“redis-cli”检查的操作/可观察性跟踪；它**不是**门
也没有任何代码为鉴权目的读取它。历史上写入过的三个取值是 `pending`（900 s）、
`verified`（300 s）和 `disabled`（86400 s）——各自的调用点见上表。
不要根据它的存在来推断其正确性。

---

## 2. 错误响应封装

每个错误响应都使用一种包络形状：

```json
{ "error": "<machine_code>", "request_id": "<id>" }
```

`error` 是一个**普通的机器可读字符串**，而不是嵌套对象 - 没有 `message`
场。 `request_id` 始终存在。

|状态 |代码|重试后 |
|---|---|---|
| 400 | `invalid_request`、`missing` | — |
| 401 | 401 `未经授权` | — |
| 403 | 403 `forbidden`、`chat_not_allowed`（仅限 `/debug/*`）| — |
| 404 | 404 `push_subscription_not_found` | — |
| 409 | 409 `冲突` | — |
| 422 | 422 `无效配置` | — |
| 429 | 429 `push_verify_rate_limited` | `30` |
| 500 | 500 `内部错误` | — |
| 502 | 502 `telegram_send_failed`（仅限`/debug/*`）| — |
| 503 | 503 `service_unavailable`、`disabled`、`reconcile_retry`、`push_verify_failed`、`push_state_unavailable`、`push_destroy_failed` |可重试时为“30”，或始终用于推送注册/禁用失败 |

`Retry-After` 有两个发射点，都在 `notify.rs` 中，并且值始终是文字
字符串“30”：

- `error_response` 仅当其 `retry` 标志为 true 时才设置它**。准备失败
  路径传递 `retry = true`，因此 `/ready` 被该规则覆盖，而不是被
  特殊情况。
- `error_response_with_id` 设置它**无条件**。仅用于推送
  注册/禁用失败“push_state_unavailable”和“push_destroy_failed”，它们是
  总是“503”。

`GET /api/status` 是使用非信封正文报告设置状态的例外情况。

---

## 3. 后端路由

所有 HTTP API 路由均在“router_with_worker_state_runtime_bootstrap”中注册。
最后一个表行中的三个静态 SPA 路由是例外：它们位于
`web::router()` (`src/web.rs`) 并通过 `.merge(web::router())` 合并到路由器中 —
在设置模式路由器中，然后在生产路由器中再次。设置模式路由器安装
这个表只有三个——“/healthz”、“/ready”和“/api/status”——没有业务或
管理路线； §5.1 解释了为什么这是故意的。

|方法|路径|处理器|
|---|---|---|
|POST|`/webhook/tg`|`telegram_webhook`|
|POST|`/push/jmap`|`jmap_push`|
|POST|`/api/push/register`|`register_push`|
|POST|`/api/telegram/register-webhook`|`register_telegram_webhook`|
|POST|`/api/push/disable`|`disable_push`|
|POST|`/reconcile`|`reconcile`|
|POST|`/worker`|`worker`|
|GET, PUT|`/api/config`|`get_config` / `put_config`|
|GET, PUT|`/api/enabled`|`get_enabled` / `put_enabled`|
|GET, PUT|`/api/business-config`|`get_business_config` / `put_business_config`|
|POST|`/api/business-config/preflight`|`preflight_business_config`|
|POST|`/api/bootstrap`|`bootstrap`|
|POST|`/api/admin/session/revoke`|`revoke_admin_session`|
|POST|`/api/admin/session`|`create_admin_session`|
|GET|`/healthz`|`healthz`|
|GET|`/ready`|`ready`|
|GET|`/api/status`|`setup_status`|
|GET|`/`、`/assets/config.js`、`/assets/styles.css`|`web::router()`|

远程调试表面（`debug_router`、`src/debug.rs`）仅在
请求调试界面 - 命令行上的“--debug”或真实的“DEBUG_ENABLED”
env var — 并且“DEBUG_TOKEN”非空**（“SAF-DEBUG-GATE”；门被读入
在构建任一路由器之前的`src/main.rs`）。否则这些路线都不存在并且
请求落入 axum 的通用“404”，而不是 401。所有七个请求都需要
`Authorization: Bearer <DEBUG_TOKEN>`，由 `debug_authorized` 检查，委托给
生产“worker_authorized”，因此比较是恒定时间。

|方法|路径|处理程序 |
|---|---|---|
|获取 | `/调试/ping` | `debug_ping` |
|获取 | `/调试/配置` | `调试配置` |
|获取 | `/调试/redis` | `debug_redis` |
|获取 | `/调试/jmap` | `debug_jmap` |
|获取 | `/调试/电报` | `调试电报` |
|获取 | `/调试/worker` | `debug_worker` |
|发布 | `/调试/通知` | `调试_通知` |

响应约定：

- `GET /healthz` 无条件返回 **200**。它是一个活性探针，不能是
  用于决定是否路由流量。
- 当配置、Redis 或上游探测器未准备好时，`GET /ready` 返回 **503**，
  并携带“Retry-After: 30”表示未准备好响应。它检查四件事：配置
  完整性（`setup_missing` 为空）、Redis 可访问性和两个上游探测器 —
  使用配置的基本凭据`GET {jmap_origin}/.well-known/jmap`，以及
  `获取 https://api.telegram.org/bot<token>/getMe`。两个探针共享“PROBE_TIMEOUT”= 3000ms
  并**并行**（`tokio::join!`），所以最坏的情况
  是单次超时，大约3s。成功时，正文是一份报告，其“jmap”/“telegram”字段
  是真实的探测结果。探针是简单的可重用函数（`probe_jmap_session`，
  `probe_telegram_get_me`) 也由远程调试路径使用，因此它们不能重复。
  JMAP 探针有意针对*规范化来源*进行身份验证：探测原始数据
  未经身份验证的会话 URL 将永远报告未就绪并使入口停止路由。
- `GET /api/status`（**始终 200**）返回
  `{“ready”：<bool>，“mode”：“configured”|“configuration-setup”，“missing”：[...]，“version”：“<build-fingerprint>”}`。
  “ready”为“false”，“missing”列出了缺少的必需密钥（“REDIS_URL”、
  `CONFIG_ENCRYPTION_KEY`）当变量不存在时 - 路由本身永远不会出错，所以它是
  可以安全进行民意调查。 `version` 是由 `build.rs` 烘焙的 `BUILD_VERSION` 字符串
  （`<git-sha-or-nogit>+<UTC 构建时间>`）。SPA 已不再渲染它，确认部署落地只能查这个接口。
- `/debug/*` 在一个地方返回 **503**：业务执行时`POST /debug/notify`
  配置未加载或没有出站客户端 — `service_unavailable`
`重试时间：30`。三个探测端点反而报告失败
  **在 200 体内** (`{"ok":false,"detail":...}`)，因此永远不会看到无法到达的上游
  就像停电一样。当聊天时，“POST /debug/notify”也会返回“403 chat_not_allowed”
  在*非空*允许列表之外，并且“502 telegram_send_failed”
  发送失败。
- 静态资产由严格的CSP提供服务；参见§7。

---

## 4. 网关与后端路由对照表

> **独立验证。** 来源：`cloudflare-worker/src/backends.js` `SAFE_ROUTES`，
> 共 19 条，并与 `ROUTE_METHODS`（`index.js`）一起定义每条路径允许的方法。Worker 入口文件是 `src/index.js`
>（`wrangler.toml`）；`src/lb.js` 实现请求转发和有界故障转移（`SAF-LB-PASSTHRU`，
> `C-NO-LONG-CONN`)。

该门是**无条件且失败时关闭的**。Worker 不读取任何配置开关：
`SAFE_ROUTES` 中缺少的路径返回 **404**，已注册路径上的错误方法返回 **405**，
缺失或无法解析的后端池返回 **503** 而不是转发请求。

**由 Worker 转发（19）：**

`/` · `/assets/config.js` · `/assets/styles.css` · `/api/status` · `/api/config` ·
`/api/business-config` · `/api/business-config/preflight` · `/api/admin/session` ·
`/api/admin/session/revoke` ·
`/api/enabled` ·
`/webhook/tg` · `/push/jmap` · `/api/push/register` · `/api/telegram/register-webhook` · `/api/push/disable` · `/reconcile` ·
`/worker` · `/ready` · `/healthz`

**已在后台注册但未转发（2）：**

|路径|为什么网关上没有它 |
|---|---|
| `POST /api/bootstrap` | 一次性信任引导；不在路由集内，只能在后端自己的地址上访问 |
| `/debug/*`（7 条路由）| 选择加入的远程调试界面（`SAF-DEBUG-GATE`）；不在 `SAFE_ROUTES` 中，因此**只能**在后端源自己的地址上访问 |

`GET, PUT /api/enabled`（`SAF-ENABLE-FLAG` 终止开关）**被转发**：
管理 SPA 从 Worker URL 提供，Service 卡片读取它（`loadEnabled`）、切换开关写入它。
两个端点本来就都要求凭据，但凭据并不相同：`/api/enabled` 由 `config_authorized` 校验，
它接受共享 worker token **或**有效的 admin session；而 `POST /api/admin/session` 由
`worker_authorized` 对 admin 凭据（`CONFIG_ENCRYPTION_KEY`）校验，它本身就是发放 session
的入口。因此新增 `/api/enabled` 并没有打开新的凭据路径——`/api/admin/session` 本来就在
转发列表里。

后果：没有任何剩余路由承载外部业务流量，所以后端实例前面不需要第二个入口。
SPA 的首次启动流程仍然无法通过 Worker 的 `/api/bootstrap` 完成——引导要么必须
直接针对后端 origin 执行，要么必须把 bootstrap 路径加入网关白名单。

`POST /api/push/register`、`POST /api/telegram/register-webhook` 和 `POST /api/push/disable` **被转发**。这些路由可以安全代理：
回调 URL 由客户端在请求正文中提供，在写入任何内容之前就被校验为 URL，并且每个推送订阅记录
都写入共享 Redis 也从共享 Redis 读取（`lock:push-register:{sha256(url)}`、`get_push_subscription_for_callback`），
因此请求由哪个后端服务实例处理并不重要——推送注册不再需要访问某个特定的后端地址。
操作员保存业务配置后，SPA 会通过受保护接口提交两个回调 URL。

---

## 5. 环境变量

三个不同的层。它们不可互换。

### 5.1 启动时配置（2 项必填，2 项可选且有默认值）

|变量|必填 |默认 |来源 |
|---|---|---|---|
| `REDIS_URL` |是的 | — |在启动时读取`src/main.rs` |
| `CONFIG_ENCRYPTION_KEY` |是的 | — | `encryption_key_from_env` |
| `端口` |没有| `8080` | `unwrap_or(8080_u16)`。监听者直接绑定它； `Config` 不携带端口字段 |
| `DEBUG_ENABLED` |没有| — |真值为 `1`/`true`/`TRUE`/`True`/`yes`/`YES` （完全匹配）；单独启用调试界面，与 `--debug` 配对作为替代方案 (`SAF-DEBUG-GATE`) |
| `DEBUG_TOKEN` |没有| — |仅在通过“DEBUG_ENABLED”或“--debug”（“SAF-DEBUG-GATE”/“SAF-DEBUG-ORIGIN-ONLY”）请求调试表面后才有效

`RUN_MODE` **不再存在**：标识符与旧版本一起被删除
`Config::from_env()` 环境解析器（在 `docs/retired.md` 中注册）。两个网络钩子
和协调流量共享一个路由器，并且协调是一个独立的“POST /reconcile”
端点，因此该变量从未改变任何运行时行为 - 现在无需设置任何内容。

**缺少必需的变量不会使进程崩溃。**它会记录警告并提供服务
`router_configuration_setup`，仅挂载`/api/status`，
静态 SPA 之上的“/ready”和“/healthz”。 §3 中的业务和管理路线不是
完全注册：“admin_token”为空，因此“constant_time_eq”会拒绝
每个候选人永远，并且“MemoryState”无法持久保存引导程序
无论如何都要写。在此模式下发布到“/api/bootstrap”会产生“404”（路由不存在），而不是“401”
其内容为“使用更好的凭据重试”。容器保持运行并回答状态
在不做任何业务工作的情况下浮出水面。这是一种故意的失败关闭设置姿势，并且它
是“容器正常但没有任何反应”的最可能的原因。

### 5.2 存储在 Redis 中的业务配置

SPA 通过 `GET /api/business-config` 读取配置，并通过 `PUT /api/business-config` 写入配置；
成功写入后，配置会在 1 秒内生效。首次写入会创建配置，后续写入会热加载配置。
`GET` 返回 `configured`、`revision`、`values` 和 `secrets_present` 四个字段：`values` 包含 10 个非敏感字段，
`secrets_present` 则分别标记 `bot_token`、`jmap_password`、`telegram_webhook_secret`、
`reconcile_token`、`worker_token` 和 `llm_api_key` 是否已配置；响应绝不包含这些密钥的值（`SAF-NO-SECRET-ECHO`）。
尚未保存配置时，接口返回 `200`、`configured: false`、`revision: 0`、空的 `values`，且所有存在标记均为 `false`，
因此 SPA 无需为首次配置另设代码路径。

`PUT` 请求体采用**部分更新**：只替换请求中列出的字段，其余字段沿用已存储值（见 `config.rs` 的 `apply`）。
未提交的密钥会保留原值，因此重新提交预填表单是安全的。显式提交空字符串会清空对应密钥。
“空白表示不变”是客户端约定；SPA 会在发送前移除空白密钥字段，服务端不会替客户端过滤。

增量语义仅在配置存在后才有效。没有任何存储，就没有
要回退到的值，因此不完整的补丁会被拒绝，并显示 **422 `invalid_configuration`**
— 第一次保存必须完成。针对后端原点的“POST /api/bootstrap”是
一次性写入路径，从头开始创建配置以实现自动化。过程从来没有
在正常操作中从环境中读取这些。

**坚持并报告，而不是坚持如果可连接。**验证是写入的唯一门，
在“PUT”上，它针对 **合并** 配置运行，而不是提交的正文：补丁
永远不会单独验证，并且完整存储的连线永远不会被丢弃（“apply”在
`config.rs`)。 “validate_business_wire”首先运行，拒绝是真正的“422”。曾经
合并的线路是有效的，它*始终*持续存在 - 合并的线路，而不是请求正文 - 并且
然后才构建客户端并运行
工人交换了。因此，失败的构建会返回 **200** 并显示“persisted: true”，
`runtime_applied: false` 和一个包含 `{component,step,detail}` 对象的 `warnings` 数组，其中
`component` 是 `jmap` 或 `llm`，`step` 是 `connect`、`account`、`config` 或 `build`，而不是
比 503 更安全。保存配置并报告中断而不是隐藏。
`runtime_applied` 是
仅当提交重新加载时才为“true”。这是故意的：无法访问的 JMAP 一定不能
将有效配置转变为静默数据丢失。 `PUT /api/business-config` 和
`POST /api/bootstrap` 都遵循这个契约；在这两种情况下都会返回修订版
`x-business-config-revision` 标头，只有 `PUT /api/business-config` 也会在
响应正文，作为“修订版”（“u64”）。 SPA 将其显示在其状态行中并发送
作为“revision”控制字段返回到请求正文中，后端将其与
存储修订版本并在不匹配时回答“409 冲突”。引导程序的
主体不携带“修订”字段，并且省略它的“PUT”主体保持最后写入获胜。

`POST /api/business-config/preflight` (`preflight_business_config`) 运行相同的
验证和客户端根据提交的线路构建并返回每个组件的判决
不写入任何数据，也不调用正在运行的服务处理流程：

```json
{
  "persisted": false,
  "validation": { "ok": true, "errors": [] },
  "components": {
    "jmap": { "ok": false, "errors": [{ "component": "jmap", "step": "connect", "detail": "..." }] },
    "llm": null
  }
}
```

当验证已经失败时，“components”为“null”（没有必要从
被拒绝的线），当“llm_enabled”/“llm_allow_net”为 false 时，“llm”为“null”
而不是空洞地“好吧”。答案是“配置不好”的预检是
**200**，因为这是呼叫者要求的答案。认证是一样的
`config_authorized` 门作为写入路径；它故意**不**位于`DEBUG_TOKEN`后面。

### 5.3 不保留旧版环境变量配置路径

`Config::from_env()` 及其助手（`required_secret`、`required_nonblank`、`env_bool`）是
**已删除** — `config.rs` 根本不执行 `std::env` 读取。下面的名字是遗产
env 表面并仅列出，以便过时的部署脚本可以被识别为过时的
（在`docs/retired.md`中注册）：

`RUN_MODE` · `CHAT_ALLOWLIST` · `TELEGRAM_CHAT_ID` · `LLM_ENABLED` · `LLM_ALLOW_NET` ·
`LLM_API_KEY` · `LLM_BASE_URL` · `LLM_MODEL` · `LLM_SUMMARY_TARGET_CHARS` · `BOT_TOKEN` ·
`JMAP_SESSION_URL` · `JMAP_USERNAME` · `JMAP_PASSWORD` · `ACCOUNT_ID` · `RECONCILE_TOKEN` ·
`TG_WEBHOOK_SECRET` · `WORKER_TOKEN`

`JMAP_*` 系列恰好是三个名称。在生产中，帐户 ID 作为
`jmap_push` 上的 `accountId` 请求正文字段，并在客户端上保存为
`account_id`;唯一剩下的环境读取这些
四个名字在`#[ignore]`真实服务器冒烟测试中
`src/domain/jmap/client.rs`，它不是生产配置界面的一部分。

单个前缀名称是“TELEGRAM_CHAT_ID”。任何其他记录为
`TELEGRAM_BOT_TOKEN` 或类似的内容是文档错误，不是受支持的变量。

**信任根。** SPA 管理员凭据本身是“CONFIG_ENCRYPTION_KEY”：在启动时读取
并保存为“admin_token”，通过以下方式在恒定时间内进行检查
两者顶部的“worker_authorized”
`POST /api/bootstrap` 和 `POST /api/admin/session`。它是
仅与请求承载进行比较——从不回显、记录或存储。会议
由 `/api/admin/session` 发出是一个新生成的随机 32 字节十六进制令牌
（每次调用生成），绝不是凭证；仅其摘要保留在 Redis 中
`config:admin_session`。 `REDIS_URL` 是单独的 Redis 连接字符串：ACL 密码
它（如果有）对 Redis 连接进行身份验证，并且不是任何 HTTP 的凭据
端点。

### 5.4 通知内容渲染

`send_notification` (`src/channel.rs`) 是唯一将通知渲染到
Telegram 文本，它只渲染三个元数据行——不是正文，也不是
大语言模型摘要：

```text
From: Zhang San <zhang@example.com>
Subject: Q4 budget
Received: 2026-09-28 09:15
```

`From:` 是标准显示形式的 JMAP `EmailAddress`：`Name <address>`
发件人带有显示名称，否则为裸地址。 “发件人：”和“主题：”
仅当 JMAP 未返回时才回退到文字（“未知”/“（无主题）”）
财产。

“已收到：”在通知时呈现，而不是存储为文本。服务器保留
JMAP `receivedAt` 作为 Unix 秒并将其转换为配置中的挂钟时间
使用 `%Y-%m-%d %H:%M` 的时区 — 24 小时制，分钟精度，无 AM/PM，无秒。
当 `receivedAt` 不存在，或超出区域的可表示即时范围时，
显示文字“未知”而不是剪辑的时间戳。

时区是一个业务配置字段，`timezone` (`REQ-TIMEZONE-DISPLAY`)：
IANA 标识符，默认“亚洲/上海”。只接受 16 个区域，每一个区域
没有夏令时转换 - `Etc/UTC`、`非洲/开罗`、`欧洲/伊斯坦布尔`、
`非洲/内罗毕`、`亚洲/迪拜`、`亚洲/卡拉奇`、`亚洲/加尔各答`、`亚洲/曼谷`、
`亚洲/胡志明市`、`亚洲/上海`、`亚洲/香港`、`亚洲/台北`、`亚洲/新加坡`、
“亚洲/马尼拉”、“亚洲/东京”、“亚洲/首尔”。任何其他标识符都会被拒绝，并显示 **422**
而不是默默地回到猜测。这是一个故意的限制：离线
build 没有 IANA tz 数据库（“chrono-tz”不可用），因此每个区域解析为
固定偏移并且从不跟踪转换。十六个人中没有一个需要一个，这就是为什么
‘亚洲/上海’可以是默认值。与其他业务配置一样，它是经过编辑的
在 SPA 中并热应用，无需重新启动。

---

## 6. 出站与运行时参数

### 6.1 出站请求

|参数|默认 |范围/上限|来源 |
|---|---|---|---|
| `jmap_timeout_ms` | 15_000 | 15_000 100..=300_000 | 100..=300_000默认在`state.rs`中；在写入路径中验证|
| `telegram_timeout_ms` | 10_000 | 10_000 100..=300_000 | 100..=300_000默认在`state.rs`中；在写入路径中验证|
| `llm_timeout_ms` | 30_000 | 100..=300_000 | 100..=300_000默认在`state.rs`中；在写入路径中验证|
| `最大重试次数` | 3 |硬顶 5 |默认在`state.rs`中；在写入路径中被拒绝；重新固定在`channel.rs` |

**没有** `LLM_MAX_RETRIES` 环境变量；重试计数位于
`config:outbound` 并且无论写入什么内容都以 5 为界。

### 6.2 状态协调预算

|恒定|价值|来源 |
|---|---|---|
| `BASELINE_PAGE_SIZE` | 100 | 100 `worker.rs` |
| `BASELINE_MAX_PAGES` | 100 | 100 `worker.rs` |
| `BASELINE_MAX_EMAILS` | 10_000 | 10_000 `worker.rs` |
| `RECONCILE_MAX_PAGES` | 100 | 100 `worker.rs` |
| `协调预算` | 20 秒 | `worker.rs` |
| `CHANGE_WINDOW_CAP` | 4_096 | 4_096 `worker.rs` |
| `协调初始更改` | 100 | 100 `notify.rs` |
|锁定TTL/心跳| 300 秒 / 90 秒 | `reconcile` 及其心跳任务 |

`initial_changes` 是一个函数参数，而不是一个常量：它只调整
第一个“/changes”调用。重播阶段之后可能会扩大窗口，最多
`CHANGE_WINDOW_CAP`，在光标前进之前。

### 6.3 空闲阈值

XAUTOCLAIM 的空闲阈值源自实时出站配置：

```
count × (max_retries + 1) × (jmap_timeout_ms + telegram_timeout_ms + llm_timeout_ms) × 2
```

低于传统 300 秒单事件上限，上限为 6 小时。地板也是
当无法读取配置时回退，因此
Redis 打嗝永远不会折叠窗口。仅在多实例上重复
赌注，永不损失。

### 6.4 搜索限制

|恒定|价值|来源 |
|---|---|---|
| `SEARCH_LIMIT` | 10 | 10 `worker.rs` |
| `SEARCH_SUBJECT_MAX` | 120 个字符 | `worker.rs` |
| `SEARCH_PREVIEW_MAX` | 160 个字符 | `worker.rs` |

### 6.5 状态协调游标

`/reconcile` 保留一个游标，`state:jmap:since`，并且它是双峰的：

- `baseline:{hex-encoded-state}:{position}` — 位置行走正在飞行中。的
  walk 按位置 (`list_emails_page`) 和 `/changes` 枚举集合
  尚未重播。
- 裸状态字符串 — 步行已完成；这是恢复的令牌
  “电子邮件/更改”来自。

这两种模式是“worker.rs”中显式的“ReconcileCursor”变体，而不是
保留位置值。页上限（`BASELINE_MAX_PAGES`）是一个硬停止
步行：如果在那里剪切步行，则光标将保持**作为步行**，从不
作为重播，因为重播模式断言列表已用完并正在重播
来自半列出状态的“/changes”将跳过仍然存在的内容。

有两个令牌类别正在发挥作用，并且它们不可互换：

- `Session.state`（RFC 8620 §2.1）是每个会话创建的，并且**不是**
  “电子邮件”-收集令牌。新的或重新设定基线的游标带有它，因为
  jmap-client `Session` 类型不公开每个集合的状态。
- “Changes”响应中的“newState”**是**一个集合令牌。

因此，严格遵守的服务器可能会拒绝第一个“/changes”调用
重新设定基线后出现 `sinceState` 错误。 `replay_changes` 通过以下方式吸收它
重新基线到一个新的令牌并返回，因此不匹配只能咬住
一次调用和下一次传递重新开始步行； 24小时重复数据删除密钥
(`ttl::DEDUP_JMAP_SECONDS`) 将生成的重播限制为最多一个重复项。

`Email/changes` 还返回 `destroyed` 和 `oldState`。两者均被丢弃
`EmailChanges`：已删除的电子邮件没有任何内容可供查找，因此它不会驱动
通知并且不移动光标，“oldState”回显
服务器接受的“sinceState”，调用者已经持有该状态。保留任一
意味着存储一个无人读取的值。

---

## 7. 静态资源与响应头

从“src/web.rs”提供服务。

- 仅三个路由：`GET /`、`GET /assets/config.js`、`GET /assets/styles.css`。
  其他一切都返回 404。
- 资产在编译时通过“include_str!”嵌入。没有
  运行时的文件系统访问。
- 每个响应都带有 `Content-Type`、`Cache-Control: no-store`、
  `X-Content-Type-Options: nosniff` 和 `Referrer-Policy: no-referrer`。
- CSP **仅应用于 HTML 文档**：插入到内部
  一个 `if html` 块：

  ```
  default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self';
  form-action 'self'; base-uri 'none'; frame-ancestors 'none'
  ```

- `X-Frame-Options: DENY` 与 CSP 一起设置，仅在 HTML 响应上。

`default-src 'none'` 故意比 `'self'` 更严格：页面默认关闭
并且只有`script-src`、`style-src`和`connect-src`重新打开同源通道。

---

## 8. 尚未实现的功能

在此记录，以便其他地方的引用不会被误认为是已发布的功能。

-“/search”**已**实现（“bfe0fd8”）。适配器：“worker.rs”中的“Intent::Search”和
  在`src/domain/jmap.rs`中，由`src/domain/jmap/client.rs`中的`search_emails`支持。
  当前无法实现：**邮件正文片段**。jmap-client `0.4.2` 仅公开
  来自“SearchSnippet/get”的“emailId”/“subject”/“preview”，其“Filter”类型没有比较器语法，因此
  无法通过锁定版本的 crate 实现对邮件各正文部分的高亮。搜索会降级为
  当不支持片段时，“主题”/“预览”突出显示以及纯 ID 列表。设计
  请注意“docs/design.md”第 5.7 节。
- Cloudflare Worker 不包含 Rust：“cloudflare-worker/src/”仅包含“index.js”，
  `lb.js`、`backends.js` 和 `health.js`。在“cloudflare-worker/”下写入的任何“*.rs”路径
  是文档错误，而不是源文件。

---

## 参考资料

外部依赖项，全部可达并在验证时返回 HTTP 200：

- axum — https://docs.rs/axum
- redis（Rust 箱，0.27）- https://docs.rs/redis/0.27/redis/
- reqwest（Rust 板条箱，0.13）- https://docs.rs/reqwest/0.13/reqwest/
- jmap-client (0.4.2) — https://docs.rs/jmap-client/0.4.2
- JMAP 规范 — https://jmap.io/spec/
- JMAP RFC 8620 — https://datatracker.ietf.org/doc/rfc8620/
- Redis 流 — https://redis.io/docs/latest/develop/data-types/streams/
- Redis 持久化 — https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/
- Cloudflare Workers — https://developers.cloudflare.com/workers/
- Cloudflare Workers 运行时 API — https://developers.cloudflare.com/workers/runtime-apis/
- RFC 8030（网络推送）— https://www.rfc-editor.org/rfc/rfc8030
- RFC 7231 §7.2.3 — https://www.rfc-editor.org/rfc/rfc7231#section-7.2.3
- Dockerfile 参考 — https://docs.docker.com/reference/dockerfile/
- rust Docker 镜像 — https://hub.docker.com/_/rust
- Uptime Kuma — https://uptime.kuma.pet/
- OWASP 日志记录备忘单 — https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html

未链接，因为它们未解析为文档：`crates.io/crates/jmap-client`
（404）和 `platform.openai.com` 文档路径（403）。项目使用的大语言模型依赖与 OpenAI API 兼容，
并通过 `reqwest` 调用。

## 9. 部署平台细节

面向操作员的序列位于 [`deployment.md`](deployment.zh-CN.md) 中。本节保留
配置或诊断时有用的特定于平台的行为和设置
部署。

### 9.1 后端部署方式

**HostStack 生产部署。** 仓库中的 `hoststack.yaml` 配置 HostStack 原生 Rust 运行时。
它运行 `cargo fetch --locked` 和 `cargo build --release --locked`，启动
`./target/release/message-weave`，并以 `/healthz` 检查存活状态。通过平台加密配置
`REDIS_URL` 和 `CONFIG_ENCRYPTION_KEY`。YAML 中的服务命令会覆盖控制面板中保存的命令；
移除该文件会恢复 HostStack 默认的 `./target/release/app`，而这个仓库并不生成该二进制文件。
在控制面板命令全部修正且替代部署通过验证前，应继续将此 YAML 文件纳入版本控制。

HostStack 部署不使用仓库的 Dockerfile；镜像构建和运行时由平台管理。Dockerfile 适用于本地容器运行和其他基于 Docker 的主机。

**Docker 镜像路径。** 根 Dockerfile 有两个阶段：

1. `rust:1-slim-bookworm` 使用锁定的 Cargo 依赖项构建发布二进制文件。
2. `debian:bookworm-slim` 接收二进制文件、CA 证书、`tini` 和只读
   诊断工具（“curl”、“procps”、“iproute2”、“jq”和“netcat-openbsd”）。它创造了
   并以“messageweave”系统用户身份运行，设置“PORT=8080”，仅公开该端口，并且
   通过`tini`启动服务。

该映像没有 Redis 进程、数据库或持久数据卷。运行时配置是
由主机注入；敏感凭据不会进入构建参数或镜像层。 Docker 平台
运行状况检查应调用 [`deployment.md`](deployment.zh-CN.md) 中描述的 HTTP 端点。

### 9.2 后端配置与密钥

该进程在启动时仅读取“REDIS_URL”和“CONFIG_ENCRYPTION_KEY”。可选的
远程调试界面使用独立的“DEBUG_ENABLED”开关和“DEBUG_TOKEN”；离开
除非有意启用远程诊断，否则两者在生产中均未设置。完全启动
变量语义和 Redis 业务配置字段位于 [§5](#5-环境变量) 中。

Telegram、JMAP、allowlist、worker、reconcile 和 LLM 业务设置驻留在 Redis 中，
不是进程环境变量。 [§5.2](#52-存储在-redis-中的业务配置)中的字段列表和验证规则
是权威的。成功的初始保存可以使用一次性 `/api/bootstrap` 端点，在后端
自己的地址上完成；该路由不在网关路由集内，所以发给 Worker 的请求到不了它。
后续编辑使用受保护的配置 API。

### 9.3 回调注册与定时任务

调用 Telegram 的 `setWebhook` 时，使用已配置的 Webhook 密钥，并在 `allowed_updates` 中包含 `message`。
使用 `getWebhookInfo` 检查已注册的 URL；Telegram 不会在响应中回显 Webhook 密钥。更换密钥后，需重新调用 `setWebhook`。

通过 `POST /api/push/register` 和 HTTPS `callback_url` 注册 Stalwart Push。后端会创建订阅并完成 Stalwart 的验证回调；
重复注册相同 URL 是幂等的。用一个不同的回调 URL 注册时，后端会先销毁上一个订阅，再创建新订阅并重指向当前指针映射，
因此旧 origin 不再收到推送。调用 `POST /api/push/disable` 并传入该 URL 可删除订阅。

后端没有内置调度器。外部调度器运行 [`scripts/cron-drain.sh`](../scripts/cron-drain.sh)：设置了 `MW_RECONCILE_TOKEN` 时，
脚本先调用 `/reconcile`，再调用 `/worker`。在调度器的密钥管理器中配置 `MW_APP_URL`、`MW_WORKER_TOKEN`，
以及可选的 `MW_RECONCILE_TOKEN`。需要绕过 Worker、直连后端源站时，可用 `MW_WORKER_URL` 覆盖默认地址。
`--once` 用于 cron 单次运行，`--loop` 用于常驻进程；脚本还提供 `--diagnose` 和 `--test-notify` 排查问题。
两个端点成功时均返回 `204` 和空响应正文。若另一轮对账仍持有锁，`/reconcile` 会返回 `409`。

### 9.4 健康检查与发布核验

- `GET /healthz` 是后端活跃度。它并不能证明Redis或上游服务已经准备好。
- `GET /ready` 检查配置、Redis、JMAP 会话和 Telegram 的 `getMe` 端点。
  当依赖项不可用时，它会返回“503”和“Retry-After”。上游检查
  需要从后端进行出站 HTTPS 访问。
- `GET /api/status` 报告启动准备情况和缺少所需的启动变量名称。
- `GET /healthz-worker` 由 Worker 生成。检查“available >= 1”和“version”；
  当原始配置丢失时，HTTP 200 本身就意味着“无后端”。

对于后端代码门，使用中锁定的命令和容器环境
[`charter.md`](charter.zh-CN.md)。文档门必须在主机上运行，因此它
路径审计可以读取Git历史记录。

### 9.5 Cloudflare Worker 与控制面板

Worker 是可选的 HTTPS 网关。后端来源必须是 HTTPS 字符串，不带任何内容
路径、查询、片段或嵌入凭据。目前公开的来源清单和
“LB_VERSION”在“[vars]”下的“cloudflare-worker/wrangler.toml”中声明。移动
只有来源地址需要保密时，才将 `BACKEND_ORIGINS_JSON` 作为加密变量存储；从不存储
该列表中的凭据。 Worker本身不持有Telegram、JMAP、Redis或后端
商业凭证。

|设置|仪表板价值 |笔记|
|---|---|---|
|根目录 | `cloudflare-worker` |存储库根包含 Rust 服务，而不是 Worker 配置。 |
|应用名称 | `messageweave-lb` |匹配“wrangler.toml”中的“name”。 |
|构建命令 |留空 | Wrangler 在部署期间捆绑 Worker；该字段不是测试命令。 |
|部署命令| `npx wrangler deploy` |将此命令填入必需的部署字段。 |
|预览命令 | `npx wrangler dev --ip 0.0.0.0 --port 8787` |启用预览构建时使用；`wrangler preview` 不是有效命令。 |

在控制面板的 **Settings → Variables & Secrets → Add variable** 中添加 Worker 调整变量。
默认值为 `LB_REQUEST_TIMEOUT_MS=10000`、`LB_MAX_ATTEMPTS=2`、
`LB_RECONCILE_TIMEOUT_MS=320000`、`LB_WORKER_TIMEOUT_MS=300000` 和
`LB_HEALTH_TTL_MS=30000`。这些是可调整的变量，不是敏感凭据。 `LB_VERSION` 属于
在“[vars]”中，以便部署的版本在源代码管理中保持可见。

如果来源列表包含不应公开的地址，请在 **Settings → Variables & Secrets** 下将
`BACKEND_ORIGINS_JSON` 添加为加密变量，并删除其 `[vars]` 条目。
对加密变量的更改无需重建代码即可生效。仅预览环境
只接收为该环境配置的密钥。预览环境可以在没有来源列表时启动，
但无法验证后端转发。

对于 Git 集成，将选定的提交推送到连接到的 GitHub 存储库
设置 `cloudflare-worker` 根目录后的 Cloudflare。构建命令可以是
空；单独运行“npm test”作为 Worker 代码检查。部署后，请求
`/healthz-worker`，确认 `available` 至少为 1，并检查 `version` 是否匹配
`LB_VERSION`。 HTTP 200 的“status: no-backends”表示 Worker 没有可用的源列表；
HTTP 503 的“status: down”意味着所有配置的后端都未通过健康检查。
