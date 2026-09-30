# 未完成项（Open Gaps）

本文件只列**仍未完成 / 仍未验证**的事项。已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 你要做的事

**两件，都是交东西不是做判断。**

1. 一个能用的 Upstash `rediss://` URL（当前持有的两个 token 都已 `WRONGPASS`），用来把 TTL 断言那条真机绿灯补上。
2. 多账号要不要做——产品决策，见文末「后续」。

## 阶段目标

| # | 项 | 状态 |
| --- | --- | --- |
| 1 | `Email/changes` 的 `newState` 语义 | 已关闭（提交 `269c8f6`） |
| 2 | TTL 实测 | 代码侧已关闭（提交 `0890eb1` + 本轮）；真机那条绿灯等一个可用的 `REDIS_TEST_URL` |
| 3 | Telegram 入站 / Stalwart `PushSubscription` 联调 | 已关闭（真实流量已驱动，操作步骤见 `docs/deployment.md` §4.1） |
| 4 | `/worker` 未在部署文档里 | 已关闭（`docs/deployment.md` §6.3.1 已补） |

门禁：`cargo fmt --check` / `cargo check --locked` / `cargo clippy --locked --all-targets -- -D warnings` 全绿；`cargo test --locked` **89 passed / 0 failed / 4 ignored**（忽略的 4 个是需要 `REDIS_TEST_URL` 或 `JMAP_TEST_URL` 的联调用例）；文档门 6/6 全绿，含 102 处行号锚点非空校验。

## 阻塞

**一个，不在代码里：TTL 的真机断言需要一个能认证通过的 `REDIS_TEST_URL`。** 当前持有的两个候选 token 都已被服务端拒绝（`-WRONGPASS invalid username-password pair`），网络与 TLS 握手本身正常。其余三条阻塞项均已关闭。

## 已关闭

### 1. TTL 实测（代码侧已关闭；真机断言待凭证）

原条目：全代码库 **12 个 TTL 写入点、17 个 TTL 值**，但只有三个真正落到 Redis 的原子命令（`set_nx_ex` 的 `SET … NX EX`、`retry_or_dlq` Lua 里的 `EXPIRE`、`set_ai_consent` 的 `SET … EX`）在真实 Redis 上被断言过实际 PTTL；其余全是裸字面量。测试替身 `MemoryState::claim_dedup` 直接忽略 TTL 参数，所以单测覆盖的是调用路径，不是过期时长——把一个 24 小时去重窗口改成 `60_480` 仍能编译通过、单测全绿。

两步关掉：

1. **真实 Redis 断言**（提交 `0890eb1`）。`src/state.rs` 的 `real_redis_ttl_tests`，两个 `#[ignore]`-gated 用例，照 `src/domain/jmap/client.rs:522` 的模式：缺 `REDIS_TEST_URL` 时打 skipped 并返回，URL 不落日志。断言三个原子写入点的实际 PTTL。运行：`REDIS_TEST_URL=… cargo test --locked -- --ignored ttl`。
2. **消灭裸字面量**（本轮）。17 个值全部收进 `src/state.rs` 的 `pub(crate) mod ttl`，调用点改引常量，新增 `ttl_contract_is_pinned` 逐个断言常量表，并带两条顺序断言（心跳必须短于锁、同意档位必须严格递增）。原条目里够不到的 B 类 Lua 内嵌值也接上了——`retry_or_dlq` 现在用 `ttl::RETRY_COUNTER_SECONDS` 插值生成脚本字符串，脚本里不再有第二个字面量。改值现在 = 改常量 + 断言失败，两处都得过评审。

契约基线（键的完整语义见 `docs/reference.md` §1）：

| 常量 | 值（秒） | 键 |
| --- | --- | --- |
| `ttl::RECONCILE_LOCK_SECONDS` | 300 | `lock:reconcile` |
| `ttl::RECONCILE_HEARTBEAT_SECONDS` | 90 | `lock:reconcile`（心跳续租） |
| `ttl::PUSH_REGISTER_LOCK_SECONDS` | 360 | `lock:push-register:{sha256(callback_url)}` |
| `ttl::PUSH_VERIFY_LIMIT_SECONDS` | 30 | `ratelimit:push-verify:{subscription_id}` |
| `ttl::DEDUP_TG_SECONDS` | 86_400 | `dedup:tg:{update_id}` |
| `ttl::DEDUP_JMAP_SECONDS` | 86_400 | `dedup:jmap:{account_id}:{email_id}`（回调入队与对账增量入队共用） |
| `ttl::DELIVERY_INFLIGHT_SECONDS` | 60 | `delivery:inflight:{stream}:{message.id}` |
| `ttl::DELIVERY_COMMITTED_SECONDS` | 604_800 | `delivery:committed:{stream}:{message.id}` |
| `ttl::PUSH_DISABLED_SECONDS` | 86_400 | `push:subscription:{id}:status`（`disabled`） |
| `ttl::PUSH_STATUS_PENDING_SECONDS` | 900 | `push:subscription:{id}:status`（`pending`） |
| `ttl::PUSH_STATUS_VERIFIED_SECONDS` | 300 | `push:subscription:{id}:status`（`verified`） |
| `ttl::PUSH_SUBSCRIPTION_SECONDS` | 300 | `push:subscription:{id}` / `push:subscription-code:{code}` |
| `ttl::PUSH_REGISTRATION_SECONDS` | 604_800 | `push:registration:{sha256(callback_url)}` |
| `ttl::PUSH_ORPHAN_SECONDS` | 604_800 | `push:orphan:{subscription_id}` |
| `ttl::ADMIN_SESSION_SECONDS` | 900 | `admin-session:{token}`（同时是 `/admin/session` 返回的 `expires_in`） |
| `ttl::RETRY_COUNTER_SECONDS` | 86_400 | `retry:{stream}:{message.id}` |
| `ttl::CONSENT_TEMPORARY_SECONDS` | 3_600 | `consent:ai:{chat_id}`（「临时 / 一次」与显式 `/ai on` 共用） |
| `ttl::CONSENT_TODAY_SECONDS` | 86_400 | `consent:ai:{chat_id}`（「今天」） |
| `ttl::CONSENT_WEEK_SECONDS` | 604_800 | `consent:ai:{chat_id}`（「7天」） |
| `ttl::CONSENT_MAXIMUM_SECONDS` | 31_536_000 | `consent:ai:{chat_id}`（「直到撤销 / 长期」） |
| `ttl::CONSENT_REVOKED_SECONDS` | 0 | `consent:ai:{chat_id}`（撤销；落 `max(1)` = 1s，语义是「已过期」） |

生产代码里已经不存在裸 TTL 字面量；`worker.rs` 的同意档位测试断言仍保留数字字面量，那是**刻意的**——用独立于常量的期望值去验生产代码，否则两边一起改的话单测永远绿。

本轮顺手修掉测试自身的三个 bug：

- 单测二进制不会跑 `main()`，rustls 没装 crypto provider，`rediss://` 握手直接 panic；现在测试模块自己调 `install_rustls_provider()`。
- `retry:{stream}:{message.id}` 的键构造用消息 id 而不是 run id，断言读到一个从未写入的键，PTTL 返回 `-2`，测试第一句就失败。
- `worker.rs` 对账增量入队那一处 TTL 字面量原先漏收，仍是裸 `86_400`。

**代码侧的缺口已经关完，但真机那条绿灯还差一把钥匙。** 当前持有的 Upstash token 两个候选都已返回 `WRONGPASS`（裸 RESP+TLS 探针确认：握手 TLSv1.3 正常，`AUTH default <token>` 被服务端拒绝），所以 `connect()` 直接失败，断言根本没机会跑。给我一个新的 `rediss://default:<token>@evolved-chicken-297696.upstash.io:6379`，一条命令就能把它补上。在拿到之前，这项的结论是「断言已就绪 + 常量已收口 + 单测已钉死」，不是「真机已验证」——这个区别不该含糊过去。

### 2. `Email/changes` 的 `newState` 语义（提交 `269c8f6`）

原条目担心「服务器无法回放旧增量时返回 `newState`，要求客户端重新基线化」。但 jmap-client 0.4.2 的 `ChangesResponse` 只有 `accountId` / `oldState` / `newState` / `hasMoreChanges` / `created` / `updated` / `destroyed` 加展开的调用参数——**没有 reset / re-baseline 信号字段**。所以 `/changes` 失败在代码里就是一个普通的 `Err`，无法区分「临时故障」和「服务端不再回放这个 state」。这个条目因此无法通过任何观测手段闭环：停机窗口里能观测到的仍然是同一个 `Err`。

代码的实际行为比条目描述的更糟：`Err` 被原样上抛成 `Err(())`，`reconcile:state` 原样保留——下一次 cron 用同一个 dead `sinceState` 重试，再失败，再 503。游标永久冻结，`/reconcile` 永远返回 `503 reconcile_retry` 且没有任何前进。

**修法**：`/changes` 失败时改走重新基线——取一个新鲜的 `current_state()`，写回 `baseline:{fresh_state}:0` 并 `Ok` 返回。position walk 完全不依赖服务端 changelog 保留策略，所以下一轮从 position 0 重走一遍即可恢复；24h 去重键（`enqueue_reconcile_event`，`ttl::DEDUP_JMAP_SECONDS`）保证重放最坏重复一次、不会漏。新增单测 `reconcile_rebaselines_when_changes_replay_is_stale` 锁定该行为。

> position walk 路径（`list_emails_page(None, position, 100)`）本身不受影响——它把 `state` 传成 `None`，根本不发服务端 state。

## 本轮顺带发现并修掉的部署文档缺陷

`docs/deployment.md` §6.3.1 原来只给了 `/reconcile` 的调度示例，而 `/worker`——全代码库唯一的队列消费入口——被描述成「运维手工触发」，且刻意不在 Worker 白名单里。**照文档照抄部署 = 邮件持续进队列、通知永远发不出去**，这正是联调环境积压了数天才被手工排空的成因。§6.3.1 已补上 `/worker` 的调度步骤、顺序要求、批量上限与「204 不能当成功信号」的说明。

同时修正了 `docs/reference.md` 与 `docs/retired.md` 里 `/worker` 的锚点：原值 `notify.rs:336` 落在 `reconcile` 函数体内，锚点审计不会报错（该行存在且非空），但语义是错的；正确值是 `notify.rs:386`。

## 后续（多账号）

单账号由产品决策固定，不在阶段目标内。当前 JMAP 登录与 `Email/query` 均为单一邮箱账号。

**需要你决定：** 是否需要多账号。要的话告诉我，我先把 `REQ-SINGLE-ACCOUNT` 的约束范围拆出来再动代码。
