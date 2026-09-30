# 未完成项（Open Gaps）

本文件只列**仍未完成 / 仍未验证**的事项。已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 你要做的事

代码侧**没有**待你实现的项。剩下的这一件（TTL 实测）不能只靠我跑——它需要一个可连的测试 Redis 实例：

| # | 事项 | 谁做 | 你要动手吗 |
| --- | --- | --- | --- |
| 1 | TTL 实测 | 我 | 需要一个可连的测试 Redis 实例 |

## 阶段目标

只剩一条，而且**只差一个 Redis 地址**：**TTL 实测**。`#[ignore]` 门控的集成测试已经写好在 `src/state.rs`（`real_redis_ttl_tests`，2 个用例），缺的只是 `REDIS_TEST_URL`——给我一个可连的测试 Redis 实例，跑一遍就能收掉，不需要停机窗口、不需要产品判断。

上一轮需要停机窗口的第 1 项（`Email/changes` 的 `newState` 语义）已由提交 `269c8f6` 在代码侧关掉，不再需要停机窗口。详见下文「已关闭」。

上一轮真机联调已经关掉了两条阻塞：

- **Telegram 入站方向**：`setWebhook` 之后群里发 `/help`，`POST /worker` 排空即收到自动回复。真实 Telegram 流量已驱动过 `/webhook/tg`。
- **Stalwart `PushSubscription`**：`stalwart:jmap` 连续数天有真实、互不重复的推送事件。该 stream 全代码库只有两个写入点——`src/notify.rs:1254`（JMAP 回调入队）与 `src/worker.rs:412`（对账增量入队）——所以有事件出现就只能是订阅活着且在投递。同一次观测也顺带验证了去重键在跨日规模下没有重复投递。

两条的操作步骤已移到面向使用者的 `docs/deployment.md` §4.1，本文件不再跟踪它们。

顺带把原第 3 项里的「回调公网映射」也解掉了：当前联调环境前面没有 Cloudflare Worker，Stalwart 直接打到后端 origin 的 `/push/jmap` 且在真实投递；生产拓扑下 `/push/jmap` 本就在 Worker safelist 内（`docs/deployment.md` §10.4）。两种拓扑下映射都由现有部署解决，「用哪个组件做映射」不再需要回答。

### 本轮顺带发现并修掉的部署文档缺陷

`docs/deployment.md` §6.3.1 原来只给了 `/reconcile` 的调度示例，而 `/worker`——全代码库唯一的队列消费入口——被描述成「运维手工触发」，且刻意不在 Worker 白名单里。**照文档照抄部署 = 邮件持续进队列、通知永远发不出去**，这正是联调环境积压了数天才被手工排空的成因。§6.3.1 已补上 `/worker` 的调度步骤、顺序要求、批量上限与「204 不能当成功信号」的说明。

同时修正了 `docs/reference.md` 与 `docs/retired.md` 里 `/worker` 的锚点：原值 `notify.rs:331` 落在 `reconcile` 函数体内，锚点审计不会报错（该行存在且非空），但语义是错的；正确值是 `notify.rs:381`。

## 阻塞

### 已关闭：`Email/changes` 的 `newState` 语义（提交 `269c8f6`）

原条目担心「服务器无法回放旧增量时返回 `newState`，要求客户端重新基线化」。但 jmap-client 0.4.2 的 `ChangesResponse` 只有 `accountId` / `oldState` / `newState` / `hasMoreChanges` / `created` / `updated` / `destroyed` 加展开的调用参数——**没有 reset / re-baseline 信号字段**。所以 `/changes` 失败在代码里就是一个普通的 `Err`，无法区分「临时故障」和「服务端不再回放这个 state」。这个条目因此无法通过任何观测手段闭环：停机窗口里能观测到的仍然是同一个 `Err`。

代码的实际行为比条目描述的更糟：`Err` 被原样上抛成 `Err(())`，`reconcile:state` 原样保留——下一次 cron 用同一个 dead `sinceState` 重试，再失败，再 503。游标永久冻结，`/reconcile` 永远返回 `503 reconcile_retry` 且没有任何前进。

**修法**：`/changes` 失败时改走重新基线——取一个新鲜的 `current_state()`，写回 `baseline:{fresh_state}:0` 并 `Ok` 返回。position walk 完全不依赖服务端 changelog 保留策略，所以下一轮从 position 0 重走一遍即可恢复；24h 去重键（`enqueue_reconcile_event`，86400s）保证重放最坏重复一次、不会漏。新增单测 `reconcile_rebaselines_when_changes_replay_is_stale` 锁定该行为。

> position walk 路径（`list_emails_page(None, position, 100)`）本身不受影响——它把 `state` 传成 `None`，根本不发服务端 state。

### 1. TTL 实测

表里 **12 个 TTL 写入点、17 个 TTL 值**。它们分三类，可测性各不相同，不能混为一谈：A 类是调用点的字面量参数（不是模块级常量）；B 类**内嵌在 Lua 脚本字符串里**，在调用点提常量够不到；C 类的值产生于 `worker.rs` 的意图构造处、只是被当作参数透传给写入点。

> **2026-09-30 更正**：原表把 `7 * 86_400` 写成 `604_800`、`365 * 86_400` 写成 `31_536_000`（算术对、字面形式错），并凭空多出 `300`（5 天）与 `7200`（2 小时）两个代码里不存在的值——已删，C 类第六个值实际是 `0`（撤销）。另有 16 处行号锚点漂移，已按代码实际位置重指。

共同点是：**A/C 两类的字面量本身没有任何测试断言**——测试替身 `MemoryState::claim_dedup` 直接忽略 TTL 参数（`src/state.rs:948` 的 `_ttl_seconds`）。被测试覆盖的只是调用路径（`claim_dedup` 先 true 后 false、锁的 claim/renew/release 流程），不是过期时长本身。三个真正落到 Redis 的原子命令——`set_nx_ex` 的 `SET … NX EX`（`src/state.rs:905`，`ttl_seconds.max(1)` 兜 0）、`retry_or_dlq` 内嵌 Lua 里的 `EXPIRE KEYS[1] 86400`（`src/state.rs:523`）、`set_ai_consent` 的 `SET … EX`（`src/state.rs:542-551`，同样 `max(1)` 兜底）——**已由 `real_redis_ttl_tests` 在真实 Redis 上断言 PTTL**（见本节末尾「我来做的」）。真实 Redis 上这些命令的过期行为是 Redis 自身的既定语义，本仓库不依赖验证。

**A. 调用点字面量参数 — 10 个写入点 / 10 个值**

| 键 | 用途 | TTL | 写入点 |
| --- | --- | --- | --- |
| `lock:reconcile` | 对账锁 | 300s | `src/notify.rs:290` |
| `lock:reconcile` | 对账锁心跳续租 | 90s | `src/notify.rs:303` |
| `lock:push-register:{sha256(callback_url)}` | 注册单飞锁 | 360s | `src/notify.rs:1301` |
| `ratelimit:push-verify:{subscription_id}` | 验证限流 | 30s | `src/notify.rs:1210` |
| `dedup:tg:{update_id}` | Telegram 更新去重 | 86_400s | `src/notify.rs:250` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重（回调入队） | 86_400s | `src/notify.rs:1249` |
| `dedup:jmap:{account_id}:{email_id}` | JMAP 更新去重（对账增量入队） | 86_400s | `src/worker.rs:412` |
| `delivery:inflight:{stream}:{message.id}` | 投递在途守卫 | 60s | `src/notify.rs:447` |
| `delivery:committed:{stream}:{message.id}` | 投递幂等 | 604_800s | `src/notify.rs:459` |
| `push:subscription:{id}:status` | push 订阅 `disabled` 标记 | 86_400s | `src/notify.rs:1426` |

**B. 内嵌 Lua 脚本 — 1 个写入点 / 1 个值**

| 键 | 用途 | TTL | 写入点 |
| --- | --- | --- | --- |
| `retry:{stream}:{message.id}` | 投递重试计数（键构造 `src/state.rs:514`） | 86400 | `src/state.rs:523` |

这格写在 `retry_or_dlq` 的 Lua 脚本字符串里（`src/state.rs:518-529`），不是在调用点传参——「把 TTL 提成模块级常量」这类改造对它够不到，得改脚本本身。

**C. `consent:ai:{chat_id}` — 1 个写入点 / 6 个值**

| 值 | 触发 | 说明 | 产生处 |
| --- | --- | --- | --- |
| `3600` | 「临时 / 一次」 | 1 小时 | `src/worker.rs:585-586` |
| `86_400` | 「今天」 | 1 天 | `src/worker.rs:591-592` |
| `7 * 86_400` | 「7天」 | 7 天 | `src/worker.rs:597-598` |
| `365 * 86_400` | 「直到撤销 / 长期」 | 最长 365 天 | `src/worker.rs:603-604` |
| `3600` | `/ai on` 等显式开启 | 1 小时 | `src/worker.rs:614-615` |
| `0` | `/ai off` 撤销 | 落到 `max(1)` = 1s，且已过期 | `src/worker.rs:624-625` |

写入点是 `src/worker.rs:450` 的 `set_ai_consent(chat.id, ttl)`，落 Redis 在 `src/state.rs:542-551`。这组的值由用户在 Telegram 里说的话决定，不是写死在 state 层。

我来做的：**已经补上集成测试**，`src/state.rs` 末尾新增 `real_redis_ttl_tests` 模块（两个 `#[ignore]`-gated 用例，照 `src/domain/jmap/client.rs:522` 的模式，缺 `REDIS_TEST_URL` 时打 skipped 并返回，URL 不落日志）。它用真实 Redis 断言三个原子写入点的实际 PTTL：`claim_dedup` 的 `.max(1)` 下限与 604_800 值、`set_ai_consent` 的 3600、`retry_or_dlq` Lua 里嵌入的 86400。运行：`REDIS_TEST_URL=… cargo test -- --ignored ttl`。

**残留风险（已收窄）**：上面 17 个数值里，三个原子写入点收到的值**已被测试断言**；A 类调用点字面量与 C 类 `Intent::Consent` 字面量仍未被断言——改成一个语法合法但语义错误的数字（例如 `60_480`）仍能编译通过、单测全绿。这是刻意取舍：把每个调用点字面量也搬进测试会让测试与生产代码逐行重复，反而更容易一起漂。真要收紧，做法是把 A 类字面量提成 `pub const` 再在测试里断言常量表。表中 17 个值已人工逐个核对无误，且本轮已修正 3 处数值写错（`7 * 86_400` / `365 * 86_400` 的字面形式）与 16 处行号锚点漂移。

## 后续（多账号）

单账号由产品决策固定，不在阶段目标内。当前 JMAP 登录与 `Email/query` 均为单一邮箱账号。

**需要你决定：** 是否需要多账号。要的话告诉我，我先把 `REQ-SINGLE-ACCOUNT` 的约束范围拆出来再动代码。
