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

**Cloudflare Worker 负载均衡器是可选项。** 后端没有任何东西依赖它，有它和没有它跑起来完全一样。它存在的理由是单一的：Cloudflare 的 Load Balancer 产品在免费套餐上不可用，所以 Worker 是免费套餐账户在多个后端前面做负载均衡与故障转移的办法。

**它也不是安全层，本意不是拿来干这个的。** 没有隐藏后端的意图，也没有加固后端的意图；负载均衡器在不在，origin 都同样可以直接访问，边缘上也没有加任何访问控制。固定的路由白名单是让它在有限的一组路径上做负载均衡的手段，不是防火墙。

| | 形态 A：仅后端 origin | 形态 B：前置 Cloudflare Worker 负载均衡器 |
|---|---|---|
| 谁应答请求 | 一个后端主机 | 一个 Worker URL 对 N 个后端 origin |
| 故障转移 | 没有——origin 挂了就一直挂着 | 有界——默认 2 次尝试，只重试超时与 `5xx` |
| 你注册的回调 URL | `https://a.example` | `https://lb.example` |
| 后端可达性 | 网络上直连可达 | 同样直连可达——负载均衡器不加访问控制 |
| 负载均衡器路由集 | 不适用——没有负载均衡器 | 固定 19 个路径；未知路径返回 `404` |
| 要看哪个健康接口 | origin 上的 `GET /ready` | 透传的 `GET /ready`，外加负载均衡器聚合 `GET /healthz-worker` |
| 额外组件 | 无 | 一个 Worker 部署、一份 origin 列表 |
| 文档 | `docs/deployment.md §2` | `docs/deployment.md §10`、`cloudflare-worker/README.zh-CN.md` |

单 origin 且没有 HA 要求时选形态 A；两个及以上 origin、或想在滚动与重建某个实例时保持一个稳定的回调 URL，就选形态 B。

切换是一次重新注册，不是迁移。注册是幂等的，注册新地址会注销旧订阅，所以从形态 A 换到形态 B（或反过来）只是 SPA 里改一个字段。不要同时注册两个 origin：注册槽只有一个，后端的平台入口不对外注册（`C-LB-SINGLE-REG-URL`）。注意这里说的是「不注册」，不是「不可达」——两种形态下 origin 都同样可以直接访问。

形态 B 里没有任何 secret 搬到负载均衡器。它不持有任何业务凭据，也不校验任何认证头。真正要求一致的是每个后端都携带同一组 `SAF-AUTH-*` secret 与同一份加密业务配置——因为回调可能落在任何一个实例上，而任何实例都证明不了自己是被点名的那一个（`C-LB-SHARED-SECRETS`）。凭据不一致表现为随机 401，而不是路由错误。

`POST /api/bootstrap` 与 `/debug/*` 不在负载均衡器的路由集里，所以发给 Worker URL 的请求到不了它们。这是白名单在需要被负载均衡的那些路径上按设计工作，并不是在隐藏什么——两种形态下 origin 都同样可以通过自己的地址直接访问。

## 3. 从零开始

本节是手把手教程：创建 Telegram bot、部署一个后端、填写配置页、注册回调、收到第一条通知。字段语义、默认值、TTL 与错误码在 `docs/reference.md`；生产、扩展与负载均衡器运维在 `docs/deployment.md`。

### 3.0 你需要准备什么

- 一个你管理的 JMAP 邮箱（本指南以 Stalwart 为例），必须 HTTPS。
- 一个 Telegram 账号。
- 一个带持久化的 Redis 实例（Upstash 免费版可用；它是纯 TLS 的，URL 以 `rediss://` 开头）。
- 一台能跑容器并暴露一个 HTTP 端口的机器或平台。
- 一个能接收 webhook 的公网 HTTPS 域名——自有域名、隧道或 Cloudflare 路由都行。Telegram 与 Stalwart 都要求 HTTPS。
- 可选：OpenAI 兼容的 LLM 密钥；想要负载均衡器的话再来一个 Cloudflare 账号。

不需要别的：没有 Postgres、没有消息队列、没有 Kubernetes。

### 3.1 创建 Telegram bot

1. 在 Telegram 中打开 `@BotFather` 的对话，发送 `/newbot`。
2. 选一个显示名，再选一个以 `bot` 或 `_bot` 结尾的用户名。
3. BotFather 会回复一串 token，形如 `1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw1`。这就是 bot token。当成密码对待：拿到它的人能读取你的 webhook 流量、以你的 bot 身份发消息。一旦保存，API 不再回显它。
4. 用应当接收通知的账号或群给新 bot 发任意一条消息（例如 `/start`）。在此之前 Telegram 没有可投递的对话。

### 3.2 找到 chat id

1. 在浏览器打开 `https://api.telegram.org/bot<TOKEN>/getUpdates`，把 `<TOKEN>` 替换为第 1 步拿到的 token。
2. 在 JSON 中找 `result[].message.chat.id`。私聊是正数，例如 `123456789`；群与超级群是负数，例如 `-1001234567890`。
3. 如果是群：先把 bot 拉进群，再在群里发一条消息，然后才调 `getUpdates`。bot 在群里只有在关闭隐私模式后才能看到非命令消息：`@BotFather` → `/setprivacy` → `Disable`。

这个 id 在第 6 步要用两次：一次当通知目标，一次放进入站白名单。白名单必须至少一个 id——空名单会被判为非法配置而拒绝，不会当成「允许所有人」。

### 3.3 拿到 JMAP 凭据

1. 在 Stalwart 管理端为邮箱创建一个应用密码——是应用密码，不是账号登录密码。后端用 HTTP Basic 走 JMAP。
2. session URL 就是你的邮箱源站：`https://mail.example.com`。也可以粘贴 `https://mail.example.com/.well-known/jmap`；后端会把两者都折叠成源站，并拒绝带凭据、查询串或片段的 URL。
3. JMAP account id 是可选的。留空：首次保存成功后后端会从服务器读回，页面会显示出来。

### 3.4 跑起后端

容器只需要两个环境变量：

```sh
# .env — 不要提交这个文件
REDIS_URL=rediss://default:<password>@<host>:6379
CONFIG_ENCRYPTION_KEY=<64 位十六进制，用：openssl rand -hex 32>
```

- `REDIS_URL` 是你唯一的状态存储。像数据库一样备份它。
- `CONFIG_ENCRYPTION_KEY` 兼作 SPA 管理员凭据与已存业务密钥的加密密钥。丢了它，你既登不上配置页，也解不开已存密钥；轮换它，之前存的密钥全部不可读。

然后构建并启动：

```sh
docker build -t messageweave .
docker run --rm --env-file .env -p 8080:8080 messageweave
curl localhost:8080/healthz   # → ok
```

不用 Docker 的本地构建：`cargo build --locked` 产出 `message-weave` 二进制。

`GET /healthz` 是静态存活回复，不碰 Redis。就绪报告是 `GET /ready`：在配置存在且所有已配置上游可达之前回答 503，之后回答 200 并带一份 JSON 组件报告。现在把你的 HTTPS 反代或隧道指向这个端口——第 6、7 步需要一个公网 HTTPS 域名。

如果服务有响应但什么都不做，请查看启动日志的告警：**缺少必需变量不会让进程崩溃**，它会退化为运行在内存态上的只读配置路由。见 §4。

### 3.5 登录配置页

1. 在浏览器打开页面。如果后端带着所需变量启动，第一张卡是 **Create admin session**。
2. 粘贴 `CONFIG_ENCRYPTION_KEY`——不是 Redis 密码。这会签发一个管理员会话，只活在页面内存里，有效期 30 分钟；刷新页面即登出。

### 3.6 填写业务配置

表单是整存整换，不是增量补丁：你提交的就是整份存储配置。各节：

- **01 JMAP mailbox**——session URL、用户名，以及第 3.3 步的应用密码。
- **02 Telegram and access auth**——bot token、通知 chat id、白名单，加上你自己生成的三个密钥：webhook 密钥、reconcile token 与 worker token。各自用 `openssl rand -hex 32` 生成。webhook 密钥之后要在 Telegram 侧对得上；reconcile 与 worker token 保护调度端点。
- **03 LLM service**——可选。不需要 AI 摘要就别开；一旦开启，HTTPS base URL、模型名与 API key 三者都变成必填。
- **04 Notification display**——通知里收信时间的渲染时区。

然后按顺序：

1. 点 **Test connection**。它预检凭据但不保存任何东西。
2. 点 **Save business config and hot-reload**。首次保存必须包含所有必填字段，否则服务器回答 422 且什么也不存。
3. 读按钮下方那行结果：`runtime_applied: true` 表示新运行时已在本后端装配；`false` 表示存了但运行时未装配，`warnings` 数组会指出失败的组件——最常见是 `jmap_session_url` 不可达。改字段再存一次；`/ready` 会一直 503 直到它变绿。
4. 如果保存回答 **409 conflict**，说明有人（另一个标签页、第二个运维者）在中间也存了。刷新页面拿到他们的值，再把你的改动重新套上去。

密钥是只写的：页面永不再显示已存密钥，提交空的密钥字段会保留之前的值——要清密钥就在单独一次编辑里故意留空。

### 3.7 注册回调

仍在同一页，**External callbacks** 区：

1. 粘贴能到达本后端的公网 HTTPS 源站，只写 scheme + host，不带路径——例如 `https://mw.example.com`。如果你把 Cloudflare 负载均衡器放在前面，就填负载均衡器的源站，不是后端的：回调要能在后端重启后仍然可达，而负载均衡器才是那个不掉线的。
2. 点 **Register callbacks**。一次点击注册双向：Telegram webhook 注册到 `/webhook/tg`，Stalwart push 订阅注册到 `/push/jmap`。验证自动完成，页面上报结果。

注册是显式的、可重复的。注册新源站会取消上一个订阅，于是旧源站不再收到 push。失败的注册绝不会删掉旧订阅——如果连不上 Telegram，上一个 webhook 原样留着；如果连上了 Telegram 但没连上 Stalwart，页面会报部分失败并让你再点一次。

在终端验证：

```sh
curl -s "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
# 期望 "url": "https://mw.example.com/webhook/tg" 且 "pending_update_count": 0
```

### 3.8 打开处理开关

把页面顶部的 **Enable business processing** 拨开。这是总开关：关着时 webhook 照收，但 `/worker` 与 `/reconcile` 拒绝排空，所以什么也到不了 Telegram。打开后，入站邮件与消息才会进队列、由 worker 排空。

### 3.9 端到端验证

1. 给自己发一封邮件。几秒内 bot 应当贴出一条通知，含发件人、主题与收信时间——绝不包含正文。这一条路径同时证明 JMAP 入站、Redis、队列与 Telegram 发送全通。
2. 在 Telegram 里试 `/search 发票`。注意：搜索触发词是中文 `搜索`、`查找`、`检索`，外加 `/search` 命令——`search invoice` 这类英文关键词不会被识别为触发。
3. 从通知里的 email id 试 `/summary <email_id>`。首次调用会请求确认；只有你确认之后、且 LLM 节开启时，摘要才会发出。

### 3.10 排程排空

Push 投递可能延迟或丢失；外部调度器是兜底。在任意一台能访问你源站的机器上，每 5–10 分钟跑一次：

```sh
MW_APP_URL=https://mw.example.com \
MW_WORKER_TOKEN=<worker token> \
MW_RECONCILE_TOKEN=<reconcile token> \
scripts/cron-drain.sh
```

脚本会排空队列（`/worker`）并重扫漏掉的 push（`/reconcile`）；两者成功都回答 204 空 body。`MW_RECONCILE_TOKEN` 是可选的——省略时跳过 reconcile 步骤，只排空队列。如果你把负载均衡器放在前面，`/worker` 必须直达后端源站——原因见 `docs/deployment.md` §10。

### 3.11 出问题时

- **保存后什么也没到。** 查处理开关（第 3.8 步），再查 `getWebhookInfo`（第 3.7 步）。`last_error_message` 里提到 401，说明 webhook 密钥与 Telegram 发来的不一致——重存 02 节并重新注册。
- **`/ready` 回 503。** 503 的 body 只是一个错误码，不会指名是哪个组件挂了。`/ready` 回 200 时，JSON 里有逐组件布尔值（`jmap`、`telegram`、`redis`）。常见原因：`jmap_session_url` 不可达、应用密码错、或上次保存 `runtime_applied: false`。
- **表单提示冲突。** 两个会话同时存了；刷新重做（第 3.6 步）。
- **bot 在群里不理你。** 隐私模式开着；去 `@BotFather` 关掉（第 3.2 步）。
- **`/search` 回的是帮助提示。** 你的查询以拉丁词开头；触发词是 `搜索` / `查找` / `检索`。

## 4. 配置入口

共三层，彼此不可替代。

**启动期环境变量 —— 2 个必需，1 个可选。**

| 变量 | 必需 | 默认值 |
|---|---|---|
| `REDIS_URL` | 是 | — |
| `CONFIG_ENCRYPTION_KEY` | 是 | — |
| `PORT` | 否 | `8080` |

`.env.example` 只提供 2 个必需变量；其余是
文档化默认值。

**业务配置 —— 存于 Redis，由浏览器写入。**
`POST /api/bootstrap` 是一次性信任引导；`PUT /api/business-config` 可在约一秒内热更新单
项配置。正常运行中进程从不从环境变量读取业务密钥。

**健康检查。**
`GET /healthz` 无条件返回 200——它是存活探测，不是就绪探测。`GET /ready` 在配置、Redis 与
两条上游（JMAP session `GET`、Telegram `getMe`，各 3s）可达之前返回 503。判断是否可路由
流量时，请探测 `/ready` 而不是 `/healthz`；但 `/ready` 是较重的探针（最坏约 3s），需要
保持轻量的监控在启用负载均衡器时请看负载均衡器聚合 `GET /healthz-worker`。

**远程联调面（可选，默认关闭）。**
`/debug/*` 是一套可选的远程联调面：JMAP 与 Telegram 实时探针、当前业务配置，以及单条
Telegram 发送。它由双因子开关控制——进程必须带 `--debug` **且** 已设置 `DEBUG_TOKEN`；
任一缺失则这些路由完全不存在（请求落到通用 404）。`/debug/*` 不在负载均衡器白名单内，因此只能
直连后端 origin 访问。运行它唯一的规则就是不要打开：启动命令里不加 `--debug`，环境变量里不
设 `DEBUG_TOKEN`。若确实要为一次排障临时开启，请先配好 chat 白名单——白名单为空时，测试
发送不会受限到任何 chat。见 `docs/deployment.md` §2.1。

## 5. 安全边界，一句话

> 所有状态都在 Redis 里；进程只读两个环境变量，不写任何磁盘。

有一条推论与部署直接相关：SPA 管理凭据——也就是 bootstrap 的信任根——就是 `CONFIG_ENCRYPTION_KEY` 本身，即启动时提供的那个 32 字节 hex 值。它只做常数时间比较，绝不回显、记录或落盘。Redis ACL 密码（如有）只认证 Redis 连接本身，不是任何 HTTP 端点的认证凭据。

前置在后端前面的可选 Cloudflare Worker 负载均衡器不在这个边界内。它不持有任何凭据，也不校验任何认证头；负载均衡器在不在，后端都同样可以直接访问，所以它不会改变攻击者与你的邮件之间的任何东西。见 §2。

## 6. 去哪读更多

| 文档 | 回答什么 |
|---|---|
| [`docs/design.zh-CN.md`](docs/design.zh-CN.md) | 为什么这样设计：数据流、JMAP 语义、Redis streams、AI 授权规则 |
| [`docs/deployment.zh-CN.md`](docs/deployment.zh-CN.md) | 怎么部署：Dockerfile、secrets、Redis 托管、webhook/push/对账路由、多实例负载均衡 |
| [`docs/reference.zh-CN.md`](docs/reference.zh-CN.md) | **可核对事实的唯一权威来源**——Redis 键与 TTL、错误码、路由、环境变量分层、预算常量 |
| [`docs/opengaps.zh-CN.md`](docs/opengaps.zh-CN.md) | 缺口、阻塞项与下一阶段目标 |
| [`docs/retired.zh-CN.md`](docs/retired.zh-CN.md) | 试过但没用的：废弃路线、未落地的设计与从未存在的名字 |
| [`docs/charter.zh-CN.md`](docs/charter.zh-CN.md) | 项目章程：项目目标、锁定的技术选型、安全不变量、禁止事项与稳定 ID 注册表 |
| [`cloudflare-worker/README.zh-CN.md`](cloudflare-worker/README.zh-CN.md) | Cloudflare Worker 负载均衡器：路由白名单、超时与重试预算、有界故障转移、健康聚合、部署 |
| [`AGENTS.md`](AGENTS.md) | 语言无关的工程规范：代码风格、配置与密钥、构建环境、测试门禁、文档治理 |

重要的事实都可追溯。两份文档不一致时，以 `docs/reference.md` 为准。
