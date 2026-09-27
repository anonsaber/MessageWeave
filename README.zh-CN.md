# MessageWeave（中文）

English documentation: [README.md](README.md) · 架构与设计见 [docs/design.md](docs/design.md)，部署与运维见 [docs/deployment.md](docs/deployment.md)

一个把 **Stalwart 邮箱**接入 **Telegram** 的个人邮件助手：新邮件来了在 Telegram 提醒你，还能按需查看/总结邮件内容。

> ⚠️ **当前仍属早期阶段，请先按本文完成小范围联调。**
> 真实邮箱和 Telegram 仍需要由部署者自行验证；JMAP Push 注册、验证回写已接入，但不会自动猜测公网 callback URL，必须由管理员显式调用注册接口。

---

## 这个项目是什么

- 你在自己的服务器上运行 [Stalwart](https://stalwart.dev) 邮箱。
- 用本工具把邮箱接到 Telegram 机器人：收到邮件时推送通知（只含发件人/主题/时间，不含正文）。
- 你可以在 Telegram 里要求查看某封邮件的原文、或让 AI 帮忙总结/翻译。

### 隐私原则

- **查看原文不会经过任何 AI**，直接读邮箱。
- **只有在你明确要求并确认后**，才把正文发给 AI 分析。
- 分析结果不会被保存。
- AI 可以完全不配置、完全关闭。

---

## 部署前，你需要准备这几样东西

1. **一个 Stalwart 邮箱账户**，并为其生成一个 **应用专用密码（App Password）**，而不是账户主密码。
2. **一个 Telegram 机器人**（通过 @BotFather 创建），拿到它的 **Bot Token**。
3. **一个 Redis 数据库**（你托管，建议开启 AOF 持久化）。机器人所有状态都放在这里。TLS Redis 使用 `rediss://`，普通 Redis 使用 `redis://`；Upstash 等服务的密码如果包含 `@`、`:`、`/` 或 `#`，必须先做 URL 编码。
4. **一个公网 HTTPS 地址**。平台会把这个地址指向机器人容器（HTTPS 由平台自动提供）。
5. **一个定时任务（外部 Cron / 调度器）**：每隔 5–10 分钟访问一次 `/reconcile` 地址，让机器人定期和邮箱对账。机器人自己不会设置定时器。

### 需要登记的地址

- 告诉 Telegram：把 webhook 指向 `你的公网地址/webhook/tg`

> 如果你使用「多实例 + Cloudflare Worker 负载均衡」（可选，见 `docs/deployment.md` §10），则把上面的 Telegram Webhook 和 Reconcile 地址指向 **Worker 的稳定域名**（`https://<your-worker>.workers.dev/webhook/tg` 等），不再指向各后端的平台地址。

---

## 在 Docker 管理器中创建容器

创建一个新容器，填好镜像、端口、内存等常规项后，只需注入 Redis 连接凭据：

| 环境变量 | 必须？ | 填什么 |
|---|---|---|
| `REDIS_URL` | ✔ | Redis 连接串（含密码），例如 `redis://:密码@redis.example.com:6379/0` |
| `CONFIG_ENCRYPTION_KEY` | ✔ | 32 字节十六进制密钥；可用 `openssl rand -hex 32` 生成 |

首次启动后，通过 SPA 的受保护配置页面提交 Telegram/JMAP/LLM 和业务鉴权配置。配置密钥加密写入 Redis，API 永不回显；管理员 PUT 成功后会校验并原子热重建客户端，后续请求即时生效，失败保留旧实例。

AI 摘要默认关闭。Telegram 用户需先发送受支持的授权语，再发送 `/summary <email_id>`：`/ai on`（或 `/ai yes`，1 小时）；含 `临时` 或 `一次` 的任意消息，如 `临时一次`（一次性，1 小时）；`今天`（24 小时）；`7天`（7 天）；`直到我撤销`（或 `长期`，最长 365 天）。这些触发词按原文字匹配，需按上文的中文原文输入，不支持英文别名。撤销：`/ai off`（或 `撤销授权`）。授权到期不会自动续期。未授权时仅返回邮件元数据；未配置 AI 或调用失败则回退为本地截断摘要。

> 缺少 `REDIS_URL` 或 `CONFIG_ENCRYPTION_KEY` 时服务进入 configuration-setup 模式，仍提供 SPA、`/api/status` 和探针；SPA 隐藏管理会话授权区，只显示缺少的环境变量与密钥不保存、不回显说明。配置恢复后 `/api/status` ready=true 才显示授权入口。

### HostStack 原生部署

HostStack 使用根目录 `hoststack.yaml` 构建并运行 Rust 服务：执行 `cargo build --release --locked`，运行 `./target/release/message-weave`，并以 `/healthz` 进行 30 秒间隔、5 秒超时的健康检查。请在 HostStack Secret 中注入 `REDIS_URL` 与 `CONFIG_ENCRYPTION_KEY`；密钥不写入 YAML、镜像或仓库文件。

管理会话内的“启用业务处理”开关持久化在 Redis `config:enabled`；缺失或读取失败均按关闭处理。关闭时 Webhook、Reconcile 和 Worker 返回 HTTP 503，不确认或丢弃上游事件；SPA、状态和管理配置 API 仍可用。

---

## 启动后怎么看它是否正常

- 浏览器或命令行访问 **`你的公网地址/healthz`**：能返回正常状态，说明机器人进程活着。
- `/ready` 返回 JSON 就绪状态并检查配置完整性与 Redis 可访问性；200 表示基础依赖就绪，503 表示不应接收业务流量。它不会调用 JMAP、Telegram 或触发任何业务副作用。
- 运行监控建议使用 Uptime Kuma：为 `/healthz` 建立存活 HTTP 检查，为 `/ready` 建立就绪 HTTP 检查（期望 200，间隔 30 秒、超时 5 秒）。项目不引入 Prometheus 或 exporter。
- `/reconcile` 执行鉴权、Redis 单飞锁与有界 JMAP `Email/changes` 对账；事件先幂等入 Redis Streams，全部成功后才推进 Redis `sinceState`，依赖暂时失败返回 `503` 以便 Cron 重试。Push 事件仍按鉴权、去重、入队路径处理，不能将其宣传为完整实时同步。
- 冷启动基线也有页数、邮件数和时间预算；超出预算时保存断点，下一次 `/reconcile` 从断点继续，避免大邮箱反复从头扫描。

## 配置管理

打开服务根地址 **`你的公网地址/`**，输入 `REDIS_URL` 中 Redis ACL 用户的密码，创建有效期 15 分钟的管理会话。会话与表单中的密钥只保存在当前页面内存；刷新或关闭页面后需要重新授权。页面不使用浏览器存储或 Cookie 保存会话、密钥和配置。

页面分开管理两类配置：运行参数（JMAP、Telegram、LLM 请求超时与最大重试次数）可通过 API 读取和保存；业务配置（JMAP、Telegram、LLM、chat allowlist 与各鉴权 token）由 `PUT /api/business-config` 完整替换。为避免密钥回显，服务端没有业务配置读取接口，因此每次完整替换都必须重新输入必填项。保存收到 HTTP 204 后页面会提示新配置已热加载，并清空密钥输入框。

Redis 尚未连接或初始化时，管理会话或配置 API 会返回 `503`；页面会说明不可用状态，并在运行参数读取成功前禁用其保存。经 Cloudflare Worker 访问时使用 Worker 根地址；`/api/config`、`/api/business-config`、`/api/admin/session`、`/api/admin/session/revoke` 等管理 API 路径已纳入受限路由白名单（`POST /api/push/register` 不在其中，需按 `docs/deployment.md` §10.5 直连某个后端实例）。

## 首次联调完整流程

下面是一条从空部署到 Telegram 收到第一条消息的可重复流程。所有示例中的 `<...>` 都是占位符，不要把尖括号原样复制；真实密钥不要发到聊天、工单或仓库。

### 1. 先确认部署和 Redis

访问：

```bash
curl -fsS https://<message-weave-domain>/api/status
curl -fsS https://<message-weave-domain>/healthz
```

当 `REDIS_URL` 与 `CONFIG_ENCRYPTION_KEY` 正确时，`/api/status` 应包含：

```json
{"ready":true,"mode":"configured","missing":[]}
```

如果缺少变量，容器不会退出，而是进入 `configuration-setup`；SPA 只显示缺少的变量，不会显示管理会话授权区。Redis URL 必须包含正确协议：`rediss://` 表示 TLS，`redis://` 表示明文连接。TLS Redis 不能只在 `redis-cli` 命令中加 `--tls`，应用本身的 `REDIS_URL` 也必须使用 `rediss://`。

### 2. 创建 Telegram Bot 和测试群

在 Telegram 中打开 `@BotFather`，发送 `/newbot`，按提示创建机器人并保存 Bot Token。然后新建“群组”（不是频道），例如 `MessageWeave`，把机器人加入群并设置为管理员。

在群里发送 `/start`，然后临时使用 Bot API 的 `getUpdates` 查看 Chat ID：

```bash
curl "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/getUpdates"
```

返回中的：

```json
"chat":{"id":-5260770881,"title":"MessageWeave","type":"group"}
```

其中 `-5260770881` 就是 Chat ID。群 ID 通常是负数，负号不能删除。`getUpdates` 不是群组列表接口；只有机器人实际收到消息后才会返回该群。若返回空数组，先在群里发送 `/start`。若已经设置 Webhook，先删除它，否则消息会被 Webhook 消费：

```bash
curl -X POST "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/deleteWebhook"
```

如果机器人看不到普通消息，在 `@BotFather` 执行 `/setprivacy`，选择机器人并选择 `Disable`；或者确保机器人是管理员。调试结束后再设置 Webhook。

### 3. 生成应用侧鉴权密钥

以下三个值必须分别生成，不能使用 Redis 密码、Bot Token 或 SPA 管理会话：

```bash
openssl rand -hex 32   # Telegram Webhook Secret
openssl rand -hex 32   # Reconcile Token
openssl rand -hex 32   # Worker Token
```

- **Telegram Webhook Secret**：由你生成，填写 SPA，并在 Telegram `setWebhook` 时作为 `secret_token`。MessageWeave 会校验请求头 `X-Telegram-Bot-Api-Secret-Token`。
- **Reconcile Token**：由你生成，供外部 Cron 调用 `/reconcile`。
- **Worker Token**：由你生成，供 `/worker` 和兼容管理 API 使用。

### 4. 创建 SPA 管理会话

打开 `https://<message-weave-domain>/`，输入部署时 `REDIS_URL` 中 Redis ACL 用户的密码，点击创建管理会话。会话有效期 900 秒，只保存在当前页面内存，刷新页面后需要重新授权。

### 5. 填写 Telegram 业务配置

示例：

```text
目标 Chat ID:           -5260770881
Chat allowlist:         -5260770881
Telegram Webhook Secret: <第一组 openssl 随机值>
Reconcile Token:         <第二组 openssl 随机值>
Worker Token:            <第三组 openssl 随机值>
```

`目标 Chat ID` 和 `Chat allowlist` 不代表同一个字段：前者是默认目标，后者是允许触发机器人的白名单；测试单个群时可以填写相同的 ID。allowlist 支持每行一个或逗号分隔，重复项会被拒绝。

### 6. 填写 JMAP 业务配置

- `JMAP Session URL` 可填 `https://mail.example.com` 或 `https://mail.example.com/.well-known/jmap`；两者会被归一化。
- 只能使用 HTTPS；URL 中不要放用户名、密码、query 或 fragment。
- `JMAP Username` 填邮箱账号。
- `JMAP Password` 填 Stalwart 应用专用密码。
- `Account ID` 初次联调可留空，使用 JMAP Session 的主账户。
- 不需要在 Stalwart WebUI 手工创建 PushSubscription。通常内置 `user` 角色已经包含 PushSubscription 的读取、创建、修改和删除权限；只有实际注册返回 `forbidden` 时，管理员才需要在 `/admin → Management → Directory → Accounts/Roles` 检查角色。Stalwart 的 Push 全局参数位于 `Settings → Network → JMAP → Push`，它不是具体订阅配置。
- LLM 初次联调建议关闭，避免把邮件问题和 AI 网络配置混在一起。

### 7. 注册 JMAP PushSubscription

MessageWeave 不要求你在 Stalwart WebUI 中手工创建 PushSubscription。保存 JMAP 业务配置后，使用当前 SPA 管理会话或 `WORKER_TOKEN` 调用注册接口，并明确提供公网 HTTPS callback URL：

```bash
curl -X POST "https://<messageweave-domain>/api/push/register" \
  -H "Authorization: Bearer <admin-session-or-worker-token>" \
  -H "Content-Type: application/json" \
  -d '{"callback_url":"https://<messageweave-domain>/push/jmap"}'
```

成功返回 `push_subscription_id`。URL 必须是 HTTPS，不能包含用户名、密码或其他内嵌凭据。相同 callback URL 的重复注册请求会复用已有订阅，避免重复创建。Stalwart 随后向 `/push/jmap` 发送它生成的 `PushVerification`；MessageWeave 自动通过 JMAP `PushSubscription/set` 回写验证码，验证成功后才接收正式 Push。注册接口不会猜测平台域名，也不会接受用户填写验证码。

普通 `user` 角色通常已经具备 PushSubscription 的读取、创建、修改和删除权限，无需手工配置；只有实际返回 `forbidden` 时，管理员才检查 `/admin → Management → Directory → Accounts/Roles`。Stalwart 的 `Settings → Network → JMAP → Push` 是全局 Push 参数，不是具体订阅创建页面。

依据：[Stalwart Push notifications](https://stalw.art/docs/http/jmap/push/)、[Stalwart Permissions](https://stalw.art/docs/auth/authorization/permissions/)、[Stalwart Roles](https://stalw.art/docs/auth/authorization/roles/)、[RFC 8620 §7.2 PushSubscription](https://www.rfc-editor.org/rfc/rfc8620.html)。

保存业务配置时服务端会真实建立 JMAP 客户端；建立失败返回 `503`，不会写入坏配置，可以直接修正后再次保存。业务配置没有 GET 接口，保存成功后密钥不会回显；完整替换时必须重新输入必填项。

### 8. 设置 Telegram Webhook 并打开业务开关

业务配置保存成功后执行：

```bash
curl -X POST "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/setWebhook" \
  --data-urlencode "url=https://<message-weave-domain>/webhook/tg" \
  --data-urlencode "secret_token=<TELEGRAM_WEBHOOK_SECRET>"

curl "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/getWebhookInfo"
```

确认返回的 `url` 正确，并且没有持续增长的 `last_error_message`。然后回到 SPA 打开“启用业务处理”。全局开关关闭时，Webhook、Reconcile 和 Worker 都返回 `503 service disabled`，不会入队，也不会 ACK 上游消息。

最后在白名单群里发送测试消息。收到消息后观察 Telegram `getWebhookInfo`、平台日志和机器人回复；如果没有回复，优先检查 Chat ID 的负号、allowlist、Webhook Secret、全局开关和机器人管理员权限。

### 9. Reconcile 鉴权测试

```bash
curl -i -X POST "https://<message-weave-domain>/reconcile" \
  -H "Authorization: Bearer <RECONCILE_TOKEN>"
```

错误 Token 必须返回 `401`。外部 Cron 建议每 5–10 分钟调用一次，用于 Push 丢失时的邮件对账兜底。

## 当前邮件同步方式

当前版本不要把 PushSubscription 当作唯一可靠来源；Push 回调仍需鉴权后入队，邮件同步请同时使用外部 Cron 定时调用 `/reconcile`。对账使用 Redis 持久 `sinceState`，并在入队失败时返回 `503`，不会提前推进游标。

如果部署平台会让服务休眠，Cron 的 HTTP 请求通常可以唤醒容器，但首次请求会承担冷启动延迟，可能超出 Cron 或平台的请求超时时间。因此：

- 将 Cron 超时时间设置得足够长，并允许失败重试；
- 频率不要高于平台允许的唤醒/请求限制；
- 不要把 `/healthz` 当作邮件同步，它只检查进程存活；
- 如果平台休眠期间无法唤醒服务，Reconcile 也无法执行，此时需要关闭休眠或改用常驻实例。

对账锁会在长任务运行期间由持有者续租；续租失败不会推进游标。Push 注册和对账都使用 Redis owner-token 单飞锁，避免并发请求重复创建订阅或并行推进同一个 JMAP 游标。

## 常见踩坑与排查

### Redis 报 `can't connect with TLS, the feature is not enabled`

通常是镜像构建时 Redis TLS feature 未启用，或 `redis://` / `rediss://` 写错。使用包含 TLS 支持的最新镜像，并确保 `REDIS_URL=rediss://...`。Upstash 密码中的特殊字符必须 URL 编码，例如 `@` 写成 `%40`。

### Redis 报 `Multiplexed connection driver unexpectedly terminated`

先用同一个 URL 在外部验证网络和凭据；不要只验证 `redis-cli --tls` 而应用仍使用 `redis://`。检查 HostStack/平台是否允许出站 6379/TLS，检查 URL 中 ACL 用户、密码和端口。

### Rustls 报 provider panic

这是旧镜像同时加载多个 TLS provider 的问题。必须使用包含显式 rustls `ring` provider 安装逻辑的最新镜像，不要继续使用旧镜像标签缓存。

### `getUpdates` 返回空数组

机器人没有收到新消息，或消息已经被 Webhook 消费。删除 Webhook 后在群里重新发送 `/start`；确认机器人已加入群、是管理员，并在需要时通过 BotFather 关闭隐私模式。

### Telegram 提示无权读取消息

确认创建的是群组而不是频道；把机器人设为管理员；重新发送 `/start`。隐私模式开启时，机器人通常只能看到命令、回复和提及它的消息。

### SPA 保存业务配置返回 `503`

这通常表示 JMAP URL 无法从部署平台访问、证书/DNS 有问题、应用密码错误，或 LLM 配置不符合 HTTPS 校验。保存失败会保留旧 worker 和旧配置，不会锁死初始化槽位；修正后可重试。

### SPA 显示 configuration-setup

检查 HostStack Secret 或容器环境变量中是否同时存在：

```text
REDIS_URL
CONFIG_ENCRYPTION_KEY
```

`CONFIG_ENCRYPTION_KEY` 必须是 32 字节十六进制值，例如：

```bash
openssl rand -hex 32
```

它不能写在公开的 `hoststack.yaml`、Dockerfile、README 或镜像中。

## 隐私与运维红线（对用户透明）

- **无数据库、无本地文件**：机器人所有状态都放在你的 Redis 里；容器自身不写任何本地文件、不挂本地卷，重启后由 Redis + 邮箱对账恢复。
- **日志只走标准输出**：容器只往 stdout/stderr 打印日志，由你的平台/采集器负责收集与落盘；机器人自己**不写日志文件**。
- **日志里不会写敏感内容**：邮箱正文、密钥、AI 请求/响应、附件内容**都不会**出现在日志或 Redis 里，只记录结构化事件、计数、时间戳、脱敏摘要。
- 详见 `docs/deployment.md` §0 与 §8.2 的检查清单。

---

## 目前能做到什么 / 还差什么

**已经有了**

- 入口框架、三处安全鉴权、配置加载。
- 配置管理 SPA：使用短期 Redis admin session；可编辑完整业务配置并热加载，也可读取和保存 `/api/config` 运行参数。
- 读取 Stalwart 邮箱的基础代码（列文件夹、列邮件、读原文）。
- Telegram Webhook、Chat ID 白名单、Reconcile/Worker Bearer Token 和 Redis 持久化全局启用开关。
- HostStack 原生 Rust 部署配置与 Docker 构建配置。
- **可选**：一个免费 Cloudflare Worker 前置负载均衡（多实例 HA），见 `docs/deployment.md` §10 与 `cloudflare-worker/` 子目录。

**还差 / 尚未验证**

- ⚠️ **真实邮箱联调需要部署者验证**：JMAP 客户端代码和配置校验已提供，但不同 Stalwart 网络、权限和应用密码仍可能不同。
- **JMAP Push 注册已接入**：通过 `POST /api/push/register` 显式提交 HTTPS callback URL 后，后端调用 `PushSubscription/set create`，并由 `/push/jmap` 自动完成验证码回写。当前仍需真实 Stalwart 环境验证；外部 Cron 调用 `/reconcile` 继续作为可靠补偿通道。
- 生产级多实例协调和发送邮件能力仍未完成；AI 总结属于可选能力，需另行配置并验证。

Docker 镜像和 HostStack 原生 Rust 部署均已提供，但仍建议先按本文完成小范围联调，再考虑生产使用。

## 对开发者的门禁

任何改动发布前须全绿（在 Debian `rust:1-slim-bookworm` 容器内执行，见 `AGENTS.md` §5 与 `docs/deployment.md` §8）：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- 真机验证用例以 `#[ignore]` 标记、由环境变量驱动（`cargo test -- --ignored jmap::`），CI 默认不跑；未实际运行前不得声称“真机通过”。
- 涉及安全边界的改动需同步 `docs/design.md`、`docs/deployment.md`、`AGENTS.md` 三份文档。

---

## 给开发/技术人员

架构、设计、部署细节见：

- [架构与设计](docs/design.md)
- [部署与运维](docs/deployment.md)
- [给 AI 编码助手的规则](AGENTS.md)
- [未完成项与阻塞原因](docs/todo.md)

## 许可证

（待定）
