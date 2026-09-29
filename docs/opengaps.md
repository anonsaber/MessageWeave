# 未完成项（Open Gaps）

本文件只列**仍未完成 / 仍未验证**的事项。已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 你要做的事

代码侧**没有**待你实现的项。四项阻塞里，两项需要你跑一条 curl，一项只需要你告诉我一个事实，一项需要你腾一个停机窗口。

| # | 事项 | 谁做 | 你要动手吗 |
| --- | --- | --- | --- |
| 1 | Telegram `setWebhook` | 你 | 跑 1 条 curl，然后发一条消息 |
| 2 | Stalwart `PushSubscription` | 你 | 跑 1 条 curl（注册由本服务自己向 JMAP 发起） |
| 3 | 回调公网映射 | 你 | 只回答一个问题：用哪个组件做映射 |
| 4 | `Email/changes` 的 `newState` 语义 | 你 + 我 | 腾一次停机窗口 |
| 5 | 单飞锁 TTL 实测 | 我 | 不需要的，依赖 #2 成功后我自己跑 |

## 阶段目标

剩余目标收窄为：**在 Telegram 侧执行 `setWebhook`**，以及在 Stalwart 侧走通 `PushSubscription` 回调链路，消掉下面 4 条阻塞项。出站查询方向（JMAP 只读 → Telegram 回帖）、`/search`、出站限流退避、多实例去重均已完成并真机通过。

## 阻塞

### 1. 真实 Telegram 回调（Telegram → 本服务方向）

出站方向已验证，但**Telegram → 本服务从未被真实流量验证过**：到目前为止 `/webhook/tg` 收到的全部是伪造 update。Telegram 侧还从未收到过 `setWebhook`，所以 Telegram 既不知道回调地址，手里也没有 secret。

**你要做的：**

1. 登记 webhook（`<SECRET>` = 业务配置里的 `telegram_webhook_secret`）：
   ```bash
   curl -sS "https://api.telegram.org/bot<BOT_TOKEN>/setWebhook" \
     -d url=https://messageweave-eu-1.motofans.club/webhook/tg \
     -d secret_token=<SECRET> \
     -d 'allowed_updates=["message"]'
   ```
   不传 `allowed_updates` 时默认订阅全部更新类型；显式收窄到 `message` 与本服务只解析 `message.text` 一致。
2. 确认已登记：
   ```bash
   curl -sS "https://api.telegram.org/bot<BOT_TOKEN>/getWebhookInfo"
   ```
   看 `result.url` 等于上面那个地址。**Telegram 不回显 `secret_token`**，所以 secret 写对没有只能靠第 3 步反过来验。
3. 用允许名单里的 chat id 发一条 `/help`，然后告诉我「发了」。

我来收尾：`POST /api/worker` 排空队列并确认回帖。如果没收到，`getWebhookInfo` 里的 `last_error_message` 与 `last_error_date` 是最快的定位线索。

**轮换注意：** 在 SPA 改了 `telegram_webhook_secret` 而**没有**重跑 `setWebhook`，Telegram 手里就是旧值——之后所有真实回调都会被 401 拒掉，**不报错、无告警，只是静默收不到消息**。改了就重跑第 1 步。

### 2. 真实 Stalwart `PushSubscription`

`PushSubscription/set` 的验证往返只在本地单测里跑过（测试指向一个永不连通的地址）；生产环境从未注册过订阅，所以 Stalwart 从未发过一次真实 push 回调，`/push/jmap` 的验证握手分支从未被真实请求走过。

**先说清分工：** 注册动作由**本服务自己**向 JMAP 发起，不是在 Stalwart 后台点。调用即完成 `PushSubscription/set` create 并限制订阅类型；**验证码由 Stalwart 生成、经回调送回来，本服务自动完成验证，不接受 SPA 里配置的验证码**（`src/notify.rs:1163`）。所以你不需要去拿验证码，你只需要让回调地址能被 Stalwart 访问到。

**你要做的：**

1. 确认回调地址公网可达，且 Stalwart 出站能访问到它（防火墙/代理放行）。是 Stalwart **回调**你，不是你去连它。
2. 注册：
   ```bash
   curl -sS -X POST "https://messageweave-eu-1.motofans.club/api/push/register" \
     -H "Authorization: Bearer <WORKER_TOKEN>" \
     -H "content-type: application/json" \
     -d '{"callback_url":"https://messageweave-eu-1.motofans.club/push/jmap"}'
   ```
   成功 → `200 {"push_subscription_id":"..."}`，状态置 `pending`（TTL 900s，`src/notify.rs:1240`），在等 Stalwart 回来验证。
3. Stalwart 的验证请求到达后，本服务自动验证并置 `verified`（TTL 300s，`src/notify.rs:1125`）。日志在 stdout（`SAF-LOG-STDOUT-ONLY`）。
4. 确认已落库：再跑一次第 2 步，返回里多出 `"idempotent":true` 即说明订阅已持久化。也可反向确认——`POST /api/push/disable` 在从未注册过该地址时回 `404 push_subscription_not_found`。

**失败怎么读：** `400 invalid_request` = 回调地址不是 HTTPS，或 URL 里带了用户名/密码（本服务禁止这两者，`src/notify.rs:1174`）；`409 conflict` = 注册单飞锁还被上一次请求占着，稍后重试；`503` = 业务配置此刻不可用或 JMAP 请求失败，锁已释放，可重试。

**取消注册：** 带 `{"callback_url":"..."}` 打 `POST /api/push/disable`。

### 3. Push callback 公网映射 + 单飞锁 TTL

**这部分你不需要跑命令，只需要回答一个问题：公网映射由哪个组件负责。** 当前这个测试部署前面没有 Cloudflare Worker，我不知道该在哪里放行 `/push/jmap`。候选是 `cloudflare-worker/` 里已有的实现（从未部署）、nginx，或其他 ingress。

TTL 参数已在代码里写死、单测也已覆盖，但真实场景下的实际过期行为从未观测过：

| 键 | 用途 | TTL | 位置 |
| --- | --- | --- | --- |
| `lock:push-register:{sha256(callback_url)}` | 注册单飞锁 | 360s | `src/notify.rs:1178` |
| `lock:reconcile` | 对账锁（长对账中每 90s 续租） | 300s | `src/notify.rs:289` / `:302` |
| `ratelimit:push-verify:{subscription_id}` | 验证限流 | 30s | `src/notify.rs:1095` |
| `dedup:tg:{update_id}` | Telegram 更新去重 | 86400s | `src/notify.rs:248` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重 | 86400s | `src/notify.rs:1134` |

我来做的：订阅注册成功（第 2 项）之后，触发一次真实回调并记录这五个键的实际过期行为，把实测值回填本节。在这之前本节保持阻塞。

### 4. `Email/changes` 的 `newState` 语义

`jmap-client` 的 `fetchChanges` 在服务器无法回放旧增量时会返回 `newState`，要求客户端重新基线化。本代码库的 `since` 游标处理只在测试服务器上跑过，**尚未出现一次服务器要求重新基线化的真实场景**。

**你要做的：** 腾一次停机窗口——在邮箱还有新邮件进入的时候把本服务停掉，停到 Stalwart 的增量历史过期为止（保留时长由你的 Stalwart 配置决定；窗口要长于它）。停机期间**不要**跑 `POST /reconcile`。

我来做的：恢复后跑 `POST /reconcile`，再看 `/debug/worker` 的 `reconcile_cursor`。预期是重新变成一个**新的 `baseline:` 游标**（重新基线化成功），而不是报错或停在旧游标。实测语义回填本节。

## 后续（多账号）

单账号由产品决策固定，不在阶段目标内。当前 JMAP 登录与 `Email/query` 均为单一邮箱账号。

**需要你决定：** 是否需要多账号。要的话告诉我，我先把 `REQ-SINGLE-ACCOUNT` 的约束范围拆出来再动代码。
