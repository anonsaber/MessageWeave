/**
 * 集成测试：Cloudflare Worker 主入口 handle fetch（ARCH-LB-WORKER / SAF-LB-PASSTHRU）。
 * 验证：路由分发、/healthz 聚合、未白名单 404、method 拦截、配置异常 fail-closed、鉴权头透传。
 * 运行：cd cloudflare-worker && node --test
 */
import test from "node:test";
import assert from "node:assert/strict";
import { handleFetch } from "../src/index.js";

function makeEnv(overrides = {}) {
  return {
    BACKEND_ORIGINS_JSON: JSON.stringify(["https://a.example", "https://b.example"]),
    LB_REQUEST_TIMEOUT_MS: "5000",
    LB_MAX_ATTEMPTS: "2",
    ...overrides,
  };
}

/** 安装/卸载 globalThis.fetch mock。 */
function stubFetch(impl) {
  const orig = globalThis.fetch;
  globalThis.fetch = impl;
  return () => {
    globalThis.fetch = orig;
  };
}

test("Worker GET /healthz aggregates backend probes without touching proxied routes", async () => {
  const restore = stubFetch(async (url) => new Response("ok", { status: 200 }));
  try {
    const res = await handleFetch(new Request("https://lb.example/healthz", { method: "GET" }), makeEnv());
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.status, "ok");
    assert.equal(body.available, 2);
    assert.equal(body.total, 2);
    assert.ok(Array.isArray(body.backends));
  } finally {
    restore();
  }
});

test("Worker: unknown route => 404 (C-LB-SINGLE-REG-URL)", async () => {
  const res = await handleFetch(
    new Request("https://lb.example/unknown", { method: "POST", body: "{}" }),
    makeEnv(),
  );
  assert.equal(res.status, 404);
  assert.match(await res.text(), /route not proxied/);
});

test("Worker: wrong method on safelist route => 405", async () => {
  const res = await handleFetch(
    new Request("https://lb.example/webhook/tg", { method: "GET" }),
    makeEnv(),
  );
  assert.equal(res.status, 405);
});

test("Worker: business config PUT is proxied with the session bearer and empty 204 response", async () => {
  const restore = stubFetch(async (url, init) => {
    assert.match(url, /^https:\/\/(a|b)\.example\/api\/business-config$/);
    assert.equal(init.method, "PUT");
    assert.equal(init.headers.get("authorization"), "Bearer opaque-session");
    assert.equal(await new Response(init.body).text(), "{\"llm_enabled\":false}");
    return new Response(null, { status: 204 });
  });
  try {
    const res = await handleFetch(
      new Request("https://lb.example/api/business-config", {
        method: "PUT",
        headers: { authorization: "Bearer opaque-session", "content-type": "application/json" },
        body: "{\"llm_enabled\":false}",
      }),
      makeEnv(),
    );
    assert.equal(res.status, 204);
  } finally {
    restore();
  }
});

test("Worker: public setup status is proxied without an authorization header", async () => {
  const restore = stubFetch(async (url, init) => {
    assert.match(url, /^https:\/\/(a|b)\.example\/api\/status$/);
    assert.equal(init.method, "GET");
    assert.equal(new Headers(init.headers).has("authorization"), false);
    return new Response(JSON.stringify({
      ready: false,
      mode: "configuration-setup",
      missing: ["REDIS_URL"],
    }), { status: 200, headers: { "content-type": "application/json" } });
  });
  try {
    const res = await handleFetch(new Request("https://lb.example/api/status"), makeEnv());
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), {
      ready: false,
      mode: "configuration-setup",
      missing: ["REDIS_URL"],
    });
  } finally {
    restore();
  }
});

test("Worker: misconfigured origins => 503 fail-closed (no env leak)", async () => {
  const res = await handleFetch(
    new Request("https://lb.example/reconcile", { method: "POST", body: "{}" }),
    makeEnv({ BACKEND_ORIGINS_JSON: "not-json" }),
  );
  assert.equal(res.status, 503);
  assert.equal(await res.text(), "misconfigured backends");
});

test("Worker: valid env, GET /ready proxied to backend with auth header intact", async () => {
  const restore = stubFetch(async (url, init) => {
    assert.match(url, /^https:\/\/(a|b)\.example\/ready$/);
    return new Response("ready", { status: 200, headers: { "content-type": "text/plain" } });
  });
  try {
    const res = await handleFetch(
      new Request("https://lb.example/ready", {
        method: "GET",
        headers: { authorization: "Bearer lb-test" },
      }),
      makeEnv(),
    );
    assert.equal(res.status, 200);
  } finally {
    restore();
  }
});
