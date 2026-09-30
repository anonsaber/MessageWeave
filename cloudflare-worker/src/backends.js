/**
 * origin 配置解析 + 校验（稳定 ID：C-HTTPS-INBOUND / SAF-LB-PASSTHRU / C-NO-DB）。
 * 后端 origin 仅允许 https（C-HTTPS-INBOUND）；不转发 Redis/JMAP 流量（C-NO-DB）。
 * 仅 ES2022，零依赖，可 node 直接执行。
 */

/** 允许透传的入站路由 safelist（C-LB-SINGLE-REG-URL）：未知路径 → 404。
 * `/healthz` 透传到源站（源站自己的健康检查）；LB 自己的聚合探针在 `/healthz-worker`，不在列。
 * SPA 静态资源为公开 GET；所有管理 API 仍由后端校验 Redis bootstrap 凭据或 admin session。 */
export const SAFE_ROUTES = Object.freeze([
  "/",
  "/assets/config.js",
  "/assets/styles.css",
  "/api/status",
  "/api/config",
  "/api/business-config",
  "/api/business-config/preflight",
  "/api/admin/session",
  "/api/admin/session/revoke",
  "/api/enabled",
  "/webhook/tg",
  "/push/jmap",
  "/api/push/register",
  "/api/push/disable",
  "/reconcile",
  "/healthz",
  "/ready",
]);

/** 需要透传到后端 origin 的路由（不含 Worker 聚合端点）。 */
export const LB_ROUTES = Object.freeze([...SAFE_ROUTES]);

export class BackendConfigError extends Error {
  constructor(message) {
    super(message);
    this.name = "BackendConfigError";
  }
}

const HTTPS_STATUS = 443;

/**
 * 校验并归一化单个 backend origin。
 * 规则（C-HTTPS-INBOUND，对齐 §10 的 fail-closed）：
 * - 必须是 https；
 * - 必须无内嵌凭据（禁 user:pass@host）；
 * - 必须无 query / fragment（origin 不应带这些）；
 * - 路径必须为空（origin 根），禁止 /path 形式（防止把路由写进 origin）。
 * @param {string} raw
 * @returns {{ url: string, host: string, port: number }}
 */
export function parseBackendOrigin(raw) {
  if (typeof raw !== "string" || raw.trim() === "") {
    throw new BackendConfigError("backend origin must be a non-empty https URL");
  }
  let parsed;
  try {
    parsed = new URL(raw.trim());
  } catch {
    throw new BackendConfigError(`backend origin is not a valid URL: ${raw}`);
  }
  if (parsed.protocol !== "https:") {
    throw new BackendConfigError(
      `backend origin must use https (C-HTTPS-INBOUND): ${raw}`,
    );
  }
  if (parsed.username !== "" || parsed.password !== "") {
    throw new BackendConfigError(
      `backend origin must not embed credentials (C-HTTPS-INBOUND): ${parsed.host}`,
    );
  }
  if (parsed.search !== "" || parsed.hash !== "") {
    throw new BackendConfigError(
      `backend origin must not contain query/fragment: ${parsed.host}`,
    );
  }
  if (parsed.pathname !== "" && parsed.pathname !== "/") {
    throw new BackendConfigError(
      `backend origin must be a bare origin (no path): ${raw}`,
    );
  }
  // 记录端口以便健康探测/请求拼 URL；缺省 443。
  const port = parsed.port !== "" ? Number(parsed.port) : HTTPS_STATUS;
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new BackendConfigError(`backend origin has an invalid port: ${raw}`);
  }
  return { url: parsed.origin, host: parsed.hostname, port };
}

/**
 * 解析 `BACKEND_ORIGINS_JSON` 环境变量：JSON 字符串数组。
 * 全部校验通过后返回去重（保持顺序）的 origins；无有效条目则报错。
 * @param {unknown} raw 可能为 JSON 字符串或已解析数组
 * @returns {Array<{url:string, host:string, port:number, source:string}>}
 */
export function parseBackendOrigins(raw) {
  let list;
  if (Array.isArray(raw)) {
    list = raw;
  } else if (typeof raw === "string") {
    try {
      list = JSON.parse(raw);
    } catch {
      throw new BackendConfigError("BACKEND_ORIGINS_JSON is not valid JSON");
    }
  } else {
    throw new BackendConfigError("BACKEND_ORIGINS_JSON must be a JSON array of https origins");
  }
  if (!Array.isArray(list) || list.length === 0) {
    throw new BackendConfigError("BACKEND_ORIGINS_JSON must be a non-empty array");
  }
  const seen = new Set();
  const out = [];
  for (const entry of list) {
    const origin = parseBackendOrigin(entry);
    if (!seen.has(origin.url)) {
      seen.add(origin.url);
      out.push(origin);
    }
  }
  return out;
}

export { HTTPS_STATUS };
