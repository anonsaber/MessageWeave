# 未完成项（Open Gaps）

本文件只列**仍未完成 / 仍未验证**的事项。已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 你要做的事

代码侧**没有**待你实现的项。剩下的一件事需要你腾一个停机窗口；第二件（TTL 实测）不能只靠我跑——它需要一个可连的测试 Redis 实例：

| # | 事项 | 谁做 | 你要动手吗 |
| --- | --- | --- | --- |
| 1 | `Email/changes` 的 `newState` 语义 | 你 + 我 | 腾一次停机窗口 |
| 2 | TTL 实测 | 我 | 需要一个可连的测试 Redis 实例 |

## 阶段目标

剩下一条需要停机窗口的阻塞：**在真实环境里确认 `Email/changes` 的 `newState` 语义**；另一条 TTL 实测（第 2 项）也不能靠「我补测」收掉——它需要一个可连的测试 Redis 实例才能跑起 `#[ignore]` 门控的集成测试，在那之前只有人工核对。

上一轮真机联调已经关掉了两条阻塞：

- **Telegram 入站方向**：`setWebhook` 之后群里发 `/help`，`POST /worker` 排空即收到自动回复。真实 Telegram 流量已驱动过 `/webhook/tg`。
- **Stalwart `PushSubscription`**：`stalwart:jmap` 连续数天有真实、互不重复的推送事件。该 stream 全代码库只有两个写入点——`src/notify.rs:1236`（JMAP 回调入队）与 `src/worker.rs:394`（对账增量入队）——所以有事件出现就只能是订阅活着且在投递。同一次观测也顺带验证了去重键在跨日规模下没有重复投递。

两条的操作步骤已移到面向使用者的 `docs/deployment.md` §4.1，本文件不再跟踪它们。

顺带把原第 3 项里的「回调公网映射」也解掉了：当前联调环境前面没有 Cloudflare Worker，Stalwart 直接打到后端 origin 的 `/push/jmap` 且在真实投递；生产拓扑下 `/push/jmap` 本就在 Worker safelist 内（`docs/deployment.md` §10.4）。两种拓扑下映射都由现有部署解决，「用哪个组件做映射」不再需要回答。

### 本轮顺带发现并修掉的部署文档缺陷

`docs/deployment.md` §6.3.1 原来只给了 `/reconcile` 的调度示例，而 `/worker`——全代码库唯一的队列消费入口——被描述成「运维手工触发」，且刻意不在 Worker 白名单里。**照文档照抄部署 = 邮件持续进队列、通知永远发不出去**，这正是联调环境积压了数天才被手工排空的成因。§6.3.1 已补上 `/worker` 的调度步骤、顺序要求、批量上限与「204 不能当成功信号」的说明。

同时修正了 `docs/reference.md` 与 `docs/retired.md` 里 `/worker` 的锚点：原值 `notify.rs:331` 落在 `reconcile` 函数体内，锚点审计不会报错（该行存在且非空），但语义是错的；正确值是 `notify.rs:381`。

## 阻塞

### 1. `Email/changes` 的 `newState` 语义

`jmap-client` 的 `fetchChanges` 在服务器无法回放旧增量时会返回 `newState`，要求客户端重新基线化。本代码库的 `since` 游标处理只在测试服务器上跑过，**尚未出现一次服务器要求重新基线化的真实场景**。

**你要做的：** 腾一次停机窗口——在邮箱还有新邮件进入的时候把本服务停掉，停到 Stalwart 的增量历史过期为止（保留时长由你的 Stalwart 配置决定；窗口要长于它）。停机期间**不要**跑 `POST /reconcile`。

我来做的：恢复后跑 `POST /reconcile`，再看 `/debug/worker` 的 `reconcile_cursor`。预期是重新变成一个**新的 `baseline:` 游标**（重新基线化成功），而不是报错或停在旧游标。实测语义回填本节。

### 2. TTL 实测

表里 **12 个 TTL 写入点、17 个 TTL 值**，值已人工逐个核对无误。它们分三类，可测性各不相同，不能混为一谈：A 类是调用点的字面量参数（不是模块级常量）；B 类**内嵌在 Lua 脚本字符串里**，在调用点提常量够不到；C 类的值产生于 `worker.rs` 的意图构造处、只是被当作参数透传给写入点。

共同点是：**仓库里没有任何测试断言过任何一个 TTL 数值**——测试替身 `MemoryState::claim_dedup` 直接忽略 TTL 参数（`src/state.rs:911` 的 `_ttl_seconds`），全仓 `PTTL` / `.ttl()` 断言数为 0，也没有 Redis 测试服务器 gating（只有 JMAP 有 `#[ignore]` 门控，见 `src/domain/jmap/client.rs:522`）。被测试覆盖的只是调用路径（`claim_dedup` 先 true 后 false、锁的 claim/renew/release 流程），不是过期时长本身。真实落到 Redis 的是三处原子命令——`set_nx_ex` 的 `SET … NX EX`（`src/state.rs:856`，`ttl_seconds.max(1)` 兜 0）、`retry_or_dlq` 内嵌 Lua 里的 `EXPIRE KEYS[1] 86400`（`src/state.rs:486`）、`set_ai_consent` 的 `SET … EX`（`src/state.rs:512-513`，同样 `max(1)` 兜底）；真实 Redis 上这些命令的过期行为是 Redis 自身的既定语义，本仓库无法验证，也不依赖验证。

**A. 调用点字面量参数 — 10 个写入点 / 10 个值**

| 键 | 用途 | TTL | 写入点 |
| --- | --- | --- | --- |
| `lock:reconcile` | 对账锁 | 300s | `src/notify.rs:290` |
| `lock:reconcile` | 对账锁心跳续租 | 90s | `src/notify.rs:303` |
| `lock:push-register:{sha256(callback_url)}` | 注册单飞锁 | 360s | `src/notify.rs:1283` |
| `ratelimit:push-verify:{subscription_id}` | 验证限流 | 30s | `src/notify.rs:1192` |
| `dedup:tg:{update_id}` | Telegram 更新去重 | 86_400s | `src/notify.rs:250` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重（回调入队） | 86_400s | `src/notify.rs:1231` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重（对账增量入队） | 86_400s | `src/worker.rs:394` |
| `delivery:inflight:{stream}:{message.id}` | 投递在途守卫 | 60s | `src/notify.rs:447` |
| `delivery:committed:{stream}:{message.id}` | 投递幂等 | 604_800s | `src/notify.rs:459` |
| `push:subscription:{id}:status` | push 订阅 `disabled` 标记 | 86_400s | `src/notify.rs:1408` |

**B. 内嵌 Lua 脚本 — 1 个写入点 / 1 个值**

| 键 | 用途 | TTL | 写入点 |
| --- | --- | --- | --- |
| `retry:{stream}:{message.id}` | 投递重试计数（键构造 `src/state.rs:481`） | 86400 | `src/state.rs:486` |

这格写在 `retry_or_dlq` 的 Lua 脚本字符串里（`src/state.rs:484-490`），不是在调用点传参——「把 TTL 提成模块级常量」这类改造对它够不到，得改脚本本身。

**C. `consent:ai:{chat_id}` — 1 个写入点 / 6 个值**

| 值 | 触发 | 说明 | 产生处 |
| --- | --- | --- | --- |
| `3600` | 「临时 / 一次」 | 1 小时 | `src/worker.rs:568` |
| `86_400` | 「今天」 | 1 天 | `src/worker.rs:574` |
| `7 * 86_400` | 「7天」 | 7 天 | `src/worker.rs:580` |
| `365 * 86_400` | 「直到撤销 / 长期」 | 最长 365 天 | `src/worker.rs:586` |
| `3600` | `/ai on` 等显式开启 | 1 小时 | `src/worker.rs:597` |
| `0` | `/ai off` 撤销 | 落到 `max(1)` = 1s，且已过期 | `src/worker.rs:607` |

写入点是 `src/worker.rs:432` 的 `set_ai_consent(chat.id, ttl)`，落 Redis 在 `src/state.rs:505-516`。这组的值由用户在 Telegram 里说的话决定，不是写死在 state 层。

我来做的：**触发一次真实回调读不到 Redis 键的 TTL**——`/debug/*` 六条只读端点里没有任何一条暴露键的 TTL（全仓 `PTTL` 断言数也为 0），所以光触发回调记不到实测值。要真正走通，需要在仓库里新增一个 `#[ignore]`-gated 的 Redis 集成测试（照 `src/domain/jmap/client.rs:522` 的模式），并配置一个真实的测试 Redis URL 才能真正执行。在那之前本节保持阻塞，但残留的风险只有「TTL 数值没被测试断言」，且表中 17 个值已人工逐个核对无误。

## 后续（多账号）

单账号由产品决策固定，不在阶段目标内。当前 JMAP 登录与 `Email/query` 均为单一邮箱账号。

**需要你决定：** 是否需要多账号。要的话告诉我，我先把 `REQ-SINGLE-ACCOUNT` 的约束范围拆出来再动代码。
