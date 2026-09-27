/**
 * 请求转发与有界故障转移（稳定 ID：SAF-LB-PASSTHRU / C-NO-LONG-CONN / C-NO-SECRET-IN-IMAGE / deployment.md §10.4）。
 *
 * 语义：
 * - 透传（A）：原样转发 method / headers / body（含鉴权头），不做鉴权改写（SAF-LB-PASSTHRU）。
 * - 仅对「超时 / 5xx」做有界重试：换下一个 origin 再试（deployment.md §10.4 建议 1 次）；
 *   非 5xx / 非超时直接返回。
 * - 全失败 → 返回 5xx（交由 Telegram 自动重投，不丢消息）。
 * - 无 WebSocket/SSE/长轮询（C-NO-LONG-CONN）：纯请求-响应，body 一次性读入后回灌。
 *
 * 可测性：所有 I/O（fetch/计时器/随机）通过依赖注入传入，便于 mock。
 * 「无密钥日志」红线（SAF-LOG-PURITY）：只打错误类别与 origin（已校验、非敏感），
 * **不打印** header/body/secret；不产生任何文件写入，日志仅 stdout。
 */

/**
 * @typedef {Request} Req
 * @typedef {Response} Res
 */

/**
 * 拼接后端 URL：origin + path + query（保留 query，丢弃 fragment）。
 * @param {string} origin 已校验的 https origin
 * @param {Req} request
 * @returns {string}
 */
export function joinUrl(origin, request) {
  const u = new URL(request.url);
  return `${origin}${u.pathname}${u.search}`;
}

/**
 * 一次性读取请求 body 为可复用的 Uint8Array（供每个故障转移尝试回灌，
 * 避免 Request.body 流在第二次尝试时被“已消费”而抛 TypeError）。
 * @param {Req} request
 * @returns {Promise<{headers: Headers, body: Uint8Array | undefined}>}
 */
export async function snapshotBody(request) {
  const headers = new Headers(request.headers);
  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  const body = hasBody ? new Uint8Array(await request.arrayBuffer()) : undefined;
  return { headers, body };
}

/** 是否「可故障转移」：5xx 状态码、超时/网络类错误（err）。 */
export function isRetryableError(err, status) {
  if (err) {
    return isRetryableNetworkError(err);
  }
  return status >= 500 && status < 600;
}

/**
 * 是否「可故障转移」的网络错误：
 * 超时（AbortError）或 fetch/连接层网络错误（Node: `fetch failed`/`ECONNREFUSED` 等）。
 * 其它错误（如 body 读取失败、配置错误）**不**触发故障转移，避免无谓重试。
 */
function isRetryableNetworkError(err) {
  if (!err) return false;
  const name = String(err.name || "");
  const msg = String(err.message || "");
  if (name === "AbortError") return true;
  if (/fetch failed|network error|socket hang up|ECONNREFUSED|ECONNRESET|EAI_AGAIN|ENOTFOUND|ETIMEDOUT|EHOSTUNREACH|ENETUNREACH/i.test(msg)) return true;
  // 递归 cause 链（Node 的 fetch 网络错误把 errno 藏在 cause）。
  if (err.cause && err.cause !== err) return isRetryableNetworkError(err.cause);
  return false;
}

/**
 * 单次 origin 尝试：透传转发。超时/网络错误会 throw（由调用方处理）。
 * 使用预读的 body 快照（Uint8Array），避免 Request.body 流在第二次尝试时“已消费”。
 * @param {string} origin
 * @param {Req} request
 * @param {object} io { fetch, setTimeout, clearTimeout, timeoutMs }
 * @param {{headers: Headers, body: Uint8Array | undefined}} snapshot 由 snapshotBody 产出
 * @returns {Promise<Res>}
 */
export async function forwardRequest(origin, request, io, snapshot) {
  const { fetch, setTimeout: st, clearTimeout: ct, timeoutMs } = io;
  const url = joinUrl(origin, request);
  const controller = new AbortController();
  const timer = st(() => controller.abort(), timeoutMs);
  try {
    const resp = await fetch(url, {
      method: request.method,
      headers: snapshot.headers,
      body: snapshot.body ? new Uint8Array(snapshot.body) : undefined,
      redirect: "follow",
      signal: controller.signal,
    });
    return resp;
  } finally {
    ct(timer);
  }
}

/**
 * 有界故障转移：按序尝试 origins（起点随机化实现双活分摊，确定性可注入）；
 * 仅超时/5xx 才换下一个实例；全失败 → 5xx 聚合响应。
 * @param {string[]} origins 有序 origin 列表（≥1）
 * @param {Req} request 入站请求
 * @param {object} opts { maxAttempts, timeoutMs, fetch, setTimeout, clearTimeout, rng?, log? }
 * @returns {Promise<Res>}
 */
export async function forwardWithFailover(origins, request, opts) {
  const { maxAttempts, timeoutMs, fetch, setTimeout, clearTimeout, rng, log } = opts;
  const start = origins.length > 1 && rng ? Math.floor(rng() * origins.length) : 0;
  const queue = origins.slice(start).concat(origins.slice(0, start));
  const attempts = Math.max(1, Math.min(maxAttempts, queue.length));
  let lastFailure = "no backends configured";
  // 一次性读 body 快照：让每次故障转移尝试可回灌同一请求体（body 流只能读一次）。
  const snapshot = await snapshotBody(request);

  for (let i = 0; i < attempts; i += 1) {
    const origin = queue[i];
    const io = { fetch, setTimeout, clearTimeout, timeoutMs };
    try {
      const resp = await forwardRequest(origin, request, io, snapshot);
      // 非 5xx / 非网络错误：直接返回（4xx、2xx、3xx 均原样透传，不故障转移）。
      if (!isRetryableError(null, resp.status)) {
        return mark(resp, origin);
      }
      // 5xx：释放 body，换下一个实例。
      resp.body?.cancel?.().catch(() => {});
      lastFailure = `backend ${origin} returned HTTP ${resp.status}`;
      if (log) log(`retrying ${request.method} ${new URL(request.url).pathname}: ${lastFailure}`);
    } catch (err) {
      lastFailure = `backend ${origin} unavailable (${err && err.name ? err.name : "error"})`;
      if (log) log(lastFailure);
      // 仅「可故障转移」的网络类失败（超时/连接错误）才换实例；其它错误 fail-fast，避免无限重试。
      if (!isRetryableError(err, 0)) break;
    }
  }
  // 全部 origin 失败 → 5xx（交由 Telegram/Stalwart 自动重投兜底，不丢消息）。
  return new Response(`all backends failed: ${lastFailure}`, {
    status: 503,
    statusText: "All Backends Unavailable",
    headers: { "content-type": "text/plain; charset=utf-8" },
  });
}

/** 在响应上加轻量可观测头（不改 body/状态语义；origin 非敏感，含在响应头中便于排障）。 */
function mark(resp, origin) {
  resp.headers.set("x-lb-backend", origin);
  return resp;
}
