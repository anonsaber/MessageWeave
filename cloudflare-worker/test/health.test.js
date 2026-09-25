/**
 * 单元测试：健康聚合（MOD-HEALTH-AGG / C-NO-DB / C-REDIS-ONLY-STATE）。
 * 运行：cd cloudflare-worker && node --test
 */
import test from "node:test";
import assert from "node:assert/strict";
import { aggregateHealth, healthResponse } from "../src/health.js";

function mockCtx(plan, opts = {}) {
  const calls = [];
  const cache = opts.cache ?? new Map();
  let time = 1_000_000;
  const fetch = async (url) => {
    calls.push(url);
    const step = plan[url] ?? { status: 500 };
    if (step.error) throw step.error;
    return new Response(step.body ?? "ok", { status: step.status });
  };
  return {
    fetch,
    timeoutMs: 1000,
    ttlMs: opts.ttlMs ?? 30_000,
    now: () => time,
    advance: (ms) => {
      time += ms;
    },
    setTimeout: () => 0,
    clearTimeout: () => {},
    cache,
    calls,
  };
}

test("aggregateHealth: all up => 200 ok summary", async () => {
  const ctx = mockCtx({
    "https://a.example/healthz": { status: 200 },
    "https://b.example/healthz": { status: 200 },
  });
  const res = await aggregateHealth(["https://a.example", "https://b.example"], ctx);
  assert.equal(res.available, 2);
  assert.equal(res.total, 2);
  const resp = healthResponse(res);
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.status, "ok");
  assert.equal(body.available, 2);
  assert.equal(body.total, 2);
});

test("aggregateHealth: partial down => still reports status (partial ok)", async () => {
  const ctx = mockCtx({
    "https://a.example/healthz": { status: 200 },
    "https://b.example/healthz": { error: new Error("conn refused") },
  });
  const res = await aggregateHealth(["https://a.example", "https://b.example"], ctx);
  assert.equal(res.available, 1);
  assert.equal(res.total, 2);
  const resp = healthResponse(res);
  assert.equal(resp.status, 200, "at least one up => 200 ok");
  const body = await resp.json();
  assert.equal(body.status, "ok");
  assert.equal(body.available, 1);
});

test("aggregateHealth: all down => 503", async () => {
  const ctx = mockCtx({
    "https://a.example/healthz": { status: 503 },
    "https://b.example/healthz": { error: new Error("conn refused") },
  });
  const res = await aggregateHealth(["https://a.example", "https://b.example"], ctx);
  assert.equal(res.available, 0);
  const resp = healthResponse(res);
  assert.equal(resp.status, 503);
  const body = await resp.json();
  assert.equal(body.status, "down");
});

test("aggregateHealth: respects cache TTL to avoid probe storm", async () => {
  const ctx = mockCtx(
    {
      "https://a.example/healthz": { status: 200 },
    },
    { ttlMs: 10_000 },
  );
  await aggregateHealth(["https://a.example"], ctx);
  assert.equal(ctx.calls.length, 1);

  // 未过 TTL：命中缓存，不再发起 fetch。
  ctx.advance(5_000);
  await aggregateHealth(["https://a.example"], ctx);
  assert.equal(ctx.calls.length, 1);

  // 超过 TTL：发起新 fetch。
  ctx.advance(6_000);
  await aggregateHealth(["https://a.example"], ctx);
  assert.equal(ctx.calls.length, 2);
});
