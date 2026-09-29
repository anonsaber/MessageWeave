# 未完成项（Open Gaps）

本文件只列**仍未完成 / 仍未验证**的事项。已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 你要做的事

代码侧**没有**待你实现的项。剩下的一件事需要你腾一个停机窗口，另一件由我跑：

| # | 事项 | 谁做 | 你要动手吗 |
| --- | --- | --- | --- |
| 1 | `Email/changes` 的 `newState` 语义 | 你 + 我 | 腾一次停机窗口 |
| 2 | 单飞锁 TTL 实测 | 我 | 不需要，我自己跑 |

## 阶段目标

剩下一条需要停机窗口的阻塞：**在真实环境里确认 `Email/changes` 的 `newState` 语义**；另有一条由我补测的 TTL 实测（第 2 项），不依赖你。

上一轮真机联调已经关掉了两条阻塞：

- **Telegram 入站方向**：`setWebhook` 之后群里发 `/help`，`POST /worker` 排空即收到自动回复。此前 `/webhook/tg` 收到的全部是伪造 update，Telegram 侧从未收到过 `setWebhook`，所以这个方向一直没有被真实流量验证过。
- **Stalwart `PushSubscription`**：`stalwart:jmap` 连续数天有真实、互不重复的推送事件。该 stream 全代码库只有两个写入点——`src/notify.rs:1140`（JMAP 回调入队）与 `src/worker.rs:394`（对账增量入队）——所以有事件出现就只能是订阅活着且在投递。同一次观测也顺带验证了去重键在跨日规模下没有重复投递。

两条的操作步骤已移到面向使用者的 `docs/deployment.md` §4.1，本文件不再跟踪它们。

顺带把原第 3 项里的「回调公网映射」也解掉了：当前联调环境前面没有 Cloudflare Worker，Stalwart 直接打到后端 origin 的 `/push/jmap` 且在真实投递；生产拓扑下 `/push/jmap` 本就在 Worker safelist 内（`docs/deployment.md` §10.4）。两种拓扑下映射都由现有部署解决，「用哪个组件做映射」不再需要回答。

### 本轮顺带发现并修掉的部署文档缺陷

`docs/deployment.md` §6.3.1 原来只给了 `/reconcile` 的调度示例，而 `/worker`——全代码库唯一的队列消费入口——被描述成「运维手工触发」，且刻意不在 Worker 白名单里。**照文档照抄部署 = 邮件持续进队列、通知永远发不出去**，这正是联调环境积压了数天才被手工排空的成因。§6.3.1 已补上 `/worker` 的调度步骤、顺序要求、批量上限与「204 不能当成功信号」的说明。

同时修正了 `docs/reference.md` 与 `docs/retired.md` 里 `/worker` 的锚点：原值 `notify.rs:331` 落在 `reconcile` 函数体内，锚点审计不会报错（该行存在且非空），但语义是错的；正确值是 `notify.rs:380`。

## 阻塞

### 1. `Email/changes` 的 `newState` 语义

`jmap-client` 的 `fetchChanges` 在服务器无法回放旧增量时会返回 `newState`，要求客户端重新基线化。本代码库的 `since` 游标处理只在测试服务器上跑过，**尚未出现一次服务器要求重新基线化的真实场景**。

**你要做的：** 腾一次停机窗口——在邮箱还有新邮件进入的时候把本服务停掉，停到 Stalwart 的增量历史过期为止（保留时长由你的 Stalwart 配置决定；窗口要长于它）。停机期间**不要**跑 `POST /reconcile`。

我来做的：恢复后跑 `POST /reconcile`，再看 `/debug/worker` 的 `reconcile_cursor`。预期是重新变成一个**新的 `baseline:` 游标**（重新基线化成功），而不是报错或停在旧游标。实测语义回填本节。

### 2. 单飞锁 TTL 实测

TTL 参数已在代码里写死、单测也已覆盖，但真实场景下的实际过期行为从未观测过。前置依赖（订阅注册成功、真实回调在投递）已解除，这一项不再依赖任何人的配合。

| 键 | 用途 | TTL | 位置 |
| --- | --- | --- | --- |
| `lock:push-register:{sha256(callback_url)}` | 注册单飞锁 | 360s | `src/notify.rs:1178` |
| `lock:reconcile` | 对账锁（长对账中每 90s 续租） | 300s | `src/notify.rs:289` / `:302` |
| `ratelimit:push-verify:{subscription_id}` | 验证限流 | 30s | `src/notify.rs:1096` |
| `dedup:tg:{update_id}` | Telegram 更新去重 | 86400s | `src/notify.rs:248` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重 | 86400s | `src/notify.rs:1135` |

我来做的：触发一次真实回调并记录这五个键的实际过期行为，把实测值回填本节。在这之前本节保持阻塞。

## 后续（多账号）

单账号由产品决策固定，不在阶段目标内。当前 JMAP 登录与 `Email/query` 均为单一邮箱账号。

**需要你决定：** 是否需要多账号。要的话告诉我，我先把 `REQ-SINGLE-ACCOUNT` 的约束范围拆出来再动代码。
