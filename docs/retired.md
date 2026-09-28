# 废弃与未采用路线（Retired Routes）

> 本文只记录**两类**东西：①评估过但没采纳的路线；②曾经写进文档、实际上从未存在或从未实现的名字（虚构条目）。
> 目的是**留原因、防复发**——以后看到这些名字，能立刻知道该用什么。
> 当前真实实现与可核对事实见 `docs/reference.md`；缺口与阻塞见 `docs/roadmap.md`；当前设计见 `docs/design.md`。
> 本文**没有行动项**。若某条变成要做的事，去 `docs/roadmap.md` 登记，本文条目保留为决策依据。
> 验证基线：`bfe0fd8`。

---

## 1. 未采用的路线

| 条目 | 类型 | 原因 | 替代或现状 |
|---|---|---|---|
| `teloxide`（Telegram Bot 框架） | 未采用 | Telegram 只需少量 API 调用，不引入重框架（dptree / 会话中间件），减少依赖面与抽象层；`reqwest` 已按 `ARCH-DEPS-STAGE4` 引入 | `src/channel.rs` 用 `reqwest` 自研实现 |
| teloxide 计划 feature 集（`macros` / `rustls` / `redis-session` / `throttle` / `webhooks-axum`） | 未实施 | 随 teloxide 未采用而失效 | — |
| `teloxide-core` 降级方案 | 未采用 | 降级前提是「用 teloxide 太重」；既然整体不引入，降级前提不成立 | — |
| `grammers` / 旧 `telegram-bot` crate | 未采用 | 直接封 Bot API + 自研即可满足需求 | `src/channel.rs` |
| `BotError::Telegram(#[from] teloxide::errors::RequestError)` | 目标形态变体，未落地 | 依附 teloxide，随其废弃 | 无 |
| `BotError::Jmap` / `Storage` / `RateLimited` / `Unauthorized` / `Llm` | 目标形态变体，未落地 | 当时为统一承接 JMAP/Redis/限流/授权/LLM 五类错误而设计；现已由各模块自行处理 | 当前 `BotError` 仅 `Config` / `Io` / `Json` / `State` 四变体（`src/error.rs` 全文） |
| Telegram 服务端 30 msg/s 限流桶 + 429 专用分支 | 目标设计，未实现 | 无真实 Bot 压测数据，不预设实现 | `src/channel.rs` 的 `max_retries`（默认 3，硬上限 5）通用重试；缺口见 `docs/roadmap.md` |
| 多步对话 FSM（`Idle` / `AwaitClarify` / `AwaitConfirm` / `Analyzing` / `AwaitFallback`） | 目标设计，未落地 | 当前 AI 授权只需一个布尔+过期时间，多步态属过度设计 | Redis TTL 授权态，键与 TTL 见 `docs/reference.md` 的 AI 授权态一节 |
| `src/domain/llm/`、`src/domain/command.rs`、`src/domain/notification.rs`、`src/domain/state.rs`、`src/channel/`、`src/notify/`、`src/util/`、`tests/` 子模块拆分 | 目标形态目录，未落地 | 代码量未到需要拆分的规模 | 实际结构见 `docs/design.md` 的工程结构一节：领域层是 `src/domain.rs` + `src/domain/jmap/`（仅 `client.rs`）；LLM 在 `src/ai.rs`；通知在 `src/notify.rs`；**`src/util/` 与 `tests/` 根本不存在**（加密逻辑内联在 `src/state.rs` / `src/config.rs`） |
| `tests/telegram_dispatch.rs` | 目标形态测试，未落地 | 依附 teloxide 测试模式 | `src/channel.rs` 的单元测试（`#[test]`） |
| docker-compose `healthcheck` 示例（`message-weave health --addr ...`） | 已删除的示例 | 运行镜像 `debian:bookworm-slim` 内无 `curl`/`wget`，且 `src/main.rs` 无 CLI 子命令解析，示例照抄必失败 | 由平台 ingress 探测 `/ready`；说明见 `docs/deployment.md` 的就绪探测一节 |

| SPA 管理凭据 = `REDIS_URL` 的 Redis ACL 密码 | 未采用路线 | 混淆基础设施凭据与 UI 管理密码；Redis 无 ACL 密码（TLS-only 托管 Redis）时 `bootstrap_token` 的 `.is_empty()` 守卫让 SPA 永久 401 | 改用 `CONFIG_ENCRYPTION_KEY`（启动必填的 32 字节高熵 hex，常数时间比较） |

### 1.1 teloxide 候选对比（评估记录）

下表与下列理由为**当初的评估结论，保留它是决策依据，不是当前技术事实**——「活跃，最新 0.17，下载量大」等框架属性未经本轮复核，不代表这些 crate 的当前版本状态。

| 框架 | crate | 维护状态 | 特性 | 适配度 | 结论 |
|---|---|---|---|---|---|
| **teloxide** | `teloxide` | 活跃，最新 0.17，下载量大 | dptree 分发、对话 FSM、webhooks+webhooks-axum、Redis 会话存储、throttle、macros、tracing、rustls | ★★★★★ | **首选** |
| grammers | `grammers` / `grammerslib` | 维护一般 | MTProto（非 Bot API），无需 Telegram Bot Token | ★★ | 仅在不能用 Bot API 时 |
| telegram-bot (旧) | `telegram-bot` | 基本停更 | reqwest + futures | ★ | 不推荐 |

**当初倾向 teloxide 的 6 条理由**：
1. 与 `jmap-client` 同为 tokio + reqwest 生态，运行时与 TLS 栈（rustls）可复用。
2. 内建 Dispatcher + `UpdateKind` 枚举匹配命令，与命令路由天然契合。
3. 支持 `webhooks-axum`（生产 Webhook 形态；不使用长轮询 `NG-LONG-POLLING`）与 Redis 会话存储（记住当前文件夹/分页游标，不用 SQLite）。
4. `throttle` feature 天然契合 Telegram 的 30 msg/s 速率限制。
5. `tracing` feature 与本项目观测性统一。
6. `macros` feature 可用 `#[teloxide::command]` 自动解析命令参数，减少样板。

**当时的 feature 集计划**（阶段2 引入时）：`macros`、`redis-session`、`throttle`、`tracing`，按需启用 `webhooks-axum`；TLS 侧 `rustls` 与 `rustls-native-roots` **按需二选一**（后者为前者补 OS 根证书，本机测试方便但生产多此一举）。

**替代/降级**：若 teloxide 升级或破坏性改动，可退到更薄的 `teloxide-core`（保留核心与 types，去掉 dispatcher 抽象）；若需多账户高吞吐，用 `webhooks-axum` + 共享 `axum::Router`。以上两条均随 teloxide 未采用而不再成立。

### 1.2 对话 FSM 状态转移表（目标设计，未落地）

当前没有 FSM：`src/worker.rs` 的 `parse_intent` 直接解析为 `Intent`，AI 授权是 Redis 里的一个布尔加过期时间。曾设计过下面 5 个状态：

| 状态 | 含义 | 进入 | 离开 |
|---|---|---|---|
| `Idle` | 空闲 | 任意完成态 | 收到消息 |
| `AwaitClarify` | 目标/意图不明，等用户选择 | 意图或邮件目标不唯一 | 用户给出明确选择 |
| `AwaitConfirm` | 等确认（AI 分析 / 附件下载） | 用户发起分析/下载但未确认 | 确认 / 取消 |
| `Analyzing` | AI 请求 in-flight | 用户确认分析 | 成功 / 失败 |
| `AwaitFallback` | AI 失败，等确认回退 | 连续 3 次失败熔断 | 用户确认 / 取消 |

当时设定的不变量：`AwaitConfirm` / `Analyzing` / `AwaitFallback` 涉及 AI 或附件下载，**未到确认态不得调用 LLM 或拉取附件**；会话状态为短期状态，统一走外部 Redis 短期 TTL（不使用 SQLite，丢失可接受）。渠道中立要求 FSM 状态与事件用领域类型，不依赖任何渠道 SDK。

该不变量中「AI 分析必须先有用户显式授权」这一条**仍然有效**，已保留在 `docs/design.md` 的会话状态机一节；状态机本身未实现。

---

## 2. 虚构条目（文档曾写、代码从未有）

这些名字**从未在代码里存在**。它们有的出现在目标形态的目录树里，有的出现在未提交的草稿与讨论记录里，共同风险是让人误以为"这个功能已经实现"，故在此登记防止再次出现。

| 条目 | 类型 | 原因 | 替代或现状 |
|---|---|---|---|
| `channel/telegram/{mod,commands,session,render}.rs` | 虚构文件 | 目标目录树里画出，从未创建 | 命令解析在 `parse_intent`（`src/worker.rs`）；会话/授权态走 Redis TTL（`docs/reference.md` 的 AI 授权态一节）；渲染函数在 `src/channel.rs` 内 |
| `delivery:pending:{stream}` | 虚构键 | 与 Redis Streams 的 pending-entries list（PEL）混淆——PEL 由 Redis 内部维护，不是可写键 | 真实键：`delivery:inflight:{stream}:{id}`（EX 60）与 `delivery:committed:{stream}:{id}`（EX 604_800），见 `docs/reference.md` 的投递流水线键一节 |
| `check_config_reload` | 虚构函数 | 未提交草稿中的名字，从未进入代码 | 热更新为 `refresh_business_config`（`src/notify.rs:717`） |
| `push:registration:{sha256(callback_url)}` 曾被写成「360s 注册单飞锁」 | 事实误标 | 该键是 7d 回调→订阅 ID 映射 | 真正的 360s 单飞锁是 `lock:push-register:{sha256(callback_url)}`（`src/notify.rs:897-900`） |
| `MESSAGWEAVE_DOMAIN` 曾被写成「生产未配置时 Worker 白名单失效」 | 虚构断言（未提交草稿/讨论中出现，未进文档） | 该环境变量全仓零命中；Worker 白名单是**无条件 fail-closed**：未知路径 404、method 不符 405、后端缺失或解析失败 503（`cloudflare-worker/src/index.js:77-92`） | 无需配置开关，白名单恒生效 |
| `read_batch` / `retry_or_dlq`「Redis 出错时仍可能返回 `Ok(())`，消费循环因此不会因单次失败退出」 | 虚构断言 | 消费入口是 HTTP handler `worker`（`notify.rs:330`），**不是后台循环**：全仓 `src/` 零命中 `select!`，无信号处理、无常驻 worker 进程。`read_batch`（`state.rs:384`）经 `?` 把 Redis 错误原样上抛（:425），唯一被丢弃的结果是 XGROUP `CREATE` 的 `BUSYGROUP` 幂等保护（:394-401）；`retry_or_dlq` 同样经 `?` 上抛。调用方对每一处 `Err` 都返回 `503 service_unavailable` + `retryable=true`，故「循环不因单次失败退出」这一语义前提本身不成立 | 无——不存在该缺口；`docs/roadmap.md`「代码缺口」4 → 3，`docs/design.md`「已知边界」同句已删 |

---

| `src/domain/llm/` 整个模块族 | 虚构模块树 | `domain/` 下只有 `jmap.rs` 与 `jmap/client.rs`；LLM 只有 `src/ai.rs` 一个文件（`LlmClient`） | 无 |
| `ai::config` / `ai::fallback` / `ai::policy` / `ai::audit` | 虚构模块 | LLM 配置在 `config.rs::LlmConfig`，运行时参数在 `state.rs::RuntimeConfig`，无 policy/audit 概念 | 无 |
| `channel/mod.rs` | 虚构文件 | 渠道层就是 `src/channel.rs`，不是 `channel/` 目录 | 无 |
| `notify::push_handler` / `notify::worker` / `notify::reconcile` 作为模块路径 | 虚构模块路径 | 这些是 `src/notify.rs` 内的自由函数（`jmap_push` / `worker` / `reconcile`），不是模块路径 | 无 |
| `mod_dedup` / `mod_streams` / `mod_sincestate` | 虚构模块名 | `state.rs` / `notify.rs` 都是平铺文件，无子模块；去重与 Streams 逻辑以自由函数存在 | 无 |
| `PushVerification` 类型 | 虚构类型 | 未定义；`register_push`（`notify.rs:884`）内联处理回调 URL 与验证码回写 | 无 |
| `CancellationToken` | 虚构类型 | 未使用；无优雅关闭、无信号处理（`src/` 零命中 `tokio::signal` / `ctrl_c`） | 无 |
| `Preview` 类型 / 4000 字符长邮件保护 / `[继续查看原文]` 按钮 / `/llm-fallback` 按钮 | 虚构类型与 UI | 全部未实现；授权后把全文交给 LLM，失败即回退前 300 字符，无任何截断标注或按钮 | 无 |
| `LlmErr`（5 变体） | 虚构枚举 | 真实是 `AiError`，仅 3 个变体（`InvalidEndpoint` / `Request` / `Response`） | 无 |
| `LLM_TEMPERATURE` / `LLM_MAX_TOKENS` / `LLM_TIMEOUT_SECS` / `LLM_MAX_RETRIES` 环境变量 | 虚构环境变量 | `src/` 零命中；真实只有 `LLM_API_KEY` / `LLM_BASE_URL` / `LLM_MODEL` / `LLM_SUMMARY_TARGET_CHARS` 四个环境变量，加 `llm_timeout_ms` / `max_retries` 两个运行时参数 | 见 `docs/reference.md` AI 授权态一节 |
| `llm.call` tracing span | 虚构观测点 | `src/` 中除 `main.rs`（6 个事件）外**零 tracing 事件、零 span**；LLM 调用无任何日志 | 无 |
| 熔断器 / 半开态 / 熔断后 60s 冷却 / 规则兜底（Redis 共享计数） | 未实施设计 | 只有超时 + 重试；LLM 失败静默降级为前 300 字回退，无用户侧提示、无状态记录 | 无 |
| 定时摘要 / 每日邮件摘要推送 | 未实施 | 未实现 | 无 |
| 附件下载（`send_document` / `Blob/get` / 下载按钮） | 未实施 | JMAP 侧只读；邮件附件仅以 `has_attachment: bool` 形式出现 | 无 |
| `/flag` / `/unseen` / 发信命令 | 未实施 | 当前只识别 5 个意图（帮助 / 同意 / 摘要 / 普通消息 / 未识别） | 无 |
| `Identity` 概念 | 未实施 | 账号识别只依赖 `ACCOUNT_ID`，无身份层抽象 | 无 |
| `run_mode.rs`（作为单独文件） | 虚构文件 | RUN_MODE 校验内联在 `main.rs::validate_env_or_exit` | 无 |
| docker-compose `message-weave health --addr` 示例 | 虚构命令 | 应用无 CLI 子命令；健康检查端点是 `GET /healthz` 与 `GET /ready` | 见 `docs/deployment.md` 的 Health-check 表 |
| 早期设计辩论问题（消息格式 / 长邮件阈值 / 附件策略 / Identity / 监控 / LLM 供应商 / 熔断等 17 条） | 已由代码回答 | 均已被实现的代码给出答案，不再属于待确认项 | 见 `docs/design.md` 的「已由代码回答的早期问题」一节 |

## 3. 已删除的文档

| 条目 | 类型 | 原因 | 替代或现状 |
|---|---|---|---|
| `docs/todo.md` | 已取代（已删除） | 结构不清，与 design/deployment 重叠，且曾承载「本轮已收口」这类历史叙述 | `docs/roadmap.md`（只放缺口、阻塞、阶段目标、决策待定、验收待办） |
| `HANDOFF.md`（根目录） | 临时交接件（已删除，**从未进入 git 历史**） | 交接内容应归位到常驻文档 | 内容并入 `docs/design.md`、`docs/deployment.md`、`docs/reference.md`、`docs/roadmap.md` |

---

## 4. 如何重启其中一条

1. 先在 `docs/roadmap.md` 登记为缺口并归属阶段。
2. 若涉及 Redis 键、TTL、HTTP 路由或默认值，同轮更新 `docs/reference.md`。
3. 本文对应条目**保留**为决策依据——它记录的是当初为什么不这么做，不要删。
