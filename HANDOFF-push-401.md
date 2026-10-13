# HANDOFF — 邮件 Push 通知投递问题

> **日期**: 2026-10-13  
> **状态**: 代码修复已完成，**未部署**  
> **接手方优先级**: P0 — 用户阻塞中，邮件 push 收不到

---

## 1. 问题描述

**症状**: 用户配置了 Stalwart 邮件服务器 + Cloudflare Worker + Telegram Bot，通过 **cron 定时任务**（`/reconcile` 路径）可以正常收到邮件通知，但 **邮件 Push**（Stalwart → `/push/jmap` 路径）**完全收不到**。

**用户已确认的现象**:
- `/help` 等 Telegram 命令秒回
- cron 路径（`/reconcile` → JMAP `/changes` → worker → Telegram）工作正常
- 邮件 push 通知（Stalwart → `/push/jmap`）从未到达 Telegram

---

## 2. 根因分析（已完成，结论确认）

### 2.1 根因: 验证码 key 5 分钟后过期（旧代码 `TTL=300`）

Stalwart 按 RFC 8620 协议向我们注册 Push 订阅时:

1. **注册时**: Stalwart 发第一次带 `verificationCode` 的验证请求，我们的后端把验证码存进 Redis（key = `push:subscription:{id}`，value = 验证码）。**旧代码 TTL = 300 秒（5 分钟）**。
2. **事件推送时**: 每封新邮件到达后，Stalwart 发带 `emailId`/`changeType` 但**不带** `verificationCode` 的事件推送。我们的后端去 Redis 查 `push:subscription:{id}` 确认订阅已验证 — 但 key 已在 5 分钟后过期 → 返回 **401 unauthorized** → 邮件通知被丢弃。

### 2.2 时间线证据（来自 Stalwart 日志）

| 时间 (UTC) | 事件 |
|---|---|
| 02:52:30 | 用户执行 register，Stalwart 分配 `push_subscription_id: "b"`，向 `/push/jmap` 发第一次验证请求，后端存 key（TTL=300） |
| ~02:57:30 | Redis key 过期被自动删除 |
| 02:58:49 | 测试邮件到达 Stalwart，changeId=305，documentId=167 |
| 02:58:50 | Stalwart 向 `/push/jmap` 推送事件 → 后端查 key → **401 unauthorized** |

**注册到 401 恰好间隔 ~6 分钟，与 TTL=300（5 分钟）完全吻合。**

### 2.3 为什么 cron 能收到

cron 走 `/reconcile` → 主动调 JMAP `/changes` → 拉新邮件 → 发 Telegram。这条路径**完全不依赖验证码 key**，所以不受 TTL 影响。push 路径依赖 key 核对，key 过期就 401。

### 2.4 次要问题: 限流器 429

注册日志里出现 `429 push_verify_rate_limited` — Stalwart 在 30 秒内重复发了验证请求，第二个被限流器（`ratelimit:push-verify:{subscription}`，TTL=30s）拦住。但**第一个请求已成功存了 key**（只是 5 分钟后过期），所以 429 不是当前 401 的原因，只是鲁棒性问题。

---

## 3. 已完成的修复（3 个 commit，均已 push 双 remote）

| Commit | 修复内容 | 关键代码位置 |
|---|---|---|
| `d452f95` | `PUSH_SUBSCRIPTION_SECONDS` 300 → 604800（7 天） | `src/state.rs` ttl 模块 |
| `7331cb7` | `PUSH_SUBSCRIPTION_SECONDS` 604800 → **0（永不过期）** | `src/state.rs` ttl 模块 |
| `f9160c0` | ① 验证响应回 `{"verificationCode":"..."}`（原返回空 204，违反 RFC 8620 §7.2）；② `verify_push_subscription` JMAP API 改为 best-effort（失败不阻塞） | `src/notify.rs` `jmap_push` 验证分支 |

### 3.1 关键代码变化

**`src/state.rs`** — TTL 常量:
```rust
pub const PUSH_SUBSCRIPTION_SECONDS: u64 = 0;  // 永不过期
```
`remember_push_subscription` 处理 `ttl_seconds == 0` 时用 `SET key value`（不带 `EX`），MemoryState 用 `expiry = i64::MAX`。

**`src/notify.rs`** — 验证流程（当前代码，已修但未部署）:
```
if 带 verificationCode（验证请求）:
    1. rate limiter claim_dedup → 429 拦截（❗这在 remember 之前，有问题）
    2. verify_push_subscription JMAP API → best-effort（warn 不阻塞）
    3. remember_push_subscription → 存 key（TTL=0 永不过期）
    4. set_push_subscription_status → "verified"
else（事件推送）:
    查 push:subscription:{id} 是否存在 → 不存在 401
```

**RFC 8620 §7.2 响应**（`notify.rs` 的 `jmap_push` handler，验证请求分支末尾）:
```rust
if let Some(code) = push.verification_code.as_deref() {
    return axum::Json(serde_json::json!({
        "verificationCode": code
    })).into_response();
}
return StatusCode::NO_CONTENT.into_response();
```

---

## 4. 未完成的工作（接手方要做）

### 4.1 【P0】部署最新代码到服务器

**当前服务器跑的是旧代码（TTL=300），三个修复 commit 都在 git 里但没部署。**

部署方式（Docker）:
```bash
# 在服务器上
git pull origin main  # 拉到 f9160c0 或更新
docker build -t messageweave .
docker stop <容器名>
docker run --rm --env-file .env -p 8080:8080 messageweave
```

或者如果不用 Docker:
```bash
git pull origin main
cargo build --locked --release
# 重启运行中的 message-weave 进程
```

### 4.2 【P0】部署后重新注册 Push 订阅

部署新代码后，旧的 key 可能还残留（或已过期），需要让 Stalwart 重新走一次验证流程:

```bash
# 变量
BACKEND="https://messageweave.motofans.club"
TOKEN="你的 worker_token（业务配置里的）"

# 1. 禁用旧订阅
curl -X POST "$BACKEND/api/push/disable" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"callback_url":"https://messageweave.motofans.club/push/jmap"}'

# 2. 重新注册（Stalwart 会发新验证请求，新代码存 key 且永不过期）
curl -X POST "$BACKEND/api/push/register" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"callback_url":"https://messageweave.motofans.club/push/jmap"}'

# 3. 发测试邮件，观察 Stalwart 日志确认不再 401
```

### 4.3 【P1】修复限流器顺序（代码改动）

**问题**: `notify.rs` `jmap_push` 的验证分支中，rate limiter（`claim_dedup("ratelimit:push-verify:{subscription}")`）在 `remember_push_subscription` **之前**。如果 Stalwart 在 30 秒内重发验证请求，第二个请求被 429 拦住 → `remember_push_subscription` 不会被调用 → 那次尝试的 key 没存。虽然第一个请求通常已存了 key，但这不鲁棒。

**修法**: 把 `remember_push_subscription` 移到 rate limiter **之前**，rate limiter 只控制**出站** `verify_push_subscription` JMAP API 调用（防止频繁调外部 API），**不拦验证请求本身**。

修改后流程应该是:
```
if 带 verificationCode:
    1. remember_push_subscription（先存 key，TTL=0）    ← 移到最前
    2. set_push_subscription_status → "verified"
    3. rate limiter claim_dedup → 只拦 JMAP API 调用，不拦整个请求
    4. verify_push_subscription JMAP API → best-effort
```

**需要同步更新的文档**（改代码后必须一起改，否则 GATE-DOCS 会挂）:
- `docs/reference.md` / `docs/reference.zh-CN.md` — `ratelimit:push-verify:{subscription}` 键说明行 + 429 `push_verify_rate_limited` 错误码表行
- `docs/opengaps.md` / `docs/opengaps.zh-CN.md` — TTL 表 `PUSH_VERIFY_LIMIT_SECONDS` 行
- `docs/design.md` / `docs/design.zh-CN.md` — "Push verification code writing current limiting" 段落

### 4.4 【P2】验证完整链路

部署 + 重新注册后:
1. 发测试邮件
2. 看 Stalwart 日志: 验证请求应该返回 200 + `{"verificationCode":"..."}`，事件推送应该返回 2xx（不再 401）
3. 看 Telegram 是否收到通知
4. 等 10+ 分钟再发第二封（确认 key 不再过期）

---

## 5. 两条投递路径说明

| 路径 | 触发方式 | 依赖验证码 key | 当前状态 |
|---|---|---|---|
| **Cron** | 定时调 `/reconcile` → JMAP `/changes` → 拉新邮件 → worker → Telegram | ❌ 不依赖 | ✅ 正常工作 |
| **Push** | Stalwart → `/push/jmap`（事件推送）→ 查 key → worker → Telegram | ✅ 依赖（401 的原因） | ❌ 旧代码 key 过期 → 401 |

---

## 6. 环境信息

- **Stalwart**: `0.16.26`，Docker 运行在 `mail.motofans.club`
- **Push 订阅 ID**: `"b"`（Stalwart 分配的单字符 ID）
- **回调 URL**: `https://messageweave.motofans.club/push/jmap`
- **后端仓库**: `/home/okabe/Repo/messageweave`
- **双 remote**: `origin`（motofans.club）+ `upstream`（GitHub fork）
- **测试基线**: Rust 105 passed / 0 failed / 4 ignored，GATE-DOCS 6/6 PASS
- **最新 commit**: `f9160c0`（已 push 双 remote）

---

## 7. 已知的文档残留（非阻塞）

- `docs/opengaps.md` 声称"当前无未完成项"，但限流器顺序问题（§4.3）还没修 — 接手方修完后应更新
- `docs/reference.md` / `docs/reference.zh-CN.md` 的 `ratelimit:push-verify` 与 429 错误码表行，在限流器顺序修完后需要改
- `docs/roadmap.md` 不存在（之前有任务提到要创建，没落地）
- `audit_anchor_semantics.py` 有 7 个候选（历史遗留，非本轮引入）
- `docs/reference.zh-CN.md` 机翻质量问题（历史遗留）

---

## 8. 快速诊断命令

```bash
# 查看部署版本
curl -s https://messageweave.motofans.club/api/status | jq .

# 查看 push 注册状态（需要 worker_token）
TOKEN="你的token"
curl -s https://messageweave.motofans.club/api/push/status \
  -H "Authorization: Bearer $TOKEN" | jq .

# 查看 Redis 中验证码 key 是否存在（需要 Redis 访问）
# key 格式: push:subscription:{subscription_id}
redis-cli GET "push:subscription:b"

# 查看 Stalwart 日志（在 mail.motofans.club 上）
docker logs <stalwart容器名> --since 10m 2>&1 | grep -i push
```
