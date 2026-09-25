# MessageWeave

一个把 **Stalwart 邮箱**接入 **Telegram** 的个人邮件助手：新邮件来了在 Telegram 提醒你，还能按需查看/总结邮件内容。

> ⚠️ **当前仍属早期阶段，还不能真正使用。**
> 目前只有底层功能骨架；**真实邮箱的联调还没验证**。仓库已提供 Debian Dockerfile，但仍不建议生产部署。

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
3. **一个 Redis 数据库**（你托管，建议开启 AOF 持久化）。机器人所有状态都放在这里。
4. **一个公网 HTTPS 地址**。平台会把这个地址指向机器人容器（HTTPS 由平台自动提供）。
5. **一个定时任务（外部 Cron / 调度器）**：每隔 5–10 分钟访问一次 `/reconcile` 地址，让机器人定期和邮箱对账。机器人自己不会设置定时器。

### 需要登记的地址
- 告诉 Telegram：把 webhook 指向 `你的公网地址/webhook/tg`
- 告诉 Stalwart：把 Push 回调指向 `你的公网地址/push/jmap`

> 如果你使用「多实例 + Cloudflare Worker 负载均衡」（可选，见 `docs/deployment.md` §10），则把上面三个 URL 指向 **Worker 的稳定域名**（`https://<your-worker>.workers.dev/webhook/tg` 等），不再指向各后端的平台地址。

---

## 在 Docker 管理器中创建容器

创建一个新容器，填好镜像、端口、内存等常规项后，只需注入 Redis 连接凭据：

| 环境变量 | 必须？ | 填什么 |
|---|---|---|
| `REDIS_URL` | ✔ | Redis 连接串（含密码），例如 `redis://:密码@redis.example.com:6379/0` |

首次启动后，通过受保护的 HTTPS `/api/bootstrap` 提交 Telegram/JMAP/LLM 和业务鉴权配置。配置密钥写入 Redis，API 永不回显；管理员 PUT 成功后会校验并原子热重建客户端，后续请求即时生效，失败保留旧实例。

AI 摘要默认关闭。Telegram 用户可发送 `/ai on`（1小时）、`临时一次`、`1小时`、`今天`、`7天` 或 `直到我撤销` 明确授权，再发送 `/summary <email_id>`；授权到期不会自动续期，`/ai off` 立即撤销。未授权时仅返回邮件元数据，LLM 失败则回退为本地截断摘要。

> 缺少 `REDIS_URL` 或 `CONFIG_ENCRYPTION_KEY` 时服务进入 configuration-setup 模式，仍提供 SPA、`/api/status` 和探针；SPA 隐藏管理会话授权区，只显示缺少的环境变量与密钥不保存、不回显说明。配置恢复后 `/api/status` ready=true 才显示授权入口。

### HostStack 原生部署

HostStack 使用根目录 `hoststack.yaml` 构建并运行 Rust 服务：执行
`cargo build --release --locked`，运行 `./target/release/message-weave`，并以 `/healthz`
进行 30 秒间隔、5 秒超时的健康检查。请在 HostStack Secret 中注入 `REDIS_URL` 与
`CONFIG_ENCRYPTION_KEY`；密钥不写入 YAML、镜像或仓库文件。

---

## 启动后怎么看它是否正常

- 浏览器或命令行访问 **`你的公网地址/healthz`**：能返回正常状态，说明机器人进程活着。
- 目前 `/ready` 只是占位，**还不能用它判断邮箱/Redis 是否正常**（这个检查以后才会补上）。

## 配置管理

打开服务根地址 **`你的公网地址/`**，输入 `REDIS_URL` 中 Redis ACL 用户的密码，创建有效期 15 分钟的管理会话。会话与表单中的密钥只保存在当前页面内存；刷新或关闭页面后需要重新授权。页面不使用浏览器存储或 Cookie 保存会话、密钥和配置。

页面分开管理两类配置：运行参数（JMAP、Telegram、LLM 请求超时与最大重试次数）可通过 API 读取和保存；业务配置（JMAP、Telegram、LLM、chat allowlist 与各鉴权 token）由 `PUT /api/business-config` 完整替换。为避免密钥回显，服务端没有业务配置读取接口，因此每次完整替换都必须重新输入必填项。保存收到 HTTP 204 后页面会提示新配置已热加载，并清空密钥输入框。

Redis 尚未连接或初始化时，管理会话或配置 API 会返回 `503`；页面会说明不可用状态，并在运行参数读取成功前禁用其保存。经 Cloudflare Worker 访问时使用 Worker 根地址；管理 API 路径也已纳入受限路由白名单。

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
- **可选**：一个免费 Cloudflare Worker 前置负载均衡（多实例 HA），见 `docs/deployment.md` §10 与 `cloudflare-worker/` 子目录。

**还差 / 尚未验证**
- ⚠️ **真实邮箱联调没做**：连真实 Stalwart 的测试因还没有测试环境而跳过，**没有宣称“真机通过”**。
- **Dockerfile 已提供**（Debian slim 多阶段构建）；真实邮箱联调仍未验证，不能按上面的“创建容器”用于生产。
- Telegram 通知推送、AI 总结、发送邮件等功能**都还没做**。

所以：**现在还没有可以下载使用的镜像，请自行构建并等待后续版本。**

---

## 给开发/技术人员

架构、设计、部署细节见：

- [架构与设计](docs/design.md)
- [部署与运维](docs/deployment.md)
- [给 AI 编码助手的规则](AGENTS.md)

## 许可证

（待定）
