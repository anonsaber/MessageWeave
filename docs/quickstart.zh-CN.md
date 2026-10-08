# 1. 快速上手

英文：[quickstart.md](quickstart.md)

本文是为人类运维者准备的从零开始指南：创建 Telegram bot、部署一个后端、填写配置页、注册回调、
收到第一条通知。只包含步骤与决策。字段语义、默认值、TTL 与错误码在 [reference.md](reference.md)；
生产、扩展、网关运维在 [deployment.md](deployment.md)。

## 1.1 你需要准备什么

- 一个你管理的 JMAP 邮箱（本指南以 Stalwart 为例），必须 HTTPS。
- 一个 Telegram 账号。
- 一个带持久化的 Redis 实例（Upstash 免费版可用；它是纯 TLS 的，URL 以 `rediss://` 开头）。
- 一台能跑容器并暴露一个 HTTP 端口的机器或平台。
- 一个能接收 webhook 的公网 HTTPS 域名——自有域名、隧道或 Cloudflare 路由都行。Telegram 与
  Stalwart 都要求 HTTPS。
- 可选：OpenAI 兼容的 LLM 密钥；想要网关的话再来一个 Cloudflare 账号。

不需要别的：没有 Postgres、没有消息队列、没有 Kubernetes。

## 1.2 创建 Telegram bot

1. 在 Telegram 中打开 `@BotFather` 的对话，发送 `/newbot`。
2. 选一个显示名，再选一个以 `bot` 或 `_bot` 结尾的用户名。
3. BotFather 会回复一串 token，形如 `1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw1`。
   这就是 bot token。当成密码对待：拿到它的人能读取你的 webhook 流量、以你的 bot 身份发消息。
   一旦保存，API 不再回显它。
4. 用应当接收通知的账号或群给新 bot 发任意一条消息（例如 `/start`）。在此之前 Telegram 没有可投递
   的对话。

## 1.3 找到 chat id

1. 在浏览器打开 `https://api.telegram.org/bot<TOKEN>/getUpdates`，把 `<TOKEN>` 替换为第 3 步拿到的
   token。
2. 在 JSON 中找 `result[].message.chat.id`。私聊是正数，例如 `123456789`；群与超级群是负数，例如
   `-1001234567890`。
3. 如果是群：先把 bot 拉进群，再在群里发一条消息，然后才调 `getUpdates`。bot 在群里只有在关闭隐私模式
   后才能看到非命令消息：`@BotFather` → `/setprivacy` → `Disable`。

这个 id 在第 7 步要用两次：一次当通知目标，一次放进入站白名单。白名单必须至少一个 id——空名单会被
判为非法配置而拒绝，不会当成「允许所有人」。

## 1.4 拿到 JMAP 凭据

1. 在 Stalwart 管理端为邮箱创建一个应用密码——是应用密码，不是账号登录密码。后端用 HTTP Basic 走
   JMAP。
2. session URL 就是你的邮箱源站：`https://mail.example.com`。也可以粘贴
   `https://mail.example.com/.well-known/jmap`；后端会把两者都折叠成源站，并拒绝带凭据、查询串或片段
   的 URL。
3. JMAP account id 是可选的。留空：首次保存成功后后端会从服务器读回，页面会显示出来。

## 1.5 跑起后端

容器只需要两个环境变量：

```bash
# .env — 不要提交这个文件
REDIS_URL=rediss://default:<password>@<host>:6379
CONFIG_ENCRYPTION_KEY=<64 位十六进制，用：openssl rand -hex 32>
```

- `REDIS_URL` 是你唯一的状态存储。像数据库一样备份它。
- `CONFIG_ENCRYPTION_KEY` 兼作 SPA 管理员凭据与已存业务密钥的加密密钥。丢了它，你既登不上配置页，
  也解不开已存密钥；轮换它，之前存的密钥全部不可读。

然后构建并启动：

```bash
docker build -t messageweave .
docker run --rm --env-file .env -p 8080:8080 messageweave
curl localhost:8080/healthz   # → ok
```

`GET /healthz` 是静态存活回复，不碰 Redis。就绪报告是 `GET /ready`：在配置存在且所有已配置上游可达
之前回答 503，之后回答 200 并带一份 JSON 组件报告。现在把你的 HTTPS 反代或隧道指向这个端口——第 8、
9 步需要一个公网 HTTPS 域名。

## 1.6 登录配置页

1. 在浏览器打开页面。如果后端带着所需变量启动，第一张卡是 **Create admin session**。
2. 粘贴 `CONFIG_ENCRYPTION_KEY`——不是 Redis 密码。这会签发一个管理员会话，只活在页面内存里，有效期
   30 分钟；刷新页面即登出。

## 1.7 填写业务配置

表单是整存整换，不是增量补丁：你提交的就是整份存储配置。各节：

- **01 JMAP mailbox**——session URL、用户名，以及第 4 步的应用密码。
- **02 Telegram and access auth**——bot token、通知 chat id、白名单，加上你自己生成的三个密钥：webhook
  密钥、reconcile token 与 worker token。各自用 `openssl rand -hex 32` 生成。webhook 密钥之后要在
  Telegram 侧对得上；reconcile 与 worker token 保护调度端点。
- **03 LLM service**——可选。不需要 AI 摘要就别开；一旦开启，HTTPS base URL、模型名与 API key 三者都
  变成必填。
- **04 Notification display**——通知里收信时间的渲染时区。

然后按顺序：

1. 点 **Test connection**。它预检凭据但不保存任何东西。
2. 点 **Save business config and hot-reload**。首次保存必须包含所有必填字段，否则服务器回答 422 且
   什么也不存。
3. 读按钮下方那行结果：`runtime_applied: true` 表示新运行时已在本后端装配；`false` 表示存了但运行时未
   装配，`warnings` 数组会指出失败的组件——最常见是 `jmap_session_url` 不可达。改字段再存一次；`/ready`
   会一直 503 直到它变绿。
4. 如果保存回答 **409 conflict**，说明有人（另一个标签页、第二个运维者）在中间也存了。刷新页面拿到
   他们的值，再把你的改动重新套上去。

密钥是只写的：页面永不再显示已存密钥，提交空的密钥字段会保留之前的值——要清密钥就在单独一次编辑里
故意留空。

## 1.8 注册回调

仍在同一页，**External callbacks** 区：

1. 粘贴能到达本后端的公网 HTTPS 源站，只写 scheme + host，不带路径——例如 `https://mw.example.com`。
   如果你把 Cloudflare 网关放在前面，就填网关的源站，不是后端的：回调要能在后端重启后仍然可达，而
   网关才是那个不掉线的。
2. 点 **Register callbacks**。一次点击注册双向：Telegram webhook 注册到 `/webhook/tg`，Stalwart push
   订阅注册到 `/push/jmap`。验证自动完成，页面上报结果。

注册是显式的、可重复的。注册新源站会取消上一个订阅，于是旧源站不再收到 push。失败的注册绝不会删掉
旧订阅——如果连不上 Telegram，上一个 webhook 原样留着；如果连上了 Telegram 但没连上 Stalwart，页面会
报部分失败并让你再点一次。

在终端验证：

```bash
curl -s "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
# 期望 "url": "https://mw.example.com/webhook/tg" 且 "pending_update_count": 0
```

## 1.9 打开处理开关

把页面顶部的 **Enable business processing** 拨开。这是总开关：关着时 webhook 照收，但 `/worker` 与
`/reconcile` 拒绝排空，所以什么也到不了 Telegram。打开后，入站邮件与消息才会进队列、由 worker 排空。

## 1.10 端到端验证

1. 给自己发一封邮件。几秒内 bot 应当贴出一条通知，含发件人、主题与收信时间——绝不包含正文。这一条
   路径同时证明 JMAP 入站、Redis、队列与 Telegram 发送全通。
2. 在 Telegram 里试 `/search 发票`。注意：搜索触发词是中文 `搜索`、`查找`、`检索`，外加 `/search`
   命令——`search invoice` 这类英文关键词不会被识别为触发。
3. 从通知里的 email id 试 `/summary <email_id>`。首次调用会请求确认；只有你确认之后、且 LLM 节开启时，
   摘要才会发出。

## 1.11 排程排空

Push 投递可能延迟或丢失；外部调度器是兜底。在任意一台能访问你源站的机器上，每 5–10 分钟跑一次：

```bash
MW_APP_URL=https://mw.example.com \
MW_WORKER_TOKEN=<worker token> \
MW_RECONCILE_TOKEN=<reconcile token> \
scripts/cron-drain.sh
```

脚本会排空队列（`/worker`）并重扫漏掉的 push（`/reconcile`）；两者成功都回答 204 空 body。如果你把网关放
在前面，`/worker` 必须直达后端源站——原因见 deployment.md。

## 1.12 可选：Cloudflare 网关

除非你想跨两个后端做负载均衡，否则跳过。Worker 是可选的，给免费层 Cloudflare 用户提供一组负载均衡器，
不是安全层：无论用不用它，后端源站都保持可直接访问。安装见 `cloudflare-worker/README.md`；用时把第 8 步
的回调源站对准网关。

## 1.13 出问题时

- **保存后什么也没到。** 查处理开关（第 9 步），再查 `getWebhookInfo`（第 8 步）。`last_error_message`
  里提到 401，说明 webhook 密钥与 Telegram 发来的不一致——重存 02 节并重新注册。
- **`/ready` 回 503。** 503 的 body 只是一个错误码，不会指名是哪个组件挂了。`/ready` 回 200 时，JSON
  里有逐组件布尔值（`jmap`、`telegram`、`redis`）。常见原因：`jmap_session_url` 不可达、应用密码错、或
  上次保存 `runtime_applied: false`。
- **表单提示冲突。** 两个会话同时存了；刷新重做（第 7.3 步）。
- **bot 在群里不理你。** 隐私模式开着；去 `@BotFather` 关掉（第 3 步）。
- **`/search` 回的是帮助提示。** 你的查询以拉丁词开头；触发词是 `搜索` / `查找` / `检索`。

字段级错误、TTL、Redis 键与路由表：[reference.md](reference.md)。
扩展、密钥轮换、网关与 cron 在线上的细节：[deployment.md](deployment.md)。
