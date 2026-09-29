# MessageWeave Cloudflare Worker（ARCH-LB-WORKER）

统一 HTTPS 入口 + 多后端 origin 故障转移（HA/LB 子项目）。
**透传模型**：Worker 不感知业务，原样转发请求到多个 https 后端 origin；仅「超时 / 5xx」做有界故障转移。

> 本组件是 **safelist 受限的边缘负载均衡器（edge load balancer）**：路由固定 15 条白名单、
> 后端 origin 在部署期固定且仅允许 https、未知路径一律 404。它只对固定后端做请求转发与
> 故障转移，不接受任意目标主机，也不提供任何形式的流量中转或访问隐藏能力。

> 设计依据：`docs/design.md §10 / NFR-HA-MULTI-INSTANCE`、`docs/deployment.md §10`。
> 安全基线与禁止事项：`docs/charter.md §3/§5`（稳定 ID 注册表 §8）。

## 目录

- `wrangler.toml` — Wrangler 示例配置（name/main/compat/vars；后端 secret 走 `wrangler secret`）
- `src/index.js` — Worker 入口（路由分发、/healthz 聚合、safelist 校验）
- `src/backends.js` — origin 解析 + https/凭据/query 校验（C-HTTPS-INBOUND）
- `src/lb.js` — 透传转发 + 有界故障转移（SAF-LB-PASSTHRU）
- `src/health.js` — 健康聚合探针（MOD-HEALTH-AGG / C-NO-DB）
- `test/*.test.js` — 单元/集成测试（零依赖，`node --test`）
- `package.json` — npm 元数据（`dev`/`deploy` 调 wrangler；`test` 纯 node）

## 零依赖设计

运行时仅依赖 ES2022 标准 + 平台/Node 的 `fetch`/`Headers`/`Request`/`Response`/`URL`。
`npm run test` 直接 `node --test test/`，**无需安装**任何生产依赖；
`wrangler` 仅在 `check`/`deploy`/`dev` 中作 devDependency（CI 可选装）。

## 语义

### 透传（A，SAF-LB-PASSTHRU）
- 原样转发 method / headers（含鉴权头）/ body；不鉴权改写。
- 所有后端实例共享同一组 secret（`C-LB-SHARED-SECRETS`）：同一 `TELEGRAM_WEBHOOK_SECRET`、
  `JMAP_PUSH_VERIFICATION_CODE`、`API_TOKEN`（`API_TOKEN` 在 `docs/deployment.md` §10 仅用于多实例互斥，非鉴权）
  + 相同 App Password（各后端实例的 `JMAP` 可不同，`docs/deployment.md` §10.2）。

### 路由 safelist（ARCH-LB-WORKER / C-LB-SINGLE-REG-URL）
- 透传公开 `GET /api/status` 启动状态，以及管理 SPA API：`POST /api/admin/session[/revoke]`、`GET|PUT /api/enabled`（业务总开关，SPA 服务卡片读写）、`GET|PUT /api/config`、`PUT /api/business-config`。启动状态只包含缺少的环境变量名称；管理 API 的鉴权仍由后端执行。
- 只透传 `POST /webhook/tg`、`POST /push/jmap`、`POST /api/push/register`、`POST /api/push/disable`、`POST /reconcile`、`GET /ready`。
- 其它路径 → **404**（不透传，避免 Worker 沦为后端任意路径的跳板）。
- method 不符 → **405**。

### 有界故障转移（`docs/deployment.md` §10.4）
- 每次请求只试 `min(LB_MAX_ATTEMPTS, origins.length)` 次；默认 2（首次 + 1 次换实例）。
- **仅** 超时（AbortError）或 5xx 触发换下一个实例；4xx / 2xx / 3xx 直接返回。
- 起点按 `rng` 随机化（默认 `Math.random`），实现双活分摊；单 origin 配置时退化为确定性。
- 全失败 → **503 All Backends Unavailable**（交由 Telegram / Stalwart 自动重投兜底，不丢消息）。
- 健康缓存（`aggregateHealth` / `LB_HEALTH_TTL_MS`）仅服务于 `/healthz` 聚合视图；转发目前**不做健康路由过滤**（`docs/deployment.md` §10.4「只向健康实例转发」为后续增强，当前靠故障转移兜底）。
- **例外：`POST /reconcile` 走 per-route 覆盖。** 它是后端同步长任务——后端持集群级锁 `lock:reconcile`（初租 300s，期间由心跳续期），全局 10s 超时会把它误判为失败并故障转移到第二实例，而第二实例必然立刻返回 409。所以该路由改为：超时取 `LB_RECONCILE_TIMEOUT_MS`（默认 `320000`，留足锁初租 + 心跳余量），且 `maxAttempts=1` 绝不故障转移。其余快路径保持全局默认。

### 健康聚合（MOD-HEALTH-AGG；C-NO-DB / C-REDIS-ONLY-STATE）
- `GET /healthz`（由 Worker 自身承载）：按 TTL 缓存（默认 30s）探测各后端 `/healthz`，
  返回 `{status: ok|down, available, total, backends:[{origin, up, status}]}`；
  有 ≥1 后端 up → 200；全部 down → 503。
- **不代理 Redis/JMAP**：Worker 不做数据库侧检查；后端端到端就绪（配置完整性 + Redis 可达 + JMAP session + Telegram getMe，对应 `ARCH-READY-BASELINE`）由 `/ready`（透传）承担。

### 无长连接 / 无密钥日志（§3/§5 红线）
- 不使用 WebSocket/SSE/长轮询（`C-NO-LONG-CONN`）：纯请求-响应转发，body 一次性 `arrayBuffer` 回灌。
- 日志只打「方法 / 路径 / origin / 失败类别」；**绝不**打印 header / body / 鉴权 secret / App Password（`SAF-LOG-PURITY`）。

## 环境变量

| 变量 | 必填 | 说明 |
|---|---|---|
| `BACKEND_ORIGINS_JSON` | 是（secret） | JSON 数组，形如 `["https://a.platform1.example","https://b.platform2.example"]`；必须全部 https、无内嵌凭据/query/fragment/路径。 |
| `LB_REQUEST_TIMEOUT_MS` | 否 | 单 origin 请求超时；默认 `10000`。建议 > 最坏 cold start + 最长 JMAP 拉取。 |
| `LB_MAX_ATTEMPTS` | 否 | 每请求最多 origin 尝试次数；默认 `2`（= 1 次故障转移）。`POST /reconcile` 固定为 `1`。 |
| `LB_RECONCILE_TIMEOUT_MS` | 否 | 仅 `POST /reconcile` 的单 origin 超时覆盖；默认 `320000`（须大于后端锁初租 300s + 心跳余量）。 |
| `LB_HEALTH_TTL_MS` | 否 | 健康探测缓存 TTL；默认 `30000`。 |

> secret 通过 `wrangler secret put BACKEND_ORIGINS_JSON` 注入；`wrangler.toml` 里 `vars` 保持为空，不写明文。

## 本地 / CI 验证

```bash
cd cloudflare-worker

# 单元/集成测试（零依赖）：
npm run test
# 等价：node --test test/

# 语法检查 + wrangler dry-run（需 npx wrangler，会拉 devDependency）：
npm run check
# 等价：node --check src/*.js && node --check test/*.test.js && wrangler deploy --dry-run
```

## 部署示例

```bash
cd cloudflare-worker
# 安装 dev dep（仅 wrangler）：
npm install
# 注入 secret：
npx wrangler secret put BACKEND_ORIGINS_JSON   # 粘贴 ["https://a.example","https://b.example"]
# 可选调参：
npx wrangler secret put LB_REQUEST_TIMEOUT_MS   # 建议 10000
npx wrangler secret put LB_MAX_ATTEMPTS         # 默认 2
# 部署：
npx wrangler deploy
```

健康探针：

```bash
curl https://lb.messageweave.example/healthz
# => {"status":"ok","available":2,"total":2,"backends":[{"origin":"https://a.example","up":true,"status":200},...]}
```
