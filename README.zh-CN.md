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

## 2. 两种部署形态

同一个镜像、同一个 Redis、同一套 SPA。唯一的区别是前面放什么，以及你把哪个 URL 交给 Telegram 和 Stalwart。

| | 形态 A：仅后端 origin | 形态 B：前置 Cloudflare Worker 网关 |
|---|---|---|
| 谁应答请求 | 一个后端主机 | 一个 Worker URL 对 N 个后端 origin |
| 故障转移 | 没有——origin 挂了就一直挂着 | 有界——默认 2 次尝试，只重试超时与 `5xx` |
| 你注册的回调 URL | `https://a.example` | `https://lb.example` |
| 可达路由 | 全部路由，含 `/api/bootstrap` 与 `/debug/*` | 只有 19 个白名单路径 |
| 要看哪个健康接口 | origin 上的 `GET /ready` | 透传的 `GET /ready`，外加网关聚合 `GET /healthz-worker` |
| 额外组件 | 无 | 一个 Worker 部署、一份 origin 列表 |
| 文档 | `docs/deployment.md §2` | `docs/deployment.md §10`、`cloudflare-worker/README.zh-CN.md` |

单 origin 且没有 HA 要求时选形态 A；两个及以上 origin、或想在滚动与重建某个实例时保持一个稳定的回调 URL，就选形态 B。

切换是一次重新注册，不是迁移。注册是幂等的，注册新地址会注销旧订阅，所以从形态 A 换到形态 B（或反过来）只是 SPA 里改一个字段。不要同时注册两个 origin：注册槽只有一个，平台入口必须保持内网（`C-LB-SINGLE-REG-URL`）。

形态 B 里没有任何 secret 搬到网关。它不持有任何业务凭据，也不校验任何认证头。真正要求一致的是每个后端都携带同一组 `SAF-AUTH-*` secret 与同一份加密业务配置——因为回调可能落在任何一个实例上，而任何实例都证明不了自己是被点名的那一个（`C-LB-SHARED-SECRETS`）。凭据不一致表现为随机 401，而不是路由错误。

`POST /api/bootstrap` 与 `/debug/*` 在两种形态下都只保留在 origin 直连，不经过网关暴露。

## 3. 五分钟跑起来

需要可用的 Redis 服务、一个 JMAP session 与一个 Telegram bot token。

这段是**本地容器冒烟测试**。生产部署可以用任何能跑起后端容器、注入加密 secret 并连到外部托管 Redis 的平台，见 `docs/deployment.md` §3 与 `docs/reference.md` 的平台相关部分。

```sh
cp .env.example .env           # 填写 REDIS_URL 与 CONFIG_ENCRYPTION_KEY
docker build -t messageweave:latest .
docker run --env-file .env -p 8080:8080 messageweave:latest
```

然后在浏览器打开 `http://localhost:8080`，输入你的 `CONFIG_ENCRYPTION_KEY` 值。SPA 用它
经 `POST /api/admin/session` 换取 1,800 秒（30 分钟）的管理会话，再用 `PUT /api/business-config`
把业务密钥写入 Redis。此后业务配置不再读取或写入任何环境变量。

不用 Docker 的本地构建：`cargo build --locked` 产出 `message-weave` 二进制。

如果服务有响应但什么都不做，请查看启动日志的告警：**缺少必需变量不会让进程崩溃**，它会
退化为运行在内存态上的只读配置路由。见 §4。

## 4. 配置入口

共三层，彼此不可替代。

**启动期环境变量 —— 2 个必需，1 个可选。**

| 变量 | 必需 | 默认值 |
|---|---|---|
| `REDIS_URL` | 是 | — |
| `CONFIG_ENCRYPTION_KEY` | 是 | — |
| `PORT` | 否 | `8080` |

没有 `RUN_MODE`：遗留的环境变量解析器已删除，webhook 与 reconcile 流量共享同一套路由表（`POST /reconcile` 是独立端点）。`.env.example` 只提供 2 个必需变量；其余是
文档化默认值。

**业务配置 —— 存于 Redis，由浏览器写入。**
`POST /api/bootstrap` 是一次性信任引导；`PUT /api/business-config` 可在约一秒内热更新单
项配置。正常运行中进程从不从环境变量读取业务密钥。

**健康检查。**
`GET /healthz` 无条件返回 200——它是存活探测，不是就绪探测。`GET /ready` 在配置、Redis 与
两条上游（JMAP session `GET`、Telegram `getMe`，各 3s）可达之前返回 503。判断是否可路由
流量时，请探测 `/ready` 而不是 `/healthz`；但 `/ready` 是较重的探针（最坏约 3s），需要
保持轻量的监控在启用网关时请看网关聚合 `GET /healthz-worker`。

**远程联调面（可选，默认关闭）。**
`/debug/*` 是一套可选的远程联调面：JMAP 与 Telegram 实时探针、当前业务配置，以及单条
Telegram 发送。它由双因子开关控制——进程必须带 `--debug` **且** 已设置 `DEBUG_TOKEN`；
任一缺失则这些路由完全不存在（请求落到通用 404）。`/debug/*` 不在网关白名单内，因此只能
直连后端 origin 访问。运行它唯一的规则就是不要打开：启动命令里不加 `--debug`，环境变量里不
设 `DEBUG_TOKEN`。若确实要为一次排障临时开启，请先配好 chat 白名单——白名单为空时，测试
发送不会受限到任何 chat。见 `docs/deployment.md` §2.1。

## 5. 安全边界，一句话

> 所有状态都在 Redis 里；进程只读两个环境变量，不写任何磁盘。

有一条推论与部署直接相关：SPA 管理凭据——也就是 bootstrap 的信任根——就是 `CONFIG_ENCRYPTION_KEY` 本身，即启动时提供的那个 32 字节 hex 值。它只做常数时间比较，绝不回显、记录或落盘。Redis ACL 密码（如有）只认证 Redis 连接本身，不是任何 HTTP 端点的认证凭据。

## 6. 去哪读更多

| 文档 | 回答什么 |
|---|---|
| [`docs/design.zh-CN.md`](docs/design.zh-CN.md) | 为什么这样设计：数据流、JMAP 语义、Redis streams、AI 授权规则 |
| [`docs/deployment.zh-CN.md`](docs/deployment.zh-CN.md) | 怎么部署：Dockerfile、secrets、Redis 托管、webhook/push/对账路由、多实例负载均衡 |
| [`docs/reference.zh-CN.md`](docs/reference.zh-CN.md) | **可核对事实的唯一权威来源**——Redis 键与 TTL、错误码、路由、环境变量分层、预算常量 |
| [`docs/opengaps.zh-CN.md`](docs/opengaps.zh-CN.md) | 缺口、阻塞项与下一阶段目标 |
| [`docs/retired.zh-CN.md`](docs/retired.zh-CN.md) | 试过但没用的：废弃路线、未落地的设计与从未存在的名字 |
| [`docs/charter.zh-CN.md`](docs/charter.zh-CN.md) | 项目章程：项目目标、锁定的技术选型、安全不变量、禁止事项与稳定 ID 注册表 |
| [`cloudflare-worker/README.zh-CN.md`](cloudflare-worker/README.zh-CN.md) | Cloudflare Worker 网关：路由白名单、超时与重试预算、有界故障转移、健康聚合、部署 |
| [`AGENTS.md`](AGENTS.md) | 语言无关的工程规范：代码风格、配置与密钥、构建环境、测试门禁、文档治理 |

重要的事实都可追溯。两份文档不一致时，以 `docs/reference.md` 为准。
