# MessageWeave

**[English documentation / 英文文档 → README.md](README.md)**

> 一个邮箱对应一个 Telegram bot。新邮件以元数据形式送达；原文始终从你自己的 JMAP 服务器
> 只读拉取。

## 1. 是什么

MessageWeave 位于 JMAP 邮件服务器（例如 Stalwart）与 Telegram bot 之间。

- **入站。** JMAP push subscription 通知本服务"有消息变更"。静止时不做任何轮询。
- **出站。** 每个事件变成一条简短的**仅元数据** Telegram 通知：发件人、主题、时间戳。
  邮件正文永不发往 Telegram。
- **按需取原文。** 你询问时，服务通过一次新的 JMAP read 调用取回原文。中间不做缓存。
- **可选摘要。** 你显式同意时，由 OpenAI-compatible LLM 返回简短摘要。请求、响应与附件
  字节一律不持久化。

在这里，状态是架构边界，不是偏好选择。

- **Redis 是唯一状态存储。** 无数据库、无内嵌存储、无第二个存储。
- **进程不写任何磁盘。** 无日志文件、无数据文件、无本地卷。
- **无长连接。** 无 SSE、无 WebSocket、无长轮询。
- **日志只写 stdout。** 日志存储与保留由平台采集器负责。
- **它不是什么。** 它不是邮件客户端，不是代理，也不是转发中继。它只把一个 bot 的元数据
  送达 Telegram；不抓取任意 URL、不开隧道、不中转流量。

## 2. 五分钟跑起来

需要可达的 Redis 7 实例、一个 JMAP session 与一个 Telegram bot token。

```sh
cp .env.example .env           # 填写 REDIS_URL 与 CONFIG_ENCRYPTION_KEY
docker build -t messageweave:latest .
docker run --env-file .env -p 8080:8080 messageweave:latest
```

然后在浏览器打开 `http://localhost:8080`。首启 SPA 调用 `POST /api/bootstrap`，把业务
密钥写入 Redis 并创建 admin session。此后浏览器通过 Redis 配置服务——业务配置不再读取或
写入任何环境变量。

不用 Docker 的本地构建：`cargo build --locked` 产出 `message-weave` 二进制。

如果服务有响应但什么都不做，请查看启动日志的告警：**缺少必需变量不会让进程崩溃**，它会
退化为运行在内存态上的只读配置路由。见 §3。

## 3. 配置入口

共三层，彼此不可替代。

**启动期环境变量 —— 2 个必需，2 个可选。**

| 变量 | 必需 | 默认值 |
|---|---|---|
| `REDIS_URL` | 是 | — |
| `CONFIG_ENCRYPTION_KEY` | 是 | — |
| `PORT` | 否 | `8080` |
| `RUN_MODE` | 否 | `webhook` |

`RUN_MODE` 只接受 `webhook` 或 `reconcile`。`.env.example` 只提供 2 个必需变量；其余是
文档化默认值。

**业务配置 —— 存于 Redis，由浏览器写入。**
`POST /api/bootstrap` 是一次性信任引导；`PUT /api/business-config` 可在约一秒内热更新单
项配置。正常运行中进程从不从环境变量读取业务密钥。

**健康检查。**
`GET /healthz` 无条件返回 200——它是存活探测，不是就绪探测。`GET /ready` 在配置、Redis 与
两条上游（JMAP session `GET`、Telegram `getMe`，各 3s）可达之前返回 503。判断是否可路由
流量时，请探测 `/ready` 而不是 `/healthz`；但 `/ready` 是较重的探针（最坏约 3s），需要
保持轻量的监控请看网关聚合的 `/healthz`。

## 4. 安全边界，一句话

> 所有状态都在 Redis 里；进程只读两个环境变量，不写任何磁盘。

有一条推论与部署直接相关：SPA 管理凭据——也就是 bootstrap 的信任根——就是 `CONFIG_ENCRYPTION_KEY` 本身，即启动时提供的那个 32 字节 hex 值。它只做常数时间比较，绝不回显、记录或落盘。Redis ACL 密码（如有）只认证 Redis 连接本身，不是任何 HTTP 端点的认证凭据。

## 5. 去哪读更多

| 文档 | 回答什么 |
|---|---|
| [`docs/design.md`](docs/design.md) | 为什么这样设计：数据流、JMAP 语义、Redis streams、AI 授权规则 |
| [`docs/deployment.md`](docs/deployment.md) | 怎么部署：Dockerfile、secrets、Redis 托管、webhook/push/对账路由、多实例负载均衡 |
| [`docs/reference.md`](docs/reference.md) | **可核对事实的唯一权威来源**——Redis 键与 TTL、错误码、路由、环境变量分层、预算常量 |
| [`docs/roadmap.md`](docs/roadmap.md) | 缺口、阻塞项与下一阶段目标 |
| [`docs/retired.md`](docs/retired.md) | 试过但没用的：废弃路线、未落地的设计与从未存在的名字 |
| [`AGENTS.md`](AGENTS.md) | 贡献规则、硬边界、跨文档引用的稳定 ID 索引 |

重要的事实都可追溯。两份文档不一致时，以 `docs/reference.md` 为准。
