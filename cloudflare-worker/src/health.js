/**
 * 健康聚合探针（稳定 ID：MOD-HEALTH-AGG │ C-NO-DB │ C-REDIS-ONLY-STATE）。
 *
 * 语义：
 * - Worker 暴露 LB 级 `GET /healthz`，聚合探测各后端 origin 的 `/healthz`（后端公开探针，无敏感信息）。
 * - 缓存 TTL（默认 30秒）避免健康探测放大；TTL 内复用上次结果，过期后才重探。
 * - 探测失败/超时 → 该 origin 记为 down；`available` 统计存活数。
 * - 仅 HTTP 层健康（<500 记为 up）：**不**检查/代理 Redis、JMAP（C-NO-DB / C-REDIS-ONLY-STATE），
 *   就绪依赖检查仍由后端 `/ready`（GATE-READY-DEPS）承担。
 *
 * 可测性：fetch / 计时器 / 时钟均依赖注入。
 */

export const DEFAULT_HEALTH_TTL_MS = 30_000;

/**
 * 聚合探测所有后端 origin 的 `/healthz`。
 * @param {string[]} origins 已校验的 origin 列表
 * @param {object} ctx { fetch, timeoutMs, ttlMs?, now?, cache?, setTimeout?, clearTimeout? }
 * @returns {Promise<{available:number,total:number,backends:Array<{origin:string,up:boolean,status:number|null}>}>}
 */
export async function aggregateHealth(origins, ctx) {
  const {
    fetch,
    timeoutMs,
    ttlMs = DEFAULT_HEALTH_TTL_MS,
    now = () => Date.now(),
    cache,
    setTimeout: st = globalThis.setTimeout,
    clearTimeout: ct = globalThis.clearTimeout,
  } = ctx;

  const backends = [];
  let available = 0;

  for (const origin of origins) {
    const key = `${origin}/healthz`;
    const cached = cache ? cache.get(key) : undefined;
    let up = false;
    let status = null;

    if (cached && ttlMs > 0 && now() - cached.at < ttlMs) {
      up = cached.up;
      status = cached.status;
    } else {
      try {
        const controller = new AbortController();
        const timer = st(() => controller.abort(), timeoutMs);
        const resp = await fetch(key, { method: "GET", signal: controller.signal });
        ct(timer);
        status = resp.status;
        up = resp.status < 500;
      } catch {
        up = false;
        status = null;
      }
      if (cache) cache.set(key, { at: now(), up, status });
    }

    if (up) available += 1;
    backends.push({ origin, up, status });
  }

  return { available, total: origins.length, backends };
}

/**
 * 组装 LB 级 `/healthz` 响应：有 ≥1 后端存活 → 200；全 down → 503；无配置 → 200 + 明确标记。
 * @param {{available:number,total:number,backends:unknown[]}} summary
 * @returns {Response}
 */
export function healthResponse(summary) {
  const healthy = summary.total > 0 && summary.available > 0;
  const status =
    summary.total === 0 ? "no-backends" : healthy ? "ok" : "down";
  return new Response(JSON.stringify({ status, ...summary }), {
    status: summary.total === 0 ? 200 : healthy ? 200 : 503,
    headers: { "content-type": "application/json" },
  });
}
