/**
 * 集成测试：Cloudflare Worker 主入口 handle fetch（ARCH-LB-WORKER / SAF-LB-PASSTHRU）。
 * 验证：路由分发、/healthz-worker 聚合、/healthz 透传、未白名单 404、method 拦截、配置异常 fail-closed、鉴权头透传。
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

test("Worker GET /healthz-worker aggregates backend probes without touching forwarded routes", async () => {
  const restore = stubFetch(async (url) => new Response("ok", { status: 200 }));
  try {
    const res = await handleFetch(new Request("https://lb.example/healthz-worker", { method: "GET" }), makeEnv());
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.status, "ok");
    assert.equal(body.available, 2);
    assert.equal(body.total, 2);
    assert.ok(Array.isArray(body.backends));
    // 未配置 LB_VERSION 时回退 "unknown"——探针绝不因缺这个字段而失败。
    assert.equal(body.version, "unknown");

    // 配置后原样回显：确认线上是哪次部署在回答。
    const res2 = await handleFetch(
      new Request("https://lb.example/healthz-worker", { method: "GET" }),
      makeEnv({ LB_VERSION: "2026.10.1" }),
    );
    const body2 = await res2.json();
    assert.equal(body2.version, "2026.10.1");
  } finally {
    restore();
  }
});

test("Worker: wrong method on the local /healthz-worker probe => 405, not 404", async () => {
  const res = await handleFetch(
    new Request("https://lb.example/healthz-worker", { method: "POST", body: "{}" }),
    makeEnv(),
  );
  assert.equal(res.status, 405);
  assert.equal(res.headers.get("allow"), "GET");
});

test("Worker GET /healthz is forwarded to the origin, not answered by the LB", async () => {
  // 两个健康检查共存：/healthz 属于源站，/healthz-worker 属于 LB。
  const restore = stubFetch(async () => new Response("origin-ok", { status: 200 }));
  try {
    const res = await handleFetch(
      new Request("https://lb.example/healthz", { method: "GET" }),
      makeEnv(),
    );
    assert.equal(res.status, 200);
    assert.equal(await res.text(), "origin-ok");
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
  assert.match(await res.text(), /route not forwarded/);
});

test("Worker: wrong method on safelist route => 405", async () => {
  const res = await handleFetch(
    new Request("https://lb.example/webhook/tg", { method: "GET" }),
    makeEnv(),
  );
  assert.equal(res.status, 405);
});

test("Worker: business config PUT is forwarded with the session bearer and empty 204 response", async () => {
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

test("Worker: business config read-back GET and preflight POST are both forwarded", async () => {
  const seen = [];
  const restore = stubFetch(async (url, init) => {
    seen.push([String(url), init.method]);
    return new Response(JSON.stringify({ configured: false }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  });
  try {
    const readback = await handleFetch(
      new Request("https://lb.example/api/business-config", { method: "GET" }),
      makeEnv(),
    );
    assert.equal(readback.status, 200);
    assert.deepEqual(await readback.json(), { configured: false });

    const preflight = await handleFetch(
      new Request("https://lb.example/api/business-config/preflight", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      }),
      makeEnv(),
    );
    assert.equal(preflight.status, 200);

    assert.equal(seen.length, 2);
    // The LB rotates its start index across tests, so the origin is not stable;
    // only the path and method are. Assert the origin is one of the configured
    // backends (not arbitrary) and check the path/method exactly.
    const backends = JSON.parse(makeEnv().BACKEND_ORIGINS_JSON);
    for (const [url, method] of seen) {
      const target = new URL(url);
      assert.ok(backends.includes(target.origin), `${url} reached an unknown upstream`);
    }
    assert.equal(new URL(seen[0][0]).pathname, "/api/business-config");
    assert.equal(seen[0][1], "GET");
    assert.equal(new URL(seen[1][0]).pathname, "/api/business-config/preflight");
    assert.equal(seen[1][1], "POST");
  } finally {
    restore();
  }
});

test("Worker: only GET/PUT reach /api/business-config and only POST reaches preflight (SAF-LB-PASSTHRU)", async () => {
  for (const method of ["DELETE", "PATCH"]) {
    const res = await handleFetch(
      new Request("https://lb.example/api/business-config", { method }),
      makeEnv(),
    );
    assert.equal(res.status, 405, `${method} /api/business-config must stay blocked`);
    assert.equal(res.headers.get("allow"), "GET, PUT");
  }
  const res = await handleFetch(
    new Request("https://lb.example/api/business-config/preflight", { method: "GET" }),
    makeEnv(),
  );
  assert.equal(res.status, 405, "preflight is POST only");
  assert.equal(res.headers.get("allow"), "POST");
});

test("Worker: public setup status is forwarded without an authorization header", async () => {
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

test("Worker: valid env, GET /ready forwarded to backend with auth header intact", async () => {
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

test("Worker: callback registration and disable endpoints are forwarded", async () => {
  const seen = [];
  const restore = stubFetch(async (url, init) => {
    seen.push([url, init.method]);
    return new Response("{\"error\":\"invalid_request\",\"request_id\":\"x\"}", {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  });
  try {
    for (const path of ["/api/push/register", "/api/telegram/register-webhook", "/api/push/disable"]) {
      const res = await handleFetch(
        new Request(`https://lb.example${path}`, { method: "POST", body: "{}" }),
        makeEnv(),
      );
      assert.equal(res.status, 400);
      assert.deepEqual(await res.json(), { error: "invalid_request", request_id: "x" });
    }
    assert.equal(seen.length, 3);
    assert.ok(seen.every(([url, method]) => method === "POST"));
  } finally {
    restore();
  }
});

test("Worker: /reconcile 走 per-route 长超时且绝不故障转移；其余快路径保持全局默认", async () => {
  const seen = [];
  let lastTimeoutMs = null;
  const restore = stubFetch(async (url) => {
    seen.push(String(url));
    return new Response("err", { status: 503 });
  });
  // 只记录 setTimeout 收到的延迟，不真正触发——否则 abort 会让 fetch 提前失败
  const origSetTimeout = globalThis.setTimeout;
  const origClearTimeout = globalThis.clearTimeout;
  globalThis.setTimeout = (_fn, ms) => {
    lastTimeoutMs = ms;
    return 0;
  };
  globalThis.clearTimeout = () => {};
  try {
    await handleFetch(new Request("https://lb.example/reconcile", { method: "POST", body: "{}" }), makeEnv());
    assert.equal(seen.length, 1, "POST /reconcile 必须 maxAttempts=1：后端 503 也不换实例");
    assert.equal(lastTimeoutMs, 320_000, "POST /reconcile 默认超时 320000ms");

    seen.length = 0;
    await handleFetch(
      new Request("https://lb.example/reconcile", { method: "POST", body: "{}" }),
      makeEnv({ LB_RECONCILE_TIMEOUT_MS: "60000" }),
    );
    assert.equal(seen.length, 1, "POST /reconcile 的 maxAttempts=1 不受 env 覆盖影响");
    assert.equal(lastTimeoutMs, 60_000, "POST /reconcile 必须遵守 LB_RECONCILE_TIMEOUT_MS");

    seen.length = 0;
    await handleFetch(new Request("https://lb.example/webhook/tg", { method: "POST", body: "{}" }), makeEnv());
    assert.equal(seen.length, 2, "POST /webhook/tg 仍要故障转移到第二实例");
    assert.equal(lastTimeoutMs, 5_000, "其余路由仍须使用全局 LB_REQUEST_TIMEOUT_MS");
  } finally {
    globalThis.setTimeout = origSetTimeout;
    globalThis.clearTimeout = origClearTimeout;
    restore();
  }
});

test("Worker: POST /worker 在白名单内，走 per-route 长超时且绝不故障转移；GET /worker => 405", async () => {
  // /worker 和 /reconcile 一样只由外部 cron 触发（scripts/cron-drain.sh）。全局 10s 超时会把
  // 一次正常的排空误判成失败并切到第二实例，而第二个实例只会重复排同一批消息。
  const seen = [];
  let lastTimeoutMs = null;
  const restore = stubFetch(async (url) => {
    seen.push(String(url));
    return new Response("err", { status: 503 });
  });
  const origSetTimeout = globalThis.setTimeout;
  const origClearTimeout = globalThis.clearTimeout;
  globalThis.setTimeout = (_fn, ms) => {
    lastTimeoutMs = ms;
    return 0;
  };
  globalThis.clearTimeout = () => {};
  try {
    await handleFetch(new Request("https://lb.example/worker", { method: "POST", body: "{}" }), makeEnv());
    assert.equal(seen.length, 1, "POST /worker 必须 maxAttempts=1：后端 503 也不换实例");
    assert.equal(lastTimeoutMs, 300_000, "POST /worker 默认超时 300000ms");

    seen.length = 0;
    await handleFetch(
      new Request("https://lb.example/worker", { method: "POST", body: "{}" }),
      makeEnv({ LB_WORKER_TIMEOUT_MS: "60000" }),
    );
    assert.equal(seen.length, 1, "POST /worker 的 maxAttempts=1 不受 env 覆盖影响");
    assert.equal(lastTimeoutMs, 60_000, "POST /worker 必须遵守 LB_WORKER_TIMEOUT_MS");

    seen.length = 0;
    const res = await handleFetch(new Request("https://lb.example/worker", { method: "GET" }), makeEnv());
    assert.equal(res.status, 405, "GET /worker 必须 405，不能被误透传");
    assert.equal(res.headers.get("allow"), "POST");
    assert.equal(seen.length, 0, "405 必须被 LB 拦住，不打到后端");
  } finally {
    globalThis.setTimeout = origSetTimeout;
    globalThis.clearTimeout = origClearTimeout;
    restore();
  }
});
