# 未完成项

> [English version / 英文版 → opengaps.md](opengaps.md)

本文件当前**无未完成项**：四条阶段目标全部关闭，唯一悬置的产品决策（多账户）已由用户于 2026-09-28 定为不做（§3）。保留关闭记录，便于回溯每个缺口为什么关、怎么关的。

已实现的接口与架构设计见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`，已退役的能力见 `docs/retired.md`。验证基线以 `docs/design.md`「P0 门禁」段落为准。

## 待办事项

**无。**

TTL 那条真机绿灯已经用你给的 Upstash URL 跑过了（见 §1），不需要再交任何东西。

## 阶段目标

| # | 项 | 状态 |
| --- | --- | --- |
| 1 | `Email/changes` 的 `newState` 语义 | 已关闭（提交 `269c8f6`） |
| 2 | TTL 实测 | 已关闭（代码收口 `0890eb1` + 真机 `ttl` 两条断言跑通） |
| 3 | Telegram 入站 / Stalwart `PushSubscription` 联调 | 已关闭（真实流量已驱动，操作步骤见 `docs/deployment.md` §4.1） |
| 4 | `/worker` 未在部署文档里 | 已关闭（`docs/deployment.md` §6.3.1 已补） |

门禁：`cargo fmt --check` / `cargo check --locked` / `cargo clippy --locked --all-targets -- -D warnings` 全绿；`cargo test --locked` **94 passed / 0 failed / 4 ignored**（4 个 `#[ignore]` 里 2 个 TTL 断言已用真实 Upstash 跑过：`REDIS_TEST_URL=… cargo test --locked -- --ignored ttl` → **2 passed / 0 failed**；剩 2 个需要真实 JMAP 服务器，该能力已在 staging 联调验证）；文档门 6/6 全绿，含 72 处行号锚点非空校验。

第 5 项不在阶段目标内：多账号产品决策，2026-09-28 已由用户定为不做，决定边界与将来要做的改动面见 §3。

## 阻塞项

**无。** 四条阶段目标全部关闭，代码侧与真机侧都通了，唯一悬置的产品决策也已定案（§3）。

## 已关闭

### 1. TTL 实测（已关闭：常量收敛并在真实机器上完成断言验证）

原条目：全代码库 **12 个 TTL 写入点、17 个 TTL 值**，但只有三个真正落到 Redis 的原子命令（`set_nx_ex` 的 `SET … NX EX`、`retry_or_dlq` Lua 里的 `EXPIRE`、`set_ai_consent` 的 `SET … EX`）在真实 Redis 上被断言过实际 PTTL；其余全是裸字面量。测试替身 `MemoryState::claim_dedup` 直接忽略 TTL 参数，所以单测覆盖的是调用路径，不是过期时长——把一个 24 小时去重窗口改成 `60_480` 仍能编译通过、单测全绿。

三步关掉：

1. **真实 Redis 断言**（提交 `0890eb1`）。`src/state.rs` 的 `real_redis_ttl_tests`，两个 `#[ignore]`-gated 用例，照 `src/domain/jmap/client.rs:522` 的模式：缺 `REDIS_TEST_URL` 时打 skipped 并返回，URL 不落日志。断言三个原子写入点的实际 PTTL。运行：`REDIS_TEST_URL=… cargo test --locked -- --ignored ttl`。
2. **消灭裸字面量**（提交 `0890eb1` 之后的一轮）。17 个值全部收进 `src/state.rs` 的 `pub(crate) mod ttl`，调用点改引常量，新增 `ttl_contract_is_pinned` 逐个断言常量表，并带两条顺序断言（心跳必须短于锁、同意档位必须严格递增）。原条目里够不到的 B 类 Lua 内嵌值也接上了——`retry_or_dlq` 现在用 `ttl::RETRY_COUNTER_SECONDS` 插值生成脚本字符串，脚本里不再有第二个字面量。改值现在 = 改常量 + 断言失败，两处都得过评审。
3. **真机绿灯**（本轮，用 Upstash 实例跑）。两条断言实际连上真实 Redis 读回 PTTL 并通过，B 类 Lua 内嵌值首次落到真机验证。过程见下文「测试自身的四个 bug」。

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
| `ttl::ADMIN_SESSION_SECONDS` | 1,800 | `admin-session:{token}`（同时是 `/admin/session` 返回的 `expires_in`） |
| `ttl::RETRY_COUNTER_SECONDS` | 86_400 | `retry:{stream}:{message.id}` |
| `ttl::CONSENT_TEMPORARY_SECONDS` | 3_600 | `consent:ai:{chat_id}`（「临时 / 一次」与显式 `/ai on` 共用） |
| `ttl::CONSENT_TODAY_SECONDS` | 86_400 | `consent:ai:{chat_id}`（「今天」） |
| `ttl::CONSENT_WEEK_SECONDS` | 604_800 | `consent:ai:{chat_id}`（「7天」） |
| `ttl::CONSENT_MAXIMUM_SECONDS` | 31_536_000 | `consent:ai:{chat_id}`（「直到撤销 / 长期」） |
| `ttl::CONSENT_REVOKED_SECONDS` | 0 | `consent:ai:{chat_id}`（撤销；落 `max(1)` = 1s，语义是「已过期」） |

生产代码里已经不存在裸 TTL 字面量；`worker.rs` 的同意档位测试断言仍保留数字字面量，那是**刻意的**——用独立于常量的期望值去验生产代码，否则两边一起改的话单测永远绿。

本轮顺手修掉测试自身的四个 bug。

- 单测二进制不会跑 `main()`，rustls 没装 crypto provider，`rediss://` 握手直接 panic；现在测试模块自己调 `install_rustls_provider()`。
- `retry:{stream}:{message.id}` 的键构造用消息 id 而不是 run id，断言读到一个从未写入的键，PTTL 返回 `-2`，测试第一句就失败。
- `worker.rs` 对账增量入队那一处 TTL 字面量原先漏收，仍是裸 `86_400`。

第 4 个是真机断言自身不稳定的，前三个是编译期就能发现的：

- 零值 case 原先断言 `assert_within(pttl, 1, 500)`——要求 1 秒 floor 读回来仍在 ±500 ms 内。但读回发生在跨网络 TLS 往返之后，那一秒通常已被消耗完，连跑三次拿到 `-2`（键已过期）、`269`、`118` ms。产品代码本身没问题：`SET … NX EX 1` 确实落地了，`-2` 恰恰证明 EX 生效、键按预期过期。改判据为「`pttl != -1`」——`-1` 才是真正要防的回归（EX 被静默丢弃、键永不过期），加上「`pttl == -2 || pttl <= 1000 + 2000`」这个只设上界的判断，因为一个还活着的值只会从 1s 往下数。改完连跑 6 轮全绿。

**两条 TTL 断言已在真实 Upstash 实例上跑通（`rediss://`，TLS-only；明文 `redis://` 会在 `AUTH` 后收到空回复断连，故必须走 TLS）**：`SET … NX EX`（`claim_dedup`）、`SET … EX 3600`（`set_ai_consent`）、Lua 内嵌 `EXPIRE 86400`（`retry_or_dlq`）三处原子写入点的实际 PTTL 全部落在 2s 容差内。原条目里够不到的 B 类 Lua 内嵌值现在也是真机验证，不只是常量引用。

### 2. `Email/changes` 的 `newState` 语义（提交 `269c8f6`）

原条目担心「服务器无法回放旧增量时返回 `newState`，要求客户端重新基线化」。但 jmap-client 0.4.2 的 `ChangesResponse` 只有 `accountId` / `oldState` / `newState` / `hasMoreChanges` / `created` / `updated` / `destroyed` 加展开的调用参数——**没有 reset / re-baseline 信号字段**。所以 `/changes` 失败在代码里就是一个普通的 `Err`，无法区分「临时故障」和「服务端不再回放这个 state」。这个条目因此无法通过任何观测手段闭环：停机窗口里能观测到的仍然是同一个 `Err`。

代码的实际行为比条目描述的更糟：`Err` 被原样上抛成 `Err(())`，`reconcile:state` 原样保留——下一次 cron 用同一个 dead `sinceState` 重试，再失败，再 503。游标永久冻结，`/reconcile` 永远返回 `503 reconcile_retry` 且没有任何前进。

**修法**：`/changes` 失败时改走重新基线——取一个新鲜的 `session_state()`，写回 `baseline:{fresh_state}:0` 并 `Ok` 返回。position walk 完全不依赖服务端 changelog 保留策略，所以下一轮从 position 0 重走一遍即可恢复；24h 去重键（`enqueue_reconcile_event`，`ttl::DEDUP_JMAP_SECONDS`）保证重放最坏重复一次、不会漏。新增单测 `reconcile_rebaselines_when_changes_replay_is_stale` 锁定该行为。

> position walk 路径（`list_emails_page(None, position, BASELINE_PAGE_SIZE)`）本身不受影响——它的签名里根本没有服务端 state 参数（`None` 是 `folder_id`），所以不会碰到上面那类 token 语义。

### 3. 多账户（已决定不做，用户于 2026-09-28 决定）

单账户由需求固定（`REQ-SINGLE-ACCOUNT`），从未进入阶段目标。用户于 2026-09-28 明确「暂时不做多账户」，此缺口关闭。`docs/design.md` §11.3、`docs/charter.md` 的 `REQ-SINGLE-ACCOUNT` 行与 `docs/deployment.md` 的环境变量表 / 需求映射表原本就按此表述，本轮未改这三处。

**这条决定覆盖的范围**

- **不做多账户单实例**——一个 bot 实例同时服务多个 JMAP 账户。因此不需要 chat→account 路由、按域 / 按文件夹路由、`JmapService` 池化。
- **需要第二个邮箱时的既有路径**：部署第二个 bot 实例，各自独立 `BOT_TOKEN` / `ACCOUNT_ID` / `JMAP_SESSION_URL` 与 Redis 前缀。这是已文档化的既有能力，不需要新代码（`docs/deployment.md`）。
- **重新开启的触发条件**：出现「同一个 TG 会话要按来源邮箱分路由」这类单实例内的路由需求时，再拆 `REQ-SINGLE-ACCOUNT`。

**将来若真要做的最小改动面**（本轮核实过现状，供拆需求时直接用）

- `state:jmap:since` 是**单一全局游标**，无账户维度（读 `src/state.rs:792`、写 `src/state.rs:800`）——两个账户会互相覆盖游标，这是第一个要动的地方。
- `lock:reconcile` 是**单一全局单飞锁**（Redis 实现 `src/state.rs:1130`）——两个账户的对账会被同一把锁串行化。
- `dedup:jmap:{account_id}:{email_id}` **已经带账户段**（`docs/reference.md` §1），键形无需改。
- `ACCOUNT_ID` 环境变量已存在且经校验（留空取 session 主账户，越界值在 `src/domain/jmap/client.rs:485` 报错），业务配置层也能覆盖 `account_id`（`src/config.rs:465` 的 `unwrap_or_else` 回落）。配置面已就绪，缺的只是实例内的分维度状态。

这条不是被跳过，是被明确拒绝：单实例多账户省下的一个部署单元，换不来它引入的路由与游标复杂度。

## 本轮发现并修复的部署文档问题

`docs/deployment.md` §6.3.1 原来只给了 `/reconcile` 的调度示例，而 `/worker`——全代码库唯一的队列消费入口——被描述成「运维手工触发」，且刻意不在 Worker 白名单里。**照文档照抄部署 = 邮件持续进队列、通知永远发不出去**，这正是联调环境积压了数天才被手工排空的成因。§6.3.1 已补上 `/worker` 的调度步骤、顺序要求、批量上限与「204 不能当成功信号」的说明。

同时修正了 `docs/reference.md` 与 `docs/retired.md` 里 `/worker` 的锚点：原值 `notify.rs:340` 落在 `reconcile` 函数体内，锚点审计不会报错（该行存在且非空），但语义是错的；正确值是 `notify.rs:390`。
