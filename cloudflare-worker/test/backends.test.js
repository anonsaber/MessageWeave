/**
 * 单元测试：origin configuration 校验（C-HTTPS-INBOUND / C-LB-SINGLE-REG-URL）。
 * 运行：cd cloudflare-worker && node --test
 */
import test from "node:test";
import assert from "node:assert/strict";
import { parseBackendOrigin, parseBackendOrigins, SAFE_ROUTES, PROXIED_ROUTES } from "../src/backends.js";

test("http origin is rejected (https-only, fail-closed)", () => {
  assert.throws(() => parseBackendOrigin("http://bot.example"), /https/);
});

test("non-https / invalid origins are rejected", () => {
  for (const bad of ["", "  ", "not a url", "://x", "ws://bot.example", "wss://bot.example", "http://x"]) {
    assert.throws(() => parseBackendOrigin(bad), Error, `must reject: ${deep(bad)}`);
  }
});

test("embedded credentials / query / fragment / path are rejected", () => {
  assert.throws(() => parseBackendOrigin("https://user:pass@bot.example"), /credentials/);
  assert.throws(() => parseBackendOrigin("https://bot.example?x=1"), /query|fragment/);
  assert.throws(() => parseBackendOrigin("https://bot.example#frag"), /query|fragment/);
  assert.throws(() => parseBackendOrigin("https://bot.example/api"), /bare origin/);
});

test("valid https origins normalize to origin form", () => {
  const a = parseBackendOrigin("https://bot.example/");
  assert.equal(a.url, "https://bot.example");
  assert.equal(a.host, "bot.example");
  assert.equal(a.port, 443);
  const b = parseBackendOrigin("https://bot.example:8443");
  assert.equal(b.url, "https://bot.example:8443");
  assert.equal(b.port, 8443);
});

test("BACKEND_ORIGINS_JSON parses, dedups, validates all entries", () => {
  const raw = JSON.stringify(["https://a.example", "https://b.example", "https://a.example"]);
  const list = parseBackendOrigins(raw);
  assert.deepEqual(list.map((o) => o.url), ["https://a.example", "https://b.example"]);
  assert.throws(() => parseBackendOrigins(JSON.stringify(["https://a.example", "http://c.example"])), /https/);
  assert.throws(() => parseBackendOrigins("nope"), /valid JSON/);
  assert.throws(() => parseBackendOrigins("[]"), /non-empty/);
});

test("route safelist excludes /healthz and unknown paths (C-LB-SINGLE-REG-URL)", () => {
  for (const r of [
    "/", "/assets/config.js", "/assets/styles.css", "/api/status", "/ready", "/webhook/tg", "/push/jmap", "/reconcile",
    "/api/config", "/api/business-config", "/api/admin/session", "/api/admin/session/revoke",
  ]) {
    assert.ok(PROXIED_ROUTES.includes(r), `proxied: ${r}`);
  }
  for (const r of ["/healthz", "/admin", "/debug", "/jmap/session", "/ready/extra"]) {
    assert.ok(!PROXIED_ROUTES.includes(r), `must be blocked: ${r}`);
    assert.ok(!SAFE_ROUTES.includes(r), `must not be a safe route: ${r}`);
  }
});

/** 让断言失败信息可读，且不泄漏敏感值。 */
function deep(v) {
  return typeof v === "string" && v.length > 48 ? `${v.slice(0, 12)}…(${v.length})` : String(v);
}
