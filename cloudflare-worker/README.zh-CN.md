# MessageWeave Cloudflare Worker 网关

> [English version / 英文版 → README.md](README.md)

面向一个或多个 MessageWeave 后端的统一 HTTPS 入口，带**有界多 origin 故障转移**（HA/LB 子项目）。**透传模型**：Worker 不解析业务载荷，把请求原样转发给后端 https origin；只有*超时 / 5xx* 才触发有界的故障转移到另一个实例。

> 本组件是**针对 MessageWeave 需要放在同一个 URL 后面的一组固定路由的负载均衡器**：路由集合固定（19 个路径）、后端 origin 在部署时固定且仅允许 https、任何未知路径一律 `404`。它只把这些固定后端之间的请求做转发与故障转移，不接受任意目标主机，也不提供通用的流量中转能力。
>
> **它的用途是负载均衡，不是防护。** Cloudflare 的 Load Balancer 产品在免费套餐上不可用，所以这是免费套餐账户在它的那些 MessageWeave 后端前面做多 origin 负载分摊与故障转移的办法。这就是它存在的唯一理由，网关是可选的：后端有它和没有它跑起来完全一样。
>
> **它不是安全特性。** 这里没有隐藏后端的意图，也没有加固后端的意图。网关在不在，origin 都同样可以直接访问，边缘上也没有加任何访问控制，所以路由白名单不是防火墙，本文档任何一处都不该被读成防护。

> 设计依据：`docs/design.md §11.3`（`NFR-HA-MULTI-INSTANCE`）与 `docs/deployment.md §10`。
> 安全基线与禁令：`docs/charter.md §3`、`docs/charter.md §5`；稳定 ID 注册表 `docs/charter.md §8`。

## 1. 它是什么，不是什么

- **只转发。** method、headers（含认证头）、body 一律原样转发。Worker 自身不做任何认证，也不改写任何内容。
- **无 secret。** 没有 Telegram bot token，没有 JMAP 密码，没有 session secret。它收到什么认证头就转发什么，自己一个都不校验。真正共享的是后端之间必须一致的同一组 `SAF-AUTH-*` secret——`RECONCILE_TOKEN`、`TG_WEBHOOK_SECRET` 与加密后的业务配置——因为回调可能落在任何一个实例上，而任何实例都证明不了自己是被点名的那一个（`C-LB-SHARED-SECRETS`）。凭据不一致的表现是随机 401，而不是路由错误。
- **不是安全层。** 它是负载均衡器，不是访问控制边界。后端 origin 有没有它都同样直连可达，这里既没有隐藏也没有加固它们。白名单对未知路径的 fail-closed 应答，是让负载均衡器在一组固定路径上定义明确的手段，不是可以依赖的防护。
- **不做 origin 发现。** origin 只来自部署时的 origin 列表。
- **零运行时依赖。** 纯 ES2022 加平台提供的 `fetch`、`Headers`、`Request`、`Response`、`URL`。`wrangler` 是 devDependency，只用于 `check`、`deploy`、`dev`。
- **零状态。** 不连 Redis、不连 JMAP、不连数据库（`C-NO-DB`、`C-REDIS-ONLY-STATE`）。唯一的可变状态是 isolate 内的健康探测缓存。
- **无长连接**（`C-NO-LONG-CONN`）。纯请求-响应。请求体只读一次为 `arrayBuffer`，每次尝试重新挂上去，因此不需要流式。
- **不是队列。** 除了下面那几轮有界尝试，不做任何重试。所有 origin 都失败时网关返回 `503`，靠 Telegram / Stalwart 的自身重投兜底。

## 2. 目录结构

| 文件 | 职责 |
|---|---|
| `wrangler.toml` | `name`、`main`、compatibility flags、`[vars]` |
| `package.json` | 只有 `check` / `test` / `deploy` / `dev` 脚本，无运行时依赖 |
| `src/index.js` | 入口：HTTPS 重定向、路由分发、白名单、健康聚合 |
| `src/backends.js` | origin 解析与校验、路由白名单 |
| `src/lb.js` | 透传转发与有界故障转移 |
| `src/health.js` | 带 TTL 的健康聚合探测 |
| `test/*.test.js` | 四个文件共 38 条测试，全部离线 |

## 3. 透传语义

- method、headers、body 原样发出；后端响应的 status、body、headers 原样返回。
- 网关唯一新增的 header 是尽力而为的观测头：`x-lb-backend`，标记应答的 origin。写入放在 `try/catch` 里。跨域且不带 `access-control-allow-*` 的响应，其 header guard 是 immutable，`headers.set` 会抛异常。一次会抛异常的观测写入绝不能把健康的 `2xx` 掩盖成 `503`，所以写不进去时静默放弃该 header。这个缺陷就是 2026-09-30 的生产事故根因。
- `GET` 和 `HEAD` 不带 body 转发：上游没有 body 的响应直接原样返回，不重算 `Content-Length`。
- 后端的 `3xx` 由网关自己跟随（`redirect: "follow"`），跳转型的 origin 不会把第二跳甩给浏览器。
- 日志与错误串只包含 method、path、origin、失败类别，绝不含 headers、body、认证 secret 或 App Password（`SAF-LOG-PURITY`）。

## 4. 路由白名单

所有入站 **HTTP** 请求先被永久 **308** 重定向到同一个 HTTPS URL，之后才做路由校验与转发（`C-HTTPS-INBOUND`）。此后请求必须命中 19 个路径之一且 method 匹配，其余组合一律在边缘拒绝。

| 路径 | 允许的 method |
|---|---|
| `/` | GET |
| `/assets/config.js` | GET |
| `/assets/styles.css` | GET |
| `/api/status` | GET |
| `/api/config` | GET PUT |
| `/api/business-config` | GET PUT |
| `/api/business-config/preflight` | POST |
| `/api/admin/session` | POST |
| `/api/admin/session/revoke` | POST |
| `/api/enabled` | GET PUT |
| `/webhook/tg` | POST |
| `/push/jmap` | POST |
| `/api/push/register` | POST |
| `/api/telegram/register-webhook` | POST |
| `/api/push/disable` | POST |
| `/reconcile` | POST |
| `/worker` | POST |
| `/healthz` | GET |
| `/ready` | GET |

- 未知路径：`404`，body 为 `route not forwarded: <path>`。因此网关永远不可能被当作跳板去访问后端的任意路径。
- 路径已知但 method 不匹配：`405`，body 为 `method not allowed for <path>`，并附带 `Allow` header。
- 引导接口（`POST /api/bootstrap`）与远程诊断面（`/debug/*`）刻意不在白名单内，两者都只保留在 origin 直连。
- `/healthz-worker` 完全不在白名单里：它由网关自己应答，且只接受 `GET`。

## 5. 超时与重试预算

| 变量 | 必填 | 默认值 | 适用范围 |
|---|---|---|---|
| `BACKEND_ORIGINS_JSON` | 是 | — | 所有路由 |
| `LB_REQUEST_TIMEOUT_MS` | 否 | `10000` | 除下面两条外的所有路由，以及 §7.2 的探测 |
| `LB_MAX_ATTEMPTS` | 否 | `2` | 每请求的尝试次数 |
| `LB_RECONCILE_TIMEOUT_MS` | 否 | `320000` | 仅 `POST /reconcile` |
| `LB_WORKER_TIMEOUT_MS` | 否 | `300000` | 仅 `POST /worker` |
| `LB_HEALTH_TTL_MS` | 否 | `30000` | 仅 `/healthz-worker` 探测缓存 |
| `LB_VERSION` | 否 | `unknown` | `/healthz-worker` 的 `version` 字段 |

整数型 env 值用 `Math.floor` 解析。非有限数或非正值会被忽略并回落到默认值，所以手滑不会把预算关掉。

`/reconcile` 与 `/worker` 是**单次尝试路由**，各自有独立的超时覆盖。两者都是后端耗时的同步长任务：按全局 10 s 超时，网关会判定它们失败并故障转移到第二个实例，但第二个实例对 `/reconcile` 会立刻返回 `409`（集群级 `lock:reconcile` 初始租约 300 s，靠心跳续租），而第二个 `/worker` 只会把同一批消息再排一遍。所以这两条路由拿到 `LB_RECONCILE_TIMEOUT_MS`（默认 320 s，比 300 s 租约加心跳多留余量）与 `LB_WORKER_TIMEOUT_MS`（默认 300 s，等于后端单条事件地板 `SINGLE_EVENT_CEILING_FLOOR_MS`），并且都是 `max_attempts = 1`。

如果 `/worker` 的批大小上调，`LB_WORKER_TIMEOUT_MS` 必须跟着加：后端批量的默认值与硬上限都是 10。

## 6. 有界故障转移

- 每个请求尝试 `max(1, min(LB_MAX_ATTEMPTS, origins.length))` 个 origin。默认 2 即首次加一次故障转移。
- 只有超时或 `5xx` 才会换下一个 origin。`2xx`、`3xx`、`4xx` 都是应答，立即原样返回。
- 超时与网络层失败被判为可重试的条件：错误 name 是 `AbortError`，或消息、或 `cause` 链上更深的错误消息命中 `fetch failed`、`network error`、`socket hang up`、`ECONNREFUSED`、`ECONNRESET`、`EAI_AGAIN`、`ENOTFOUND`、`ETIMEDOUT`、`EHOSTUNREACH`、`ENETUNREACH`。其余一律 fail-fast。
- 当 origin 大于 1 且尝试次数大于 1 时，起点 origin 由 `rng` 决定（默认 `Math.random`），把负载摊到健康实例上；单个 origin 退化为固定顺序。
- 所有 origin 都失败：`503`，status text 为 `All Backends Unavailable`，body 为 `all backends failed: <last reason>`。随后由 Telegram 与 Stalwart 重投，消息因此不丢。

健康缓存只服务于 `/healthz-worker` 的聚合视图。转发路径**不**按健康过滤；现阶段故障转移就是安全网。

## 7. 健康接口

两个接口刻意不重叠，谁都不能掩盖谁。

### 7.1 GET /healthz — 透传

与其他白名单路由一样被转发。响应是后端自己的 liveness envelope，这也正是它在另一个 origin 已挂时依然显示健康的原因。

### 7.2 GET /healthz-worker — 网关聚合

由网关自己应答，完全不接触被转发的路由集合。

- 用与转发同源的请求超时（`LB_REQUEST_TIMEOUT_MS`，默认 10 s）探测每个 origin 的 `/healthz`。重定向会被跟随，所以返回 `3xx` 的 origin 同样计为 up。
- origin 应答状态码低于 500 即为 `up`。探测失败记 `up: false` 且 `status: null`。
- 结果按 origin 缓存 `LB_HEALTH_TTL_MS`（默认 30 s）在 isolate 内的 map 中，用来控制探测流量。未命中就探测并写入；未过期的缓存直接返回，不再探测。
- 响应体：`{status, version, available, total, backends: [{origin, up, status}]}`。
  - 至少一个 origin 在线：HTTP `200`，`status: "ok"`
  - 全部 origin 离线：HTTP `503`，`status: "down"`
  - 未配置 origin：HTTP `200`，`status: "no-backends"`
  - `version` 即 `LB_VERSION`，未配置时为字面量 `unknown`
- 任何非 `GET` 的 method：`405`，并带 `Allow: GET`。
- 网关不代理 Redis 或 JMAP 的 readiness。端到端后端就绪判定（`ARCH-READY-BASELINE`：配置完整、Redis 可达、JMAP session、Telegram `getMe`）留在后端的 `/ready` 上，`/ready` 本身被透传。

## 8. 回调注册

SPA 的回调 URL 只注册**一个** origin（`C-LB-SINGLE-REG-URL`）。只填 origin，不带路径：

- 启用网关：填 Worker URL，例如 `https://lb.example`。
- 不启用网关：填后端 origin，例如 `https://a.example`。

你注册的是哪个 origin，`/webhook/tg` 与 `/push/jmap` 就落在哪。三个注册接口都在白名单内、都会经过网关透传，所以两种前门都能注册。注册是幂等的，且注册新地址会注销旧订阅——换前门就靠这一步。

## 9. 配置

- 默认值写在 `wrangler.toml` 的 `[vars]` 里，这是一个**刻意的明文例外**：当前取值只是公开可达的 https origin，不是凭据。一旦出现内网地址、带内嵌凭据的 URL、或不想公开的主机名，立刻改用 `npx wrangler secret put BACKEND_ORIGINS_JSON` 并删掉 `[vars]` 里那一行。git 历史不可逆，删掉并不等于没进过库。
- `BACKEND_ORIGINS_JSON` 是**纯字符串数组**的 JSON。`{"url": "..."}` 这类对象形式会被拒绝：`parseBackendOrigins` 抛异常，而配置是**每个请求**重新解析的，所以表现是**每个请求都 503**，而不是启动即失败。body 为 `misconfigured backends`；空列表同样走这个异常路径，在转发任何请求之前就被拦下。
- 每一项必须是 https **origin**：不允许 http、不允许 path、不允许 query、不允许 fragment、不允许内嵌凭据（`C-HTTPS-INBOUND`）。
- `wrangler.toml` 同时固定 `name = "messageweave-lb"`、`main = "src/index.js"`、`compatibility_date = "2025-01-01"` 与 `compatibility_flags = ["nodejs_compat_v2"]`。`nodejs_compat_v2` 的作用是让测试文件能 import `node:test` 与 `node:assert/strict`；运行时代码不引入任何 node 模块。
- `wrangler.toml` 的注释必须保持单行 `#` 注释。TOML 没有块注释，用 JSDoc 的 `/** */` 块会让 wrangler 直接解析不了整个配置文件。

## 10. 本地与 CI 验证

```bash
cd cloudflare-worker

npm test       # 四个文件共 38 条测试，全部离线
npm run check  # 对源码与测试跑 node --check，再 wrangler deploy --dry-run
```

`npm test` 即 `node --test`，会自动收集 `test/*.test.js`：`test/index.test.js` 15、`test/lb.test.js` 12、`test/backends.test.js` 6、`test/health.test.js` 5。跑测试不需要安装依赖——Worker 没有运行时依赖，测试文件只引入 `node:test` 与 `node:assert/strict`。

两个坑：

- `node --test test/`（传目录）在某些 Node 版本上会跑完**零条**测试然后退出 0。必须传文件 glob。
- 如果 CI 默认安装步骤是 `bun install`，它会忽略 npm lockfile：自行重新解析，可能把 wrangler 跨大版本移动。需要锁住 wrangler 时，在测试步骤前显式加一步 `npm ci`。

## 11. 部署

```bash
cd cloudflare-worker
npm install --no-audit --no-fund
npm run deploy
curl https://<your-worker>.workers.dev/healthz-worker
```

`npm run check` 内含 `wrangler deploy --dry-run --outdir=.build-check`，会在发布前把四个源文件打包并报告产物体积。网关由 push 触发部署，所以对 `/healthz-worker` 发一次 `curl` 并对比其 `version` 就是验收手段。改 LB 逻辑的提交要同时 bump `LB_VERSION`，这样才能判断线上是哪一次构建在应答。

两起值得记住的生产事故：

- **2026-09-30**：某次构建把 `x-lb-backend` 写进了代理响应。跨域且不带 `access-control-allow-*` 的响应 header guard 是 immutable，`headers.set` 抛异常，外层 catch 于是把一个健康的后端记成不可用并耗尽重试——网关返回 `503`，而 `GET /` 实际返回的是 `200`。`/healthz-worker` 不走那段代码，这正是事故被掩盖的原因。已改为把 header 写入做成尽力而为。
- **对象形式的 origin 列表在解析期静默失败。** `parseBackendOrigins` 抛异常后每个请求都是 `503 misconfigured backends`。`weight` 之类的 per-origin 调参不要写进列表，没有任何地方读它们。

## 12. 设计不变量

- `SAF-LB-PASSTHRU` — 透传路径不改写认证、header 或 body
- `C-HTTPS-INBOUND` — origin 仅允许 https，拒绝内嵌凭据与路径，HTTP 走 308
- `C-LB-SINGLE-REG-URL` — 只注册一个回调 origin，未知路径 404
- `C-LB-SHARED-SECRETS` — 所有后端携带同一组业务凭据
- `MOD-HEALTH-AGG` — 由网关持有的带 TTL 健康聚合探测
- `C-NO-DB`、`C-REDIS-ONLY-STATE` — 网关不访问数据库与 Redis
- `C-NO-LONG-CONN` — 仅请求-响应
- `ARCH-READY-BASELINE` — 端到端就绪判定留在后端的 `/ready`
- `SAF-LOG-PURITY` — 仅 method、path、origin、失败类别
- `ARCH-LB-WORKER` — 网关是唯一边缘组件
- `NFR-HA-MULTI-INSTANCE` — 多 origin 故障转移即 HA 的实现方式

## References

- `docs/deployment.md §10` — 网关部署、配置与上线
- `docs/reference.md §4` — 网关与后端的路由矩阵
- `docs/reference.md §9.2` — 后端配置与 secret
- `docs/reference.md §9.4` — 健康与发布检查
- `docs/design.md §11.3` — HA 与多实例设计
- `docs/charter.md §3`、`docs/charter.md §5`、`docs/charter.md §8` — 安全基线、禁令、稳定 ID 注册表
