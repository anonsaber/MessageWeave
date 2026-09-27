# MessageWeave — Roadmap

> 只放**缺口**、**阻塞**与**阶段目标**。已实现能力见 `README.md`、`docs/design.md`、`docs/deployment.md`；
> 可核对的事实（Redis 键、TTL、错误码、路由）见 `docs/reference.md`。
> 验证基线：`ed93358`。门禁命令见 `docs/deployment.md` 的 Gate 一节。

## 阶段目标

`GATE-P0` 已过（fmt / clippy / check / test，Debian 容器内）。下一阶段目标：
在真实邮箱 + 真实 Telegram Bot 上完成端到端联调，消掉下面的阻塞项。

## 阻塞（需要真实环境，当前无法验证）

- **真实 Stalwart 邮箱联调** — 未做任何真实邮箱端到端集成。JMAP adapter 代码与配置校验已完成并通过本地 `cargo test`，但 `Email/changes`、`PushSubscription` 路径未在真实账号上验证。`REQ-JMAP-SESSION-URL` 已实现（`D-G1-1`）但待真机验证。**阻塞原因**：无可用 Stalwart 账号/凭据；禁止使用测试账号或伪造结果。
- **真实 Telegram Bot 联调** — 入站 `POST /webhook/tg` 的 secret 头校验 + chat 白名单（`notify.rs:95` 起）、出站 `sendMessage`（`channel.rs`）均已在代码内并有单元测试，但**都没有在真实 Bot 上跑过**。另：向 Telegram 注册 webhook URL 是运维步骤（调 `setWebhook`），应用内不实现；长轮询 `getUpdates` 从未实现（见 `docs/retired.md`）。**阻塞原因**：无 Bot Token；禁止向真实用户发消息。
- **真实 Redis TLS 连接** — `rediss://` 握手、密码特殊字符 URL 编码、`CONFIG_ENCRYPTION_KEY` 热更新未实测。**阻塞原因**：无托管 Redis 实例。
- **`Email/changes` 的 `newState` 语义** — 客户端已用「同 `sinceState` 翻倍 `maxChanges` 扩窗」消除按页漏批，但 `newState` 是否表示"全部待报变更之后"仍需真实 Stalwart 复验，否则停机积压边界无法判定。**阻塞原因**：无可用 Stalwart 账号/凭据。
- **Push callback 公网映射** — 单飞锁 `lock:push-register:{sha256(callback_url)}`（360s）与映射 `push:registration:{sha256(callback_url)}`（7d）的 TTL 需真实回调时序验证。**阻塞原因**：无真实推送回调可观察。

## 代码缺口（已定位，待实现）

- **`/search` + `SearchSnippet/get` 高亮** — 未实现。`src/` 中 `SearchSnippet` / `search_snippet` 零命中，worker 的 `Intent` 枚举也无 `Search` 变体，因此该路径无法进入。设计文档中的接口表已标注「（未实现）」。
- **Telegram 429 退避实测** — `src/channel.rs` 的 `max_retries`（默认 3，硬上限 5）不针对 Telegram 服务端 30 msg/s 限流做专门退避；429 行为无实测数据。需真实 Bot 压测后再决定是否引入指数退避。
- **Redis 错误映射** — `read_batch` 与 `retry_or_dlq` 在 Redis 出错时仍可能返回 `Ok(())`，消费循环不会因单次失败退出，这类故障只能靠 Redis 侧告警发现。（可选改进）
- **多实例重复投递窗口（残余）** — XAUTOCLAIM 空闲阈值已按批大小缩放（批大小 × 单条上限 300s）；仅当单条事件处理耗时接近 300s 上限时，多实例部署下另一实例仍可能提前认领（仅重复不丢）。单实例不受影响。
- **Worker safelist 与注册流程不一致** — 网关白名单是无条件 fail-closed 的（无开关可放宽，见 `docs/reference.md` 的网关路由矩阵一节）。后端共注册 18 条路由，其中 12 条经 Worker 转发、6 条不转发；不转发的 6 条里只有 `POST /api/push/register` 与 `POST /api/push/disable` 按设计**需要**业务流量直连后端（另有 `GET, PUT /api/enabled`、`POST /api/bootstrap`、`POST /worker`、`GET /healthz` 四条属内部运维路径，不需要业务流量）。因此实际缺口只有这两个端点。**待决策**：把它们加进 safelist（单入口，但放宽了「只转发明细路由」的边界），还是维持双入口（边界严，但运维要配两条路径）。

## 决策待定

这两条都是**上线时的运维选择，不是设计分歧**——不影响代码结构、不影响门禁、可以无限期挂着而不产生技术债。建议处理方式：**先按任一默认值写进部署文档，等真的上平台时再确认**；确认前不要为它们开 issue 或分支。

- **平台 URL / 域名与证书配置方**（`Q-DEP-A`，关联 `C-HTTPS-URL`；默认按「平台自动证书 + 自管反代二选一，先文档化两条路径」处理）— 平台自动证书 or 自管反代，以及 4 条路径的可达性验证方式。
- **外部调度器选型**（`Q-DEP-B`，关联 `NFR-RECONCILE-INTERVAL`；默认按「k8s CronJob」写）— 系统 crontab / k8s CronJob / CI scheduled / 第三方 cron。仅影响运维方式，不改变架构。

## 验收待办

- **真实平台日志采集验收**（`Q-DEP-B` 允许延后）— 容器不写日志文件、只走 stdout 已由代码审查确认（`C-LOG-STDOUT-ONLY`）；结构化日志字段与脱敏已由代码审查确认（`SAF-LOG-PURITY`）。未验证项：真实平台采集器是否落盘、是否可检索。
- **`/ready` 端到端依赖探测**（`GATE-READY-DEPS`）— 当前 `ARCH-READY-BASELINE` 仅覆盖配置完整性 + Redis 可达性；JMAP session / Telegram getMe 等端到端探测尚未接入。
- **JMAP 只读 adapter 真机验证**（`GATE-G1-JMAP-READONLY`）— 代码已实现（mock + `#[ignore]` 真机测试），待 `cargo test -- --ignored jmap::`。
