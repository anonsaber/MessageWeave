/**
 * 单元测试：透传转发 + 有界故障转移 + 全失败 5xx（SAF-LB-PASSTHRU / SAF-LOG-PURITY / C-NO-SECRET-IN-IMAGE）。
 * 运行：cd cloudflare-worker && node --test
 */
import test from "node:test";
import assert from "node:assert/strict";
import { forwardWithFailover, joinUrl, isRetryableError } from "../src/lb.js";

/**
 * 可控 fetch mock：steps[i] 每次调用消费一个；
 * - {status, body?} → 返回该 Response；
 * - {error: Error} → 抛出。
 * - {response: Response} → 原样返回该对象（用于模拟 workerd 特有行为，如不可变响应头）；
 */
function mockFetch(steps) {
  const calls = [];
  const fetch = async (url, init) => {
    const i = calls.length;
    calls.push({ url, init });
    const step = steps[Math.min(i, steps.length - 1)];
    if (step.error) throw step.error;
    if (step.response) return step.response;
    return new Response(step.body ?? "ok", { status: step.status, headers: { "content-type": "text/plain" } });
  };
  return { fetch, calls };
}

/** 测试用计时器：不真触发超时。 */
const noOpTimers = { setTimeout: () => 0, clearTimeout: () => {} };

/**
 * 模拟「不可变响应头」：跨域且不带 CORS 允许头的响应，其 headers guard 是 immutable，
 * 任何写操作都抛 `TypeError: Can't modify immutable headers.`（workerd 实测行为）。
 * 生产 LB 代理后端 `/` 正是这种响应。
 */
function immutableResponse(status, body = "healthy") {
  return {
    status,
    headers: {
      get() { return null; },
      set() { throw new TypeError("Can't modify immutable headers."); },
      append() { throw new TypeError("Can't modify immutable headers."); },
    },
    body: undefined,
    text: async () => body,
  };
}

function req(path = "/reconcile") {
  return new Request(`https://lb.example${path}`, {
    method: "POST",
    headers: { authorization: "Bearer secret-token", "content-type": "application/json" },
    body: JSON.stringify({ id: 42 }),
  });
}

function abortError() {
  const err = new Error("The operation was aborted");
  err.name = "AbortError";
  return err;
}

test("isRetryableError: only 5xx (status) and timeout/network (err) are retryable", () => {
  assert.equal(isRetryableError(new Error("boom"), 0), false, "generic error not retryable");
  assert.equal(isRetryableError(abortError(), 0), true, "AbortError retryable");
  assert.equal(isRetryableError(new Error("fetch failed"), 0), true, "network-class message retryable");
  assert.equal(isRetryableError(new Error("bad inner state"), 0), false, "non-network error not retryable");
  assert.equal(isRetryableError(null, 503), true, "5xx retryable");
  assert.equal(isRetryableError(null, 404), false, "4xx not retryable");
  assert.equal(isRetryableError(null, 200), false, "2xx not retryable");
});

test("transparent passthrough forwards headers + body + method (SAF-LB-PASSTHRU)", async () => {
  const { fetch, calls } = mockFetch([{ status: 202, body: "accepted" }]);
  const res = await forwardWithFailover(["https://a.example"], req("/webhook/tg"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0.5,
  });
  assert.equal(res.status, 202);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "https://a.example/webhook/tg");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers.get("authorization"), "Bearer secret-token");
  assert.equal(calls[0].init.headers.get("content-type"), "application/json");
  const body = await new Response(new Uint8Array(calls[0].init.body)).text();
  assert.equal(body, JSON.stringify({ id: 42 }));
  assert.equal(res.headers.get("x-lb-backend"), "https://a.example");
});

test("immutable response headers never mask a healthy 2xx as 503 (跨域无 CORS)", async () => {
  // 回归：2026-09-30 生产事故。后端返回健康的 200，但该响应跨域且无 CORS 头，
  // headers guard 为 immutable；mark() 写 `x-lb-backend` 抛 TypeError，被外层 catch
  // 记成 `backend ... unavailable (TypeError)` → 重试耗尽 → 503。/healthz 自建响应
  // 不调 mark()，所以一直正常，掩盖了问题。
  const { fetch, calls } = mockFetch([{ response: immutableResponse(200) }]);
  const res = await forwardWithFailover(["https://a.example"], req("/"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 200, "healthy 200 must be returned as-is");
  assert.equal(calls.length, 1, "a healthy response must not trigger failover");
  assert.equal(await res.text(), "healthy");
});

test("immutable headers are tolerated on the 5xx failover path too", async () => {
  const { fetch, calls } = mockFetch([{ response: immutableResponse(503) }, { status: 200, body: "ok" }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 200, "503 with immutable headers still fails over");
  assert.equal(calls.length, 2);
  assert.equal(res.headers.get("x-lb-backend"), "https://b.example", "marker still added on a mutable response");
});

test("GET/HEAD do not forward a body", async () => {
  const { fetch, calls } = mockFetch([{ status: 200 }]);
  const get = new Request("https://lb.example/ready", { method: "GET", headers: { authorization: "Bearer x" } });
  await forwardWithFailover(["https://a.example"], get, {
    ...noOpTimers,
    fetch,
    maxAttempts: 1,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(calls[0].init.body, undefined, "GET must not carry a body");
});

test("4xx is returned as-is WITHOUT retry and without failing over", async () => {
  const { fetch, calls } = mockFetch([{ status: 401, body: "unauthorized" }, { status: 200 }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/push/jmap"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 401);
  assert.equal(calls.length, 1, "must NOT retry/share next backend on 4xx");
  assert.equal(calls[0].url, "https://a.example/push/jmap");
});

test("5xx triggers failover to next origin, then returns success", async () => {
  const { fetch, calls } = mockFetch([{ status: 503, body: "busy" }, { status: 200, body: "ok" }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/push/jmap"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0, // 确定性起点 a
  });
  assert.equal(res.status, 200);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].url, "https://a.example/push/jmap");
  assert.equal(calls[1].url, "https://b.example/push/jmap");
  assert.equal(res.headers.get("x-lb-backend"), "https://b.example");
});

test("timeout(AbortError) triggers failover and recovery", async () => {
  const { fetch, calls } = mockFetch([{ error: abortError() }, { status: 200, body: "recovered" }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/reconcile"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 200);
  assert.equal(calls.length, 2);
});

test("all backends 5xx/timeout => aggregated 503 (Telegram redelivery fallback)", async () => {
  const { fetch, calls } = mockFetch([{ error: abortError() }, { status: 503, body: "x" }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/push/jmap"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 503);
  assert.equal(calls.length, 2);
  assert.match(await res.text(), /all backends failed/);
});

test("bounded attempts: maxAttempts caps the number of origin tries", async () => {
  const { fetch, calls } = mockFetch([{ error: abortError() }]); // 每次都超时（可故障转移）
  const res = await forwardWithFailover(["https://a", "https://b", "https://c"], req("/reconcile"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 503);
  assert.equal(calls.length, 2, "must stop after maxAttempts=2 (bounded, no infinite retry)");
});

test("non-retryable (non-network) error fails fast without burning other origins", async () => {
  const { fetch, calls } = mockFetch([{ error: new Error("invalid inner state") }, { status: 200 }]);
  const res = await forwardWithFailover(["https://a.example", "https://b.example"], req("/push/jmap"), {
    ...noOpTimers,
    fetch,
    maxAttempts: 2,
    timeoutMs: 5000,
    rng: () => 0,
  });
  assert.equal(res.status, 503);
  assert.equal(calls.length, 1, "non-network error must not failover");
});

test("joinUrl preserves path + query, discards fragment", () => {
  const r = new Request("https://lb.example/reconcile?cursor=abc#frag", { method: "POST" });
  assert.equal(joinUrl("https://a.example", r), "https://a.example/reconcile?cursor=abc");
});
