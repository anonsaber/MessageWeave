# MessageWeave — 项目章程（Project Charter）

> 本文件是**项目专属**规范：项目目标、技术选型、安全不变量、实现阶段、禁止事项、
> 测试验收、文档边界与**稳定 ID 注册表**。
>
> 通用、语言无关的代码编写与环境构建规范见 [`../AGENTS.md`](../AGENTS.md)；
> 可核对事实（路由 / Redis 键与 TTL / 配置项 / 错误码 / 出站常量）的唯一权威来源见
> [`reference.md`](reference.md)；缺口与阻塞见 [`opengaps.md`](opengaps.md)。
>
> 三者冲突时：**可核对事实以 `reference.md` 为准，项目约束以本文件为准，质量规则以 `../AGENTS.md` 为准。**

> **代码基线** `fc686ab`（src/ 行号锚点）｜文档以当前 `main` 为准，锚点须随代码基线复核。

---

## 1. 项目目标

一个**部署在 Docker + 外部托管 Redis** 的 JMAP 邮件通知服务：

- 通过 JMAP 只读拉取邮件变更，判断是否需要通知。
- 可选的 LLM 分析（AI 开关 + 用户级外部授权），分析结果**不存储、不进入消息**。
- 通过 Telegram 向**白名单 chat** 推送**仅含元数据**的通知。
- 用户通过 Telegram 命令（`/summary`、`/search`）与 `/help` 交互。
- 通过浏览器 SPA 配置业务参数；通过受保护接口开关业务处理。

**不做**：多租户、账号体系、Web UI 之外的客户端、消息持久化、邮件双向同步。

## 2. 技术选型（锁定）

| 层 | 选型 | 版本 |
|---|---|---|
| 语言 | Rust | edition 2021 |
| Web 框架 | axum | 0.8 |
| 异步 | tokio | 1 |
| 状态存储 | Redis | redis 0.27 |
| JMAP 客户端 | jmap-client | 0.4.2（`default-features = false` + features `["async","rustls"]`） |
| Telegram | 自建轻量客户端（`src/channel.rs`），**不引入** bot 框架 | — |
| HTTPS 客户端 | reqwest | 0.13 |
| 密钥类型 | secrecy | 0.10 |
| 加密 | ring | 0.17 |
| 日志 | tracing + tracing-subscriber | 0.1 / 0.3 |
| 调度 | 外部 cron → `POST /reconcile` | — |
| 前端 | 原生 HTML + 单文件 JS + 单文件 CSS，**无构建工具、无框架** | — |
| 网关 | Cloudflare Worker（纯 JS，零依赖） | — |

## 3. 安全边界（不可违反）

23 条，编号 `SAF-*` / `REQ-*` / `C-*`，全部登记在 §8 注册表。

1. **JMAP 只读**：不得使用 Send 类接口，禁止修改邮箱任何内容。
2. **不出站发信**：`src/channel.rs` 是唯一 Telegram 出站出口；领域层禁止直接发信。
3. **禁止长连接**：不得引入 WebSocket / SSE / 长轮询；所有通信为短请求-响应。
4. **无状态恢复**：不得用本地文件或内存状态做恢复；崩溃后仅依赖 Redis 中的可重放记录。
5. **无数据库**：不得引入关系型或文档型数据库。
6. **无本地写**：除 stdout 日志外，进程不写任何文件。
7. **日志只走 stdout**：不写日志文件、不接外部日志 agent。
8. **状态只在 Redis**：一切可变状态只存在于外部托管 Redis。
9. **密钥只来自环境变量**：进程配置不从 Redis 读取。
10. **密钥不回显**：任何日志、错误响应、Debug 输出都不得包含密钥本体。
11. **密钥常量时间比较**：管理凭据与 admin session 摘要必须常数时间比对。
12. **通知只含元数据**：Telegram 通知只含发件人 / 主题 / 时间（+ 附件数），正文绝不进入通知。
13. **AI 结果不进入消息**：LLM 输出只用于决策，不落 Redis、不进入 Telegram 消息。
14. **AI 需用户级外部授权**：只有显式授权过且未过期的 chat 才触发 LLM；授权是外部行为，进程不代授权。
15. **业务白名单**：出站 Telegram chat 必须命中 `CHAT_ALLOWLIST`。
16. **诊断面默认关闭**：`/debug/*` 需要 `DEBUG_ENABLED`（或 `--debug`）+ `DEBUG_TOKEN` 双因子同时满足。
17. **诊断面不进网关**：`/debug/*` 不在 Worker `SAFE_ROUTES`，只能直连后端 origin。
18. **网关只透传、不决策**：Worker 不解析请求体、不校验业务逻辑、不下发 `Retry-After`。
19. **AI 结果不重试**：LLM 分析失败即视为「不通知」，不重试、不降级为无 AI 通知。
20. **无状态进程**：两个容器实例并发安全；无本地写。
21. **无长轮询调度**：调度由外部 cron 触发 `POST /reconcile`。
22. **AI 分析不持久化**：分析结果与授权状态之外的中间态不落盘。
23. **JMAP capability 动态发现**：禁止硬编码 JMAP 能力列表，必须运行时读取。

## 4. 实现阶段（历史顺序，用于解释代码结构）

| 阶段 | 内容 | 稳定 ID |
|---|---|---|
| 0 | 配置 / 状态存储接口抽象，HTTP 骨架 | `ARCH-DEPS-STAGE0` |
| 1 | JMAP 只读 adapter | `ARCH-DEPS-STAGE1`, `MOD-JMAP-CLIENT` |
| 2 | Telegram 渠道自建客户端（不引入 bot 框架） | `ARCH-DEPS-STAGE4`, `MOD-TELEGRAM-NOTIFY` |
| 3 | AI 可选接入 | `REQ-AI-EXTERNAL-CONSENT`, `REQ-AI-FUSE` |
| 4 | Redis 落地 | `ARCH-DEPS-STAGE4` |
| 5 | 生产化（debug 面、网关、对账） | `MOD-DEBUG`, `SAF-DEBUG-GATE` |

**阶段编号是历史顺序，不是当前状态声明。** 当前状态以 `reference.md` 与代码为准。

## 5. 禁止事项

1. **不得引入未在本文件 §2 登记的依赖。** 新增依赖须先在本文件登记并说明理由。
2. **不得为通过门禁而删除测试、跳过校验、放宽检查级别、删除访问控制。**
3. **不得在文档或注释中引用不存在的依赖、函数、类型、Redis 键或行号。**
4. **不得在代码中写「计划实现 X」类注释**；占位实现必须显式标注为占位。
5. **不得在 SPA / 网关引入构建工具或框架**；`web/` 保持单文件 JS + 单文件 CSS。
6. **不得让 SPA 持久化任何凭据**（无 `localStorage` / `sessionStorage` / Cookie 赋值）。
7. **不得提交未验证的代码**；门禁结果必须来自本轮实际执行。
8. **不得声称已通过真实环境验证，除非本轮真的连过。**
9. **不得在 `web/`、`cloudflare-worker/` 之外引入前端资源**。
10. **不得让业务配置静默回退到环境变量。**

## 6. 测试验收与门禁

### 6.1 变更验证闭环

任何涉及运行时行为或可核对事实的改动，**同一改动内**完成：实现 → 测试 → 文档。

不得只改代码不改文档，也不得只改文档不改代码。

### 6.2 GATE-P0（代码门禁）

Debian 最小发行版容器内执行，容器内必须显式 `export PATH=/usr/local/cargo/bin:$PATH`：

```bash
cd /home/okabe/Repo/messageweave && docker run --rm --user 1000:1000 \
  -e HOME=/tmp -e RUSTUP_HOME=/app/.gate-cache/rustup -e CARGO_HOME=/app/.gate-cache \
  -v "$PWD":/app -w /app rust:1-slim-bookworm \
  bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo fmt --all -- --check && cargo check --locked && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked 2>&1 | tail -12'
```

**当前基线：93 passed / 0 failed / 4 ignored**（新增 4 个游标解析与 walk 分页单测后复测；ignored：`real_server_tests::session_list_and_read_smoke` 与 `debug::tests::debug_config_reports_timezone_of_business_configured_app` 需外部真实 JMAP 服务器凭据，`real_redis_ttl_tests::ttl_claim_dedup_sets_the_exactly_requested_expiry` 与 `real_redis_ttl_tests::ttl_consent_and_retry_landing_on_real_redis` 需 `REDIS_TEST_URL`）。

**注意**：`docker run` 的 bash `-lc` 脚本必须用**单引号**包裹。用双引号会先在宿主机展开 `$PWD` / `$PATH`，容器内找不到 cargo。

### 6.3 GATE-DOCS（文档门禁）

```bash
cd /home/okabe/Repo/messageweave && bash scripts/docs_check/run_all.sh
```

6 个校验器全过、exit 0 才算通过。只读，从不写文件。

新增 `check_file_size`：`src/`、`web/`、`cloudflare-worker/src/` 下超过 500 行的源文件必须带
`SPLIT-EVAL:` 标记（AGENTS.md §2.1）。它证明标记存在，不证明写下的拆分理由站得住脚。

新增 `audit_paths`：`docs/*.md` 与 `AGENTS.md` 里反引号内的路径型 token 必须真实存在。
它存在的原因是：文档曾出现从未创建的路径，而虚构路径读起来和真实路径完全一样，读者无法分辨
「我们决定不做」和「这写错了」。`docs/retired.md` 同样在检查范围内——废弃登记里的路径也必须真实，
未落地的形态只以文字描述。豁免只有三类，都在脚本里可见：指向外部仓库的所有者前缀
（`stalwartlabs/...`）、git 历史可证已删除的文件、以及一份注明理由的知名文件名白名单。
它不检查反引号外的散文，裸文件名也只按同名校验解析。

**`0 error` 只证明「可达」**：行号存在、引用可解析、表格列数一致、标记就位。
**它不证明语义正确。** 报告校验结果时必须说明这个区分。

### 6.4 前端测试

```bash
cd /home/okabe/Repo/messageweave/web && node --test *.test.mjs
cd /home/okabe/Repo/messageweave/cloudflare-worker && node --test test/*.test.js
```

### 6.5 不变量必须有断言

§3 的每条安全不变量都要有一条测试或代码级断言能证明它成立，
而不是只写在文档里。纯重构不得改变测试总数。

### 6.6 外部依赖测试

Stalwart 与 Telegram 的业务凭据已在真机联调通过——出站、查询与**入站回调**两个方向
均已验证（`setWebhook` 与 `PushSubscription` 的注册、回调、验证往返全部走通），托管 Redis
已连接。真实环境集成测试必须用 `#[ignore]` 标注，缺凭据时静默跳过（不泄露凭据、不判失败）。

**环境阻塞项**（未验证，不得声称已验证）见 [`opengaps.md`](opengaps.md)「阻塞」区：
`Email/changes` 的 `newState` 语义、TTL 数值实测（12 个写入点 / 17 个值）。

## 7. 文档边界

每个文档只回答一个问题；**不得跨文档复制表格、清单或数值。**

| 文档 | 回答什么 | 权威范围 |
|---|---|---|
| [`../AGENTS.md`](../AGENTS.md) | 通用、语言无关的代码编写与环境构建规范 | 质量规则 |
| `README.md` / `README.zh-CN.md` | 面向使用者：是什么、怎么跑、怎么配置 | 用户可见事实（中英必须信息对等） |
| `docs/design.md` | 为什么这样设计：数据流、模块边界、状态机、错误处理 | 架构意图 |
| `docs/deployment.md` | 怎么部署：Dockerfile、secrets、网关、多实例、cron | 部署与运维 |
| `docs/reference.md` | 可核对事实的唯一权威来源：路由、Redis 键与 TTL、配置项、错误码、出站常量 | **可核对事实** |
| `docs/opengaps.md` | 缺口、阻塞、下一阶段目标 | 未决项 |
| `docs/retired.md` | 已废弃或已改名方案的记录与替代指向 | 历史决策 |
| `docs/charter.md` | 本文件：项目约束、技术选型、实现阶段、安全不变量、稳定 ID 注册表 | 项目约束 |
| `web/`（无文档，4 个文件） | 管理 SPA 源：静态配置页与前端逻辑，由 `web/config.test.mjs` 覆盖 | 前端行为（权威事实记在 `docs/design.md` 与 `docs/reference.md`） |
| `cloudflare-worker/README.md` | 网关自身的配置与语义 | 网关 |

**跨文档引用规则**：

- 跨文档**禁止**用 `§x.y` 章节号（章节号随编辑漂移）；用稳定 ID。
- **同文档内**允许 `§x.y`。
- 新增稳定 ID 必须先登记到 §8 注册表；未登记 ID 不得出现在注释、文档或标识符中。
- **改公共 API / Redis 键与 TTL / 配置项 / 错误码 / 用户可见文案时，同轮同步全部相关文档。**
- **新增文档必须加入 `scripts/docs_check/` 各校验器的文档清单**，否则它永远不会被校验。

## 8. 稳定 ID 注册表

**唯一权威索引表。** 跨文档引用一律用稳定 ID（不用 `§x.y` 章节号）。

- ID 语义稳定、全仓唯一、可 grep；本表登记每个 ID 的**定义文件**与一句话说明。
- 新增约束 / 组件时，先在本表登记一行，再在定义点写 `**ID**` 标签。
- 新增 ID 必须先登记，再出现在注释、文档或标识符中；校验器会拒绝未登记 ID。
- 内容搬家时**只更新本表「定义文件」列**，所有引用本身零改动。

共 85 个 ID，按前缀分组：`C-*`（部署约束）· `NG-*`（非目标）· `MOD-*`（模块）·
`FLOW-*`（数据流）· `REQ-*`（需求）· `SAF-*`（安全）· `NFR-*`（非功能）· `GATE-*`（门禁）·
`BOUND-*`（边界）· `ARCH-*`（架构）。

| ID | 定义文件 | 一句话 | 类别 |
|---|---|---|---|
| `C-DOCKER` | docs/deployment.md §0 | 必须 Docker 部署 | 部署约束 |
| `C-DEBIAN-SLIM` | docs/deployment.md §0 | Debian slim，禁 Alpine | 部署约束 |
| `C-NO-SECRET-IN-IMAGE` | docs/deployment.md §0 | secrets 不进镜像 | 部署约束 |
| `C-RUSTLS` | docs/deployment.md §0 | rustls + native-roots | 部署约束 |
| `C-HTTPS-INBOUND` | docs/deployment.md §0 | HTTPS-only 入站，容器内明文 HTTP | 部署约束 |
| `C-HTTPS-URL` | docs/deployment.md §0 | 公网 HTTPS URL 由平台提供（bot 不持证书） | 部署约束 |
| `C-AUTH-APP-BASIC` | docs/deployment.md §9 / docs/design.md §3.1 | Stalwart 认证 = App Password + Basic | 部署约束 |
| `C-NO-TCP-EXPOSE` | docs/deployment.md §0 | 单监听 `PORT`，不暴露附加 TCP 端口 | 部署约束 |
| `C-NO-LONG-CONN` | docs/deployment.md §0 | 无 SSE/WS/长轮询等长连接 | 部署约束 |
| `C-REDIS-ONLY-STATE` | docs/deployment.md §0 | 状态仅外部 Redis，不用 SQLite/本地卷 | 部署约束 |
| `C-REDIS-MANAGED-AOF` | docs/deployment.md §0 | Redis 用户托管 + 开启 AOF 持久化 | 部署约束 |
| `C-PORT` | docs/deployment.md §0 | 通用 PORT 约定 | 部署约束 |
| `NG-SERVER-MODE` | docs/deployment.md §1 | `RUN_MODE=server` 常驻，非目标（该变量已随 `Config::from_env()` 删除，代码中已无此标识符） | 非目标 |
| `NG-POLLING-SSE` | docs/deployment.md §1 | EventSource/SSE 长连接，非目标 | 非目标 |
| `NG-LONG-POLLING` | docs/deployment.md §1 | Telegram 长轮询，非目标 | 非目标 |
| `NG-SQLITE-PERSIST` | docs/deployment.md §1 | SQLite 持久化，非目标 | 非目标 |
| `NG-LOCAL-VOLUME` | docs/deployment.md §1 | 本地卷持久化，非目标 | 非目标 |
| `NG-SERVERLESS-BIND` | docs/deployment.md §1 | 绑定具体 serverless 平台，非目标 | 非目标 |
| `MOD-DEDUP` | docs/deployment.md §6 | Redis `SET NX` 幂等键 | 组件 |
| `MOD-STREAMS` | docs/deployment.md §6 | Redis Streams 队列 + worker | 组件 |
| `MOD-SINCESTATE` | docs/deployment.md §6 | sinceState 游标（存 Redis） | 组件 |
| `FLOW-NEW-MAIL` | docs/design.md §5.4 / docs/deployment.md §6 | Push 新邮件流 | 数据流 |
| `FLOW-RECONCILE` | docs/deployment.md §6 | 外部 Cron 对账补差 + Redis 丢失恢复 | 数据流 |
| `REQ-AI-CONFIRM` | docs/design.md §12 | AI 仅在明确要求+确认后接触正文 | 需求 |
| `REQ-VIEW-DIRECT` | docs/design.md §12 | 查看原文始终 JMAP 直取 | 需求 |
| `REQ-LONG-EMAIL` | docs/design.md §12 | 长邮件禁止发全文，AI 摘要 ~300 字 | 需求 |
| `REQ-ANALYSIS-EPHEMERAL` | docs/design.md §12 | 分析/摘要结果不持久化 | 需求 |
| `REQ-ATTACH-ONDEMAND` | docs/design.md §12 | 附件按需拉取 | 需求 |
| `REQ-AI-FUSE` | docs/design.md §12 | AI 失败约 3 次 → 熔断 + 用户确认回退 | 需求 |
| `REQ-LLM-OPENAI-COMPAT` | docs/design.md §12 | OpenAI-compatible 环境变量 | 需求 |
| `REQ-AI-EXTERNAL-CONSENT` | docs/design.md §12 / §3 | 仅用户明确允许后才向外部 AI 发正文 | 需求 |
| `REQ-AI-CONSENT` | docs/design.md §12.3.1 | AI 授权期限与 Redis 短期 TTL（授权有效期、到期后重新询问） | 需求 |
| `REQ-SINGLE-ACCOUNT` | docs/design.md §3.1/§11.3 | 单账户；多账户 = 多 bot 实例 | 需求 |
| `REQ-PUSH-TYPES` | src/domain/jmap/client.rs:110 注释 | `PushSubscription/set` create 在 jmap-client 0.4.2 中没有 `types` 参数；订阅 id 对外暴露前须经 `push_subscription_update_types` 收窄为 `Email` + `EmailDelivery` | 需求 |
| `REQ-RECONCILE-IDEMPOTENCY` | src/state.rs `claim_dedup` + `get_reconcile_state` / docs/design.md §8.2 | JMAP 对账游标只有在全部分页事件成功入队（XADD）后才推进；单次对账由 Redis SET NX EX 锁 `lock:reconcile` 保证单飞（TTL 300s，owner token 续期 90s，仅持有者可续期/释放）；处理端再经 `claim_dedup`（SET NX EX，86400s）保证同一流消息不重复投递 | 需求 |
| `REQ-TIMEZONE-DISPLAY` | src/config.rs `SUPPORTED_TIMEZONES` / docs/reference.md §5.4 | 通知的收件时间按业务配置 `timezone`（IANA，默认 `Asia/Shanghai`）渲染为 `%Y-%m-%d %H:%M`；仅接受 16 个无夏令时区域，未匹配返回 422，不做时区库推断 | 需求 |
| `NFR-NOTIFY-SLA` | docs/deployment.md §6.5 | 通知可用性 ≥99.9%，允许少量延迟 | 非功能 |
| `NFR-RECONCILE-INTERVAL` | docs/deployment.md §6.3 | 外部 Cron 对账间隔 5–10 分钟 | 非功能 |
| `SAF-NOTIFY-META` | §3 | 新邮件通知只含元数据，正文不入通知 | 安全 |
| `SAF-CHAT-ALLOWLIST` | §3 / docs/design.md §7.3 | CHAT_ALLOWLIST 硬约束，处理前先拒绝非白名单 | 安全 |
| `SAF-AUTH-RECONCILE` | §3 / docs/design.md §7.3 | `/reconcile` 需 `Authorization: Bearer RECONCILE_TOKEN`，fail-closed | 安全 |
| `SAF-AUTH-TG-WEBHOOK` | §3 / docs/design.md §7.3 | `/webhook/tg` 需头 `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET` | 安全 |
| `SAF-AUTH-JMAP-PUSH` | §3 / docs/design.md §7.3 | `/push/jmap` 按 `pushSubscriptionId` 查 Redis 短期验证状态（存 `session_digest` 摘要），缺失或摘要不符拒绝；状态缺失回落 worker 重新验证（fail-closed） | 安全 |
| `SAF-ADMIN-SESSION` | src/config.rs `session_digest` / §3 | 返回用于 Redis admin-session 记录的 SHA-256 摘要；bearer token 本体绝不写入 Redis | 安全 |
| `SAF-NO-SECRET-ECHO` | src/config.rs `SecretString` 字段与公共配置 API | 密钥字段不实现 `Debug` 且永不进入公共配置 API 响应；Redis 线格式类型私有，不得序列化进 HTTP 响应 | 安全 |
| `SAF-PROBE-PUBLIC` | §3 / docs/design.md §7.3 | `/healthz`、`/ready` 公开探针：无鉴权、无敏感信息 | 安全 |
| `ARCH-HEALTHZ` | docs/design.md §7.3/§10.0 | `/healthz` liveness（进程存活），语义长期稳定 | 架构 |
| `ARCH-READY-BASELINE` | docs/design.md §7.3 / src/notify.rs `ready`+`probe_jmap_session`+`probe_telegram_get_me` | `/ready` 端到端就绪：配置完整性 + Redis 可达性 + **出站只读探测**（JMAP session `GET`、Telegram `getMe`，各 `PROBE_TIMEOUT`=3000ms、并行，最坏约 3s），四者全过 `200`（就绪报告 JSON 含真实 `jmap`/`telegram` 字段），任一失败 `503`（标准错误 envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`）；探针只读、无状态写入（`refresh_business_config` 仅读 Redis），bot token 仅用于拼 URL | 架构 |
| `GATE-P0` | docs/design.md §10.0 | 阶段0 P0 门禁（fmt/clippy/test 过 Debian 容器等 8 项） | 流程 |
| `BOUND-STAGE1` | docs/design.md §10.0 | 阶段1 推进边界（过 GATE-P0 才进；R1 入口鉴权已在阶段0 落地） | 流程 |
| `ARCH-CONFIG-ENV` | docs/design.md §7.1 | 配置=环境变量手工解析，无 figment/TOML | 架构 |
| `ARCH-AXUM-08` | docs/design.md §10.0 | 单端口 axum 0.8 入口（版本以 Cargo.toml 为准） | 架构 |
| `ARCH-STAGE0` | docs/design.md §10.0 / §5.3 | 阶段0 实际交付与现状 | 架构 |
| `ARCH-DEPS-STAGE0` | docs/design.md §10.0 / §4 | 阶段0 实际依赖集（axum/serde/secrecy/subtle/tokio/tracing…） | 架构 |
| `ARCH-DEPS-STAGE1` | docs/design.md §10.0-1 / §4 | 阶段1 依赖现况：`jmap-client 0.4.2` 已引入，版本/features 以 Cargo.toml 为准，禁用 WebSocket feature | 架构 |
| `ARCH-DEPS-STAGE4` | docs/design.md §10.0-1 / §4 | 阶段3.5/4 后依赖现况：`redis 0.27`、`reqwest 0.13` 已引入并实际使用；`teloxide` **未**引入（Telegram 由 `src/channel.rs` 用 reqwest 自研实现）；版本一律以 `Cargo.toml` 为准 | 架构 |
| `MOD-JMAP-CLIENT` | docs/design.md §6-7 / §4 | `domain::jmap::client` 真实只读 adapter（session/account/mailbox/email 只读） | 组件 |
| `MOD-TELEGRAM-NOTIFY` | src/worker.rs / docs/design.md §12 | 有界元数据通知 worker：消费出站队列，把脱敏通知经 Telegram 投递（不承载正文/AI 响应） | 组件 |
| `REQ-JMAP-SESSION-URL` | docs/design.md §7.1 / .env.example | `JMAP_SESSION_URL` 接受服务基地址或完整 /.well-known/jmap，归一化为 origin/base 后再交 jmap-client（无重复路径）；**代码已实现（D-G1-1），已在真实账号真机验证**（2026-09-28：`fetchChanges` 持久化 `baseline:` 游标、`Email/query` 搜索回帖，均经用户确认） | 需求 |
| `SAF-JMAP-URL` | docs/design.md §7.1 / §4 | JMAP URL 约束：仅 HTTPS、禁止内嵌凭据、拒绝危险 query | 安全 |
| `REQ-JMAP-RAW-MULTIPART` | docs/design.md §3.2/§10.1 | `read_email` 多 part 原文：按 text_body 顺序拼接"有 part_id 且 bodyValue"的部分；无可用部分→明确错误 | 需求 |
| `GATE-G1-JMAP-READONLY` | docs/design.md §10.1 / §6 | G1 门禁：只读 adapter **代码已实现**（mock + `#[ignore]` 真机测试），**待真实 `cargo test -- --ignored jmap::` 验证** | 流程 |
| `ARCH-LB-WORKER` | docs/deployment.md §10 | 多实例 LB/HA：免费 Cloudflare Worker 作唯一对外入口 + 故障转移，后端为多平台同镜像 | 架构 |
| `C-LB-SINGLE-REG-URL` | docs/deployment.md §10.1 | Telegram/Push/Cron 只登记 Worker 的稳定 URL；后端平台入口不对外登记 | 约束 |
| `C-LB-SHARED-SECRETS` | docs/deployment.md §10.3 | 多实例必须共享同一组 `SAF-AUTH-*` secret，否则随机 401 | 约束 |
| `SAF-LB-PASSTHRU` | docs/deployment.md §10.3 / §3 | 信任模型=透传：Worker 不改写鉴权；后端必须继续 fail-closed 校验（后端可能被公网直连） | 安全 |
| `SAF-RECONCILE-LOCK` | docs/deployment.md §10.5 | `/reconcile` 不扇出，Redis 锁保证单实例执行，避免重复对账 | 安全 |
| `MOD-STREAMS-GROUP` | docs/deployment.md §10.5 | 多实例用同一 Streams 消费组名，Redis 自动分摊（at-least-once 不重复处理） | 组件 |
| `MOD-HEALTH-AGG` | docs/deployment.md §10.6 | Worker 聚合健康视图，报告各后端存活供外部监控 | 组件 |
| `MOD-DEBUG` | src/debug.rs / docs/deployment.md §2.1 / docs/design.md §7.6 / docs/reference.md §3 | 远程联调只读表面：`DEBUG_ENABLED`（或 `--debug`）+ `DEBUG_TOKEN` 双因子开启后挂载 `/debug/*`，否则不挂载 | 组件 |
| `SAF-DEBUG-GATE` | src/main.rs / src/debug.rs / docs/design.md §7.6 / docs/deployment.md §2.1 | 双因子门禁：「`DEBUG_ENABLED` 为真值或命令行带 `--debug`」**且** `DEBUG_TOKEN` 非空才挂载路由；缺任一完全不挂载（请求落通用 `404`），默认绝对关闭。开启信号走 env 而非 argv，使启动命令保持静态、开关可在平台控制台单点切换 | 安全 |
| `SAF-DEBUG-AUTH` | src/debug.rs / docs/deployment.md §2.1 | 挂载后 `/debug/*` 须 `Authorization: Bearer DEBUG_TOKEN` 常数时间比较，失败 `401` 且无副作用 | 安全 |
| `REQ-DEBUG-ENDPOINTS` | src/debug.rs / docs/reference.md §3 / docs/deployment.md §2.1 | 端点契约：`GET /debug/ping`、`/config`、`/redis`、`/jmap`、`/telegram`、`/worker` 均只读；`POST /debug/notify` 走真实出站链路发一条测试消息；响应体不含 secret 原文（凭据字段只出 `*_configured` 布尔，非密文的身份与预算字段仍明文返回） | 需求 |
| `SAF-DEBUG-ORIGIN-ONLY` | docs/deployment.md §2.1 / docs/reference.md §4 | `/debug/*` 不在网关 16 条安全路由内，Worker 一律 `404 route not forwarded`；只能直连后端 origin，公网不可达 | 安全 |
| `SAF-DEBUG-ALLOWLIST` | src/debug.rs / docs/deployment.md §2.1 | `POST /debug/notify` 仅在 chat 白名单**非空**时校验 `chat_id`；白名单未配置（空）时不拦截，故启用本面须确认业务白名单已配置 | 安全 |
| `NFR-HA-MULTI-INSTANCE` | docs/deployment.md §10.7 / §9.1 | 多实例高可用语义；双活或主备均可；Redis 单点故障不在方案范围（用户外部解决） | 非功能 |
| `C-NO-DB` | docs/deployment.md §0 / §9.1 / §3 | 生产不使用任何数据库（无 SQLite/Postgres/MySQL/嵌入式），Redis 为唯一状态存储；应用不连接第二个数据库 | 约束 |
| `C-NO-LOCAL-WRITE` | docs/deployment.md §0 / §9.1 / §3 | 禁止本地文件/目录写入（日志/数据/临时缓存/本地卷） | 约束 |
| `C-LOG-STDOUT-ONLY` | docs/deployment.md §0 / §9.1 / §3 | 日志只写 stdout/stderr，由平台采集；禁用文件日志后端 | 约束 |
| `SAF-LOG-PURITY` | docs/deployment.md §0 / §9.1 / §3 | 日志与 Redis 写入内容仅限结构化事件/计数/时间戳/脱敏摘要；禁止密钥/邮件正文/AI 请求响应/附件内容 | 安全 |
| `SAF-ENABLE-FLAG` | src/notify.rs `put_enabled` / src/state.rs `config:enabled` | 全局开关是 Redis 单键 `config:enabled`；未写入即视为关闭，`business_enabled` 出错也按关闭处理（fail-closed）；写入须 admin-session Bearer | 安全 |
| `C-NO-STATEFUL-RECOVERY` | docs/deployment.md §0 / §9.1 / §3 | 禁止依赖进程内状态做生产恢复；恢复一律走 Redis + JMAP 对账；进程内缓存仅为性能优化，丢失须安全可重入 | 约束 |
| `ARCH-STATE-REDIS` | docs/design.md §10.0 / Redis 为唯一状态来源 | 状态层统一走 Redis（Streams/SET NX/锁/摘要），进程不持有可恢复状态 | 架构 |
| `C-REDIS-EXTERNAL` | docs/deployment.md §8.1 | Redis 由外部已认证实例提供，不与本服务同容器 | 部署约束 |
| `GATE-UPTIME-KUMA` | docs/deployment.md §10.1 | `/healthz` 稳定语义可直接接 Uptime Kuma 等外部探针 | 门禁 |
| `GATE-DOCS` | scripts/docs_check/run_all.sh / docs/deployment.md | 文档门禁：校验器全部 0 error、exit 0 才算通过 | 门禁 |
