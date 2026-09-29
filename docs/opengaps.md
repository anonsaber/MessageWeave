# Open Gaps（`docs/opengaps.md`）

> 只登记**仍未完成、仍未验证**的事项。已实现能力见 `docs/reference.md`、`docs/design.md`、`docs/deployment.md`；
> 被移除、废弃、明确拒绝的能力见 `docs/retired.md`。门禁命令见 `docs/deployment.md` 的 Gate 一节。

## 阶段目标

`GATE-P0` 已过（fmt / clippy / check / test，Debian 容器内）。真实邮箱与真实 Telegram Bot 的**出站与查询方向**已在真机联调通过（见下文各条的「已验证」括注）。
剩余目标收窄为：**在 Telegram 侧执行 `setWebhook`** 与 **`PushSubscription` 回调链路**，消掉下面 5 条阻塞项。

## 阻塞（需要真实环境，当前无法验证）

- **真实 Stalwart `PushSubscription`** — `Email/changes` 与 `Email/query` 搜索已在真实账号跑通（2026-09-28：`POST /reconcile` 持久化了 `baseline:` 游标，而该游标只在 `reconcile()` 返回 `Ok(new_state)` 之后写入（`notify.rs:331`），即 `fetchChanges` 端到端成功）。**未验证**：账号角色是否具备 push 权限、Stalwart 回调重试次数、TTL/对账间隔的匹配。**阻塞原因**：无真实推送回调可观察。禁止使用测试账号或伪造结果。

- **真实 Telegram 回调（Telegram→本服务方向）** — 应用侧入站 secret 头校验 + chat 白名单（`notify.rs:225-263`）、出站 `sendMessage`（`channel.rs`）、出站重试预算与重试耗尽终局错误（`channel.rs:182`）已在真实 Bot 上验证（2026-09-28：`POST /debug/notify` 投递成功；伪造 update 全链路「入队→worker→JMAP `Email/query` 搜索→回帖」通过；以上两条均经用户确认收到）。**未验证**：真实的 Telegram→本服务回调从未到达——向 Telegram 注册 webhook URL 是运维步骤（调 `setWebhook`），应用内不实现，**目前尚未执行**，因此 Telegram 侧既不知道本服务的 webhook URL，也不持有 secret 值。另：长轮询 `getUpdates` 从未实现（见 `docs/retired.md`）。**阻塞原因**：需在 Telegram 侧执行 `setWebhook`（`url=<公网地址>/webhook/tg`、`secret_token=<业务配置里的值>`，两者必须一致）并触发一次真实回调。

- **真实 Redis TLS 连接** — `rediss://` 握手、密码特殊字符 URL 编码、Redis ACL 密码均未实测。**阻塞原因**：无托管 Redis 实例。注：`CONFIG_ENCRYPTION_KEY` 与 `REDIS_URL` 只在启动时读取（`src/config.rs:17`、`src/main.rs:54`），本系统不存在这两项的热更新路径，也不在本系统的待办范围内。

- **`Email/changes` 的 `newState` 语义** — 客户端已用「同 `sinceState` 翻倍 `maxChanges` 扩窗」消除按页漏批；2026-09-28 在真实账号上跑通 `fetchChanges` 并拿到 `baseline:` 游标，但 `newState` 是否表示"全部待报变更之后"仍需**停机积压后恢复**的场景才能判定，否则积压边界仍无法确证。**阻塞原因**：无可控的真实积压场景。

- **Push callback 公网映射 + 单飞锁 TTL** — 单飞锁 `lock:push-register:{sha256(callback_url)}`（360s）与映射 `push:registration:{sha256(callback_url)}`（7d）的 TTL 需真实回调时序验证。**阻塞原因**：无真实推送回调可观察。

## 代码缺口（已定位）

仅 1 条，按产品决策保留不改；其余已实现，见各自 commit 与 `docs/reference.md`。真正的阻塞是上面的真实环境依赖。

- **`/debug/notify` 空白名单不拦截**（`SAF-DEBUG-ALLOWLIST`）— 白名单校验只在白名单**非空**时才生效（`src/debug.rs:232-235`），因此业务白名单未配置时 `chat_id` 可为任意值。**已定位、已文档化、不改代码**：该面默认关闭且需 Bearer，且业务白名单为空时主产品本身即接受任意 chat，沿用同一语义不构成额外泄露面。若日后要收紧，需把语义改为「空白名单则拒绝」，并补单元测试、跑 `GATE-P0`。

## 后续（可选项，产品决策后开启）

- **多账号支持**（`REQ-SINGLE-ACCOUNT`）— 当前单账号由产品决策固定，非阶段目标。
