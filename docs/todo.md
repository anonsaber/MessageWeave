# 未完成项与阻塞原因

> 本文件取代已删除的根目录 `HANDOFF.md`。只保留未完成项与阻塞原因；已完成能力见 `README.md`、`docs/design.md`、`docs/deployment.md`。门禁命令见 `docs/deployment.md` §8.0。

## 阻塞（需要真实环境，当前无法验证）

- **真实 Stalwart 邮箱联调**：未运行任何真实邮箱端到端集成。JMAP 客户端代码与配置校验已完成并通过本地 `cargo test`，但 `Email/changes`、`PushSubscription` 等路径没有用真实邮箱账号验证过。→ 阻塞原因：无可用 Stalwart 账号/凭据，禁止使用测试账号或伪造结果。
- **真实 Telegram Bot 联调**：`/webhook/tg` 的签名校验、`getUpdates` 迁移、`setWebhook` 注册、`sendMessage` 出站均未在真实 Bot 上验证。→ 阻塞原因：无 Bot Token，禁止向真实用户发消息。
- **真实 Redis TLS 连接**：`REDIS_URL` 使用 `rediss://` 的 TLS 握手、密码特殊字符 URL 编码、`CONFIG_ENCRYPTION_KEY` 热更新均未实测。→ 阻塞原因：无 Upstash/托管 Redis 实例。
- **`Email/changes` 的 `newState` 语义**（design.md §10.5.1 应修-1）：客户端已用「同 `sinceState` 翻倍 `maxChanges` 扩窗」消除按页漏批，但 `newState` 是否表示"全部待报变更之后"仍需真实 Stalwart 复验，否则停机积压边界无法判定。→ 阻塞原因：无可用 Stalwart 账号/凭据。
- **Push callback 公网映射**：`push:registration` 注册锁（360s）与 callback 映射（7d）TTL 依赖真实 Stalwart 的回调时序。→ 阻塞原因：无真实推送回调可观察。

## 代码缺口（已定位，待实现）

- **Redis 错误映射**：`read_batch` 与 `retry_or_dlq` 在 Redis 出错时仍可能返回 `Ok(())`，消费循环不会因单次失败退出，这类故障只能靠 Redis 侧告警发现（可选改进）。
- **多实例重复投递窗口（残余）**：XAUTOCLAIM 空闲阈值已按批大小缩放（批大小 × 单条上限 300s）；仅当单条事件处理耗时接近 300s 上限时，多实例部署下另一实例仍可能提前认领（仅重复不丢），单实例不受影响。
- **Worker safelist 与注册流程不一致**：`POST /api/push/register` 不在 `cloudflare-worker/src/backends.js` 的 `SAFE_ROUTES` 中，Worker 对其返回 404；`docs/deployment.md` §10.4 已修正为直连后端实例。→ 待决策：加入 safelist，或保持直连后端。

## 未排期（阶段 5「搜索 + 搜索片段 + 打磨」）

- `/search` + `SearchSnippet/get` 高亮。
- 错误消息打磨、速率/退避实测、`clippy`/`fmt`、README 复核。

## 监控验收（Q-DEP-B）

- 容器不写日志文件、只走 stdout，已由 `docker`/`docker-compose.yml` 检查项覆盖。
- 业务事件、计数、时间戳、脱敏摘要的结构化日志已由代码审查确认。
- 未验证项：真实平台的日志采集器是否落盘、是否可检索。**→ 阻塞原因：Q-DEP-B 明确允许延后到真实部署验收。**

## 本轮已收口（2026-09-26）

- **错误 envelope 统一**：全部公开路由错误响应（含 `GET /ready` 不就绪时的 503）统一为 `{"error":<code>,"request_id":<id>}`，可重试的 503 追加 `Retry-After: 30`；`/ready` 就绪时 `200` 仍返回结构化诊断报告。Uptime Kuma 按 HTTP 状态码（`/ready` 期望 200）监控，不受响应体变化影响。
- **Web SPA i18n**：按浏览器语言判断 `zh`/`en`，默认英文，保持纯静态 SPA（`data-i18n` 属性 + 内联消息字典，无新增资源文件）。
- **文档**：`README.md` 改英文默认，新增 `README.zh-CN.md`，两版章节与关键事实 1:1 对齐；`docs/design.md`、`docs/deployment.md`、`AGENTS.md` 同步修正滞后描述。
