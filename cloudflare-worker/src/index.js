/**
 * Cloudflare Worker 入口 — MessageWeave 多实例 LB/HA（ARCH-LB-WORKER / deployment.md §10）。
 *
 * 稳定 ID（AGENTS.md §5/§7）：
 * - SAF-LB-PASSTHRU：透传 headers/body（含鉴权头），不做鉴权改写。
 * - C-HTTPS-INBOUND：仅 https 后端 origin。
 * - ARCH-LB-WORKER / C-LB-SINGLE-REG-URL：safelist 路由，未知路径 404、method 不符 405。
 * - MOD-HEALTH-AGG：/healthz 由 Worker 聚合，不落后端。
 * - C-NO-DB / C-REDIS-ONLY-STATE：不代理 Redis/JMAP；不检查数据库侧可用性。
 * - C-NO-LONG-CONN：纯请求-响应，body 一次性读入回灌；无 WS/SSE/长轮询。
 * - C-NO-SECRET-IN-IMAGE：日志仅「方法/路径/origin/失败类别」，不含 secret；origin 清单走 wrangler secret。
 *
 * 路由模型：
 * - GET /、/assets/config.js、/assets/styles.css → 内嵌 SPA 静态资源透传
 * - GET /api/status → 后端配置引导状态；只返回缺失变量名称
 * - GET|PUT /api/config、PUT /api/business-config、POST /api/admin/session[/revoke]
 *   → 透传至后端；后端校验 bootstrap 凭据或短期 admin session
 * - GET  /healthz            → LB 聚合健康（MOD-HEALTH-AGG）
 * - POST /webhook/tg|/push/jmap|/reconcile、GET /ready → 透传 + 有界故障转移
 * - 其它 → 404 / 405
 *
 * 日志红线：仅打「方法 / 路径 / origin / 失败类别」，绝不打印 header/body/secret。
 */

import { parseBackendOrigins, PROXIED_ROUTES } from "./backends.js";
import { proxyWithFailover } from "./proxy.js";
import { aggregateHealth, healthResponse, DEFAULT_HEALTH_TTL_MS } from "./health.js";

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_MAX_ATTEMPTS = 2; // 首次 + 1 次故障转移（§10.4 建议）

/**
 * 健康探测缓存（模块作用域 = CF Worker isolate 内跨请求复用）。
 * 仅存「origin → {at,up,status}」，不含任何 header/body/secret（SAF-LOG-PURITY 精神）。
 * TTL 由 LB_HEALTH_TTL_MS 控制；isolate 冷启时为空，首个 /healthz 全量探测。
 */
const healthCache = new Map();

/** 各透传路由允许的 method（ARCH-LB-WORKER / C-LB-SINGLE-REG-URL；其余 405，不透传至后端）。 */
const ROUTE_METHODS = Object.freeze({
  "/": ["GET"],
  "/assets/config.js": ["GET"],
  "/assets/styles.css": ["GET"],
  "/api/status": ["GET"],
  "/api/config": ["GET", "PUT"],
  "/api/business-config": ["PUT"],
  "/api/admin/session": ["POST"],
  "/api/admin/session/revoke": ["POST"],
  "/webhook/tg": ["POST"],
  "/push/jmap": ["POST"],
  "/reconcile": ["POST"],
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
  if (path === "/healthz" && request.method === "GET") {
    return handleHealth(env);
  }

  // safelist 路由校验：未知路径 404，不透传至后端（C-LB-SINGLE-REG-URL）。
  if (!PROXIED_ROUTES.includes(path)) {
    return text(`route not proxied: ${path}`, 404);
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

  const timeoutMs = intFromEnv(env.LB_REQUEST_TIMEOUT_MS, DEFAULT_TIMEOUT_MS);
  const maxAttempts = intFromEnv(env.LB_MAX_ATTEMPTS, DEFAULT_MAX_ATTEMPTS);

  // 透传转发 + 有界故障转移（仅超时/5xx）。
  return proxyWithFailover(origins, request, {
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
  let origins;
  try {
    origins = parseBackendOrigins(env.BACKEND_ORIGINS_JSON).map((o) => o.url);
  } catch {
    return healthResponse({ available: 0, total: 0, backends: [] });
  }
  if (origins.length === 0) {
    return healthResponse({ available: 0, total: 0, backends: [] });
  }
  const summary = await aggregateHealth(origins, {
    fetch: lazyFetch(),
    timeoutMs: intFromEnv(env.LB_REQUEST_TIMEOUT_MS, DEFAULT_TIMEOUT_MS),
    ttlMs: intFromEnv(env.LB_HEALTH_TTL_MS, DEFAULT_HEALTH_TTL_MS),
    cache: healthCache,
  });
  return healthResponse(summary);
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
