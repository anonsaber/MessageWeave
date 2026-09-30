/**
 * Cloudflare Worker 入口 — MessageWeave 多实例 LB/HA（ARCH-LB-WORKER / deployment.md §10）。
 *
 * 稳定 ID（docs/charter.md §3 安全边界；注册表 §8）：
 * - SAF-LB-PASSTHRU：透传 headers/body（含鉴权头），不做鉴权改写。
 * - C-HTTPS-INBOUND：仅 https 后端 origin。
 * - ARCH-LB-WORKER / C-LB-SINGLE-REG-URL：safelist 路由，未知路径 404、method 不符 405。
 * - MOD-HEALTH-AGG：/healthz-worker 由 Worker 聚合；/healthz 透传到源站。
 * - C-NO-DB / C-REDIS-ONLY-STATE：不代理 Redis/JMAP；不检查数据库侧可用性。
 * - C-NO-LONG-CONN：纯请求-响应，body 一次性读入回灌；无 WS/SSE/长轮询。
 * - C-NO-SECRET-IN-IMAGE：日志仅「方法/路径/origin/失败类别」，不含 secret；origin 清单走 wrangler secret。
 *
 * 路由模型：
 * - GET /、/assets/config.js、/assets/styles.css → 内嵌 SPA 静态资源透传
 * - GET /api/status → 后端配置引导状态；只返回缺失变量名称
 * - GET|PUT /api/config、GET|PUT /api/business-config、POST /api/business-config/preflight、
 *   POST /api/admin/session[/revoke]
 *   → 透传至后端；后端校验 bootstrap 凭据或短期 admin session
 * - GET  /healthz            → 透传到源站（源站自己的健康检查）
 * - GET  /healthz-worker     → LB 聚合健康（MOD-HEALTH-AGG），响应体带 LB_VERSION
 * - POST /webhook/tg|/push/jmap|/api/push/register|/api/push/disable、GET /ready → 透传 + 有界故障转移
 * - POST /reconcile|/worker → 外部 cron 触发的同步长任务，长超时 + 单发不故障转移
 * - 其它 → 404 / 405
 *
 * 日志红线：仅打「方法 / 路径 / origin / 失败类别」，绝不打印 header/body/secret。
 */

import { parseBackendOrigins, LB_ROUTES } from "./backends.js";
import { forwardWithFailover } from "./lb.js";
import { aggregateHealth, healthResponse, DEFAULT_HEALTH_TTL_MS } from "./health.js";

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_MAX_ATTEMPTS = 2; // 首次 + 1 次故障转移（§10.4 建议）
// /reconcile 是后端同步长任务：handler 持集群级锁 `lock:reconcile`（初租 300s、30s 心跳续期至 90s，
// notify.rs reconcile 入口）并在同一请求内同步跑 reconcile()。全局 10s 超时会把它误判失败并故障转移到
// 第二实例——第二实例必然立刻撞 409（锁已被占），重试只放大冲突。故仅对此路由覆盖：
// 长超时（默认 320s > 300s 初租 + 心跳余量）+ maxAttempts=1，绝不故障转移；其余快路径路由保持全局默认。
const DEFAULT_RECONCILE_TIMEOUT_MS = 320_000;
// /worker 同样是外部 cron 触发的同步长任务：一次最多排空 batch 条（默认 10，硬上限 10），
// 每条消息的出站调用会重试 max_retries 次，最坏墙钟远超全局 10s——超时会把一次正常的
// 排空误判成失败并按 maxAttempts=2 切到第二实例，第二个实例只会重复排同一批消息。
// 取后端自己的单条事件下界（state.rs SINGLE_EVENT_CEILING_FLOOR_MS = 300s）做默认值：
// 单条慢消息永远装得下；batch 拉大时需要相应上调。
const DEFAULT_WORKER_TIMEOUT_MS = 300_000;

/**
 * 健康探测缓存（模块作用域 = CF Worker isolate 内跨请求复用）。
 * 仅存「origin → {at,up,status}」，不含任何 header/body/secret（SAF-LOG-PURITY 精神）。
 * TTL 由 LB_HEALTH_TTL_MS 控制；isolate 冷启时为空，首个 /healthz-worker 全量探测。
 */
const healthCache = new Map();

/** 各透传路由允许的 method（ARCH-LB-WORKER / C-LB-SINGLE-REG-URL；其余 405，不透传至后端）。 */
const ROUTE_METHODS = Object.freeze({
  "/": ["GET"],
  "/assets/config.js": ["GET"],
  "/assets/styles.css": ["GET"],
  "/api/status": ["GET"],
  "/api/config": ["GET", "PUT"],
  "/api/business-config": ["GET", "PUT"],
  "/api/business-config/preflight": ["POST"],
  "/api/admin/session": ["POST"],
  "/api/admin/session/revoke": ["POST"],
  "/api/enabled": ["GET", "PUT"],
  "/webhook/tg": ["POST"],
  "/push/jmap": ["POST"],
  "/api/push/register": ["POST"],
  "/api/push/disable": ["POST"],
  "/reconcile": ["POST"],
  "/worker": ["POST"],
  "/healthz": ["GET"],
  "/ready": ["GET"],
});

/**
 * Worker 主处理器（纯函数，便于单测）。
 * @param {Request} request
 * @param {object} env { BACKEND_ORIGINS_JSON, LB_REQUEST_TIMEOUT_MS?, LB_MAX_ATTEMPTS? }
 * @returns {Promise<Response>}
 */
export async function handleFetch(request, env) {
  const path = new URL(request.url).pathname;

  // LB 级健康聚合探针（不含敏感信息，SAF-PROBE-PUBLIC 精神）。
  // 挂在 /healthz-worker：/healthz 保留给源站自己的健康检查（透传），这样入站域名上
  // 两个健康检查互不遮蔽、能分别看到 LB 与源站的版本。
  if (path === "/healthz-worker") {
    if (request.method !== "GET") {
      return text("method not allowed for /healthz-worker", 405, { allow: "GET" });
    }
    return handleHealth(env);
  }

  // safelist 路由校验：未知路径 404，不透传至后端（C-LB-SINGLE-REG-URL）。
  if (!LB_ROUTES.includes(path)) {
    return text(`route not forwarded: ${path}`, 404);
  }
  const allowed = ROUTE_METHODS[path];
  if (!allowed || !allowed.includes(request.method)) {
    return text(`method not allowed for ${path}`, 405, { allow: allowed?.join(", ") });
  }

  let origins;
  try {
    origins = parseBackendOrigins(env.BACKEND_ORIGINS_JSON).map((o) => o.url);
  } catch {
    return text("misconfigured backends", 503); // fail-closed，不泄漏 secret
  }
  if (origins.length === 0) {
    return text("no backends configured", 503);
  }

  let timeoutMs = intFromEnv(env.LB_REQUEST_TIMEOUT_MS, DEFAULT_TIMEOUT_MS);
  let maxAttempts = intFromEnv(env.LB_MAX_ATTEMPTS, DEFAULT_MAX_ATTEMPTS);

  // Per-route 覆盖：/reconcile 是后端同步长任务（handler 持集群级锁 lock:reconcile，
  // 初租 300s、30s 心跳续期，notify.rs reconcile 入口），并在同一请求内同步跑 reconcile()。
  // 全局 10s 超时会把它误判失败并按 maxAttempts=2 故障转移到第二实例——第二实例必然立刻撞
  // 409（锁已被占），重试只放大冲突、对 curl/cron 调度器是硬伤。故仅对此路由覆盖：
  // 长超时（LB_RECONCILE_TIMEOUT_MS，默认 320s > 300s 初租 + 心跳余量）+ maxAttempts=1，
  // 绝不故障转移；其余快路径（/webhook/tg 等）保持全局默认 10s/2 不变。
  if (path === "/reconcile") {
    timeoutMs = intFromEnv(env.LB_RECONCILE_TIMEOUT_MS, DEFAULT_RECONCILE_TIMEOUT_MS);
    maxAttempts = 1; // 故障转移无意义：切到另一 origin 必然撞 lock:reconcile 409
  } else if (path === "/worker") {
    timeoutMs = intFromEnv(env.LB_WORKER_TIMEOUT_MS, DEFAULT_WORKER_TIMEOUT_MS);
    maxAttempts = 1; // 故障转移无意义：切到另一 origin 只会重复排同一批消息
  }

  // 透传转发 + 有界故障转移（仅超时/5xx）。
  return forwardWithFailover(origins, request, {
    maxAttempts,
    timeoutMs,
    fetch: lazyFetch(),
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
    rng: Math.random,
    log: (msg) => {
      try {
        console.error(`[lb] ${request.method} ${path}: ${msg}`);
      } catch {
        /* 日志不可用不致命 */
      }
    },
  });
}

/** 透传路由的统一错误/兜底日志不打印任何 header/body。 */
async function handleHealth(env) {
  const version = lbVersion(env);
  let origins;
  try {
    origins = parseBackendOrigins(env.BACKEND_ORIGINS_JSON).map((o) => o.url);
  } catch {
    return healthResponse({ available: 0, total: 0, backends: [] }, version);
  }
  if (origins.length === 0) {
    return healthResponse({ available: 0, total: 0, backends: [] }, version);
  }
  const summary = await aggregateHealth(origins, {
    fetch: lazyFetch(),
    timeoutMs: intFromEnv(env.LB_REQUEST_TIMEOUT_MS, DEFAULT_TIMEOUT_MS),
    ttlMs: intFromEnv(env.LB_HEALTH_TTL_MS, DEFAULT_HEALTH_TTL_MS),
    cache: healthCache,
  });
  return healthResponse(summary, version);
}

/**
 * `/healthz` 的版本标识：确认线上是哪次部署在回答（`env.LB_VERSION`，写在
 * `wrangler.toml` 的 `[vars]`，随 git 变动；每次改 LB 逻辑的提交同时 bump）。
 * 不进 `[vars]` 的理由：它必须与代码同版本、可 diff，dashboard 里的手写值反而容易漂。
 * 未配置回退 `unknown`——探针永远不因缺这个字段而失败。
 */
function lbVersion(env) {
  return env.LB_VERSION ?? "unknown";
}

/** 惰性绑定 globalThis.fetch，避免解构时丢失 this。 */
function lazyFetch() {
  return (...args) => globalThis.fetch(...args);
}

function text(body, status, extraHeaders = {}) {
  return new Response(body, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", ...extraHeaders },
  });
}

function intFromEnv(raw, fallback) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return fallback;
  return Math.floor(n);
}

/** Cloudflare Workers module 入口。 */
export default {
  async fetch(request, env, _ctx) {
    return handleFetch(request, env);
  },
};
