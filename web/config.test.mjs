import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
const source = readFileSync(new URL("./config.js", import.meta.url), "utf8");
const styles = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

class FakeElement {
  constructor(hidden = false) {
    this.hidden = hidden;
    this.disabled = false;
    this.required = false;
    this.checked = false;
    this.value = "";
    this.textContent = "";
    this.listeners = new Map();
    this.attributes = new Map();
    this.classList = {
      toggle() {},
      add() {},
      remove() {},
    };
    this.children = new Map();
    this.options = [];
  }

  addEventListener(type, callback) {
    this.listeners.set(type, callback);
  }

  setAttribute(name, value) {
    this.attributes.set(name, value);
  }

  querySelector(selector) {
    if (!this.children.has(selector)) this.children.set(selector, new FakeElement());
    return this.children.get(selector);
  }

  querySelectorAll() { return []; }
  appendChild(child) { this.options.push(child); }
  focus() {}
  reset() {}
  setCustomValidity() {}
  reportValidity() { return true; }
}

async function startPage(statusResponse) {
  const elements = new Map();
  for (const match of html.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/gs)) {
    const [, id] = match;
    const hidden = /\shidden(?:\s|=|>)/.test(match[0]);
    elements.set(id, new FakeElement(hidden));
  }
  const document = {
    querySelector(selector) {
      const match = selector.match(/^#(.+)$/);
      if (!match || !elements.has(match[1])) throw new Error(`Missing selector: ${selector}`);
      return elements.get(match[1]);
    },
    getElementById(id) {
      if (!elements.has(id)) throw new Error(`Missing element: ${id}`);
      return elements.get(id);
    },
    querySelectorAll() { return []; },
    createElement() { return new FakeElement(); },
  };
  const window = {
    addEventListener() {},
    setTimeout() { return 1; },
    clearTimeout() {},
  };
  const fetchCalls = [];
  const fetch = async (path, options) => {
    fetchCalls.push({ path, options });
    if (path !== "/api/status") throw new Error(`Unexpected request: ${path}`);
    if (statusResponse instanceof Error) throw statusResponse;
    const httpStatus = statusResponse.httpStatus ?? 200;
    return {
      ok: httpStatus >= 200 && httpStatus < 300,
      status: httpStatus,
      text: async () => JSON.stringify(statusResponse.body),
    };
  };

  vm.runInNewContext(source, { document, window, fetch, Headers, URL });
  await new Promise((resolve) => setImmediate(resolve));
  await Promise.resolve();
  return { elements, fetchCalls, window };
}

test("configuration-setup hides all authorization UI and lists only missing variable names", async () => {
  const { elements, fetchCalls } = await startPage({
    body: {
      ready: false,
      mode: "configuration-setup",
      missing: ["REDIS_URL", "CONFIG_ENCRYPTION_KEY", "secret-value-must-not-display"],
    },
  });

  assert.equal(elements.get("setup-card").hidden, false);
  assert.equal(elements.get("intro").hidden, true);
  assert.equal(elements.get("auth-card").hidden, true);
  assert.equal(elements.get("config-panel").hidden, true);
  assert.equal(elements.get("notice").hidden, true);
  assert.equal(elements.get("privacy-note").hidden, true);
  assert.equal(elements.get("setup-missing").textContent, "REDIS_URL, CONFIG_ENCRYPTION_KEY");
  assert.equal(elements.get("setup-message").textContent.includes("setup mode"), true);
  assert.equal(fetchCalls.length, 1);
  assert.equal(fetchCalls[0].path, "/api/status");
  assert.equal(fetchCalls[0].options.credentials, "omit");
  assert.equal(fetchCalls[0].options.cache, "no-store");
  assert.equal(fetchCalls[0].options.headers.has("authorization"), false);
});

test("ready status preserves the existing admin-session entry and configuration page", async () => {
  const { elements } = await startPage({
    body: { ready: true, mode: "configured", missing: [] },
  });

  assert.equal(elements.get("setup-card").hidden, true);
  assert.equal(elements.get("intro").hidden, false);
  assert.equal(elements.get("auth-card").hidden, false);
  assert.equal(elements.get("config-panel").hidden, true);
  assert.equal(elements.get("privacy-note").hidden, false);
  assert.equal(elements.get("notice").hidden, false);
});

test("status errors keep the authorization form hidden until readiness is confirmed", async () => {
  const { elements } = await startPage({
    httpStatus: 503,
    body: {},
  });

  assert.equal(elements.get("setup-card").hidden, false);
  assert.equal(elements.get("auth-card").hidden, true);
  assert.equal(elements.get("config-panel").hidden, true);
  assert.match(elements.get("setup-message").textContent, /authorization entry stays hidden/);
});

test("setup card inherits desktop layout width and follows the existing mobile layout width", () => {
  const setupRules = [...styles.matchAll(/\.setup-panel[^{}]*\{([^}]*)\}/g)].map((match) => match[1]);
  const setupDeclarations = setupRules.join("\n");

  assert.doesNotMatch(setupDeclarations, /\b(?:width|max-width)\s*:/);
  assert.match(styles, /\.layout\s*\{[^}]*width:\s*min\(960px,\s*calc\(100% - 40px\)\)/);
  assert.match(styles, /@media \(max-width: 720px\)\s*\{[\s\S]*?\.layout\s*\{[^}]*width:\s*calc\(100% - 32px\)/);
  assert.match(styles, /\.auth-panel\s*\{[^}]*padding:\s*27px 29px 25px/);
});

test("i18n defaults to English and keeps the zh/en dictionaries in key parity", async () => {
  const { window } = await startPage({
    body: { ready: true, mode: "configured", missing: [] },
  });

  const { detectLocale, getLocale, t, messages } = window.__mw;
  assert.equal(detectLocale(), "en");
  assert.equal(getLocale(), "en");

  const enKeys = Object.keys(messages.en).sort();
  const zhKeys = Object.keys(messages.zh).sort();
  assert.deepEqual(enKeys, zhKeys);
  assert.ok(enKeys.length > 100, `expected a broad message catalog, got ${enKeys.length}`);

  assert.equal(t("common.required"), "Required");
  assert.equal(
    t("session.expiry", { n: 5 }),
    "Admin session expires in about 5 minutes; kept only in page memory",
  );
  assert.equal(t("missing.key"), "missing.key");
});

test("every data-i18n key in the markup resolves in both dictionaries", async () => {
  const { window } = await startPage({
    body: { ready: true, mode: "configured", missing: [] },
  });
  const { messages } = window.__mw;

  const keys = new Set();
  for (const match of html.matchAll(/data-i18n(?:-placeholder|-aria)?="([^"]+)"/g)) {
    keys.add(match[1]);
  }
  assert.ok(keys.size > 40, `expected many i18n keys in the markup, got ${keys.size}`);

  for (const key of keys) {
    assert.ok(key in messages.en, `markup key missing from en dictionary: ${key}`);
    assert.ok(key in messages.zh, `markup key missing from zh dictionary: ${key}`);
  }

  assert.match(html, /<html lang="en">/);
  assert.match(html, /data-i18n-placeholder="auth\.placeholder"/);
  assert.match(html, /data-i18n-aria="brand\.aria"/);
});

test("timezone catalogue is a duplicate-free, fixed-offset-only set with Asia/Shanghai as the default", async () => {
  const { window } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const { timezones, defaultTimezone } = window.__mw;

  assert.equal(defaultTimezone, "Asia/Shanghai");
  assert.equal(timezones.length, 16);
  assert.equal(new Set(timezones.map((zone) => zone.iana)).size, 16);

  const expected = [
    "Etc/UTC",
    "Africa/Cairo",
    "Europe/Istanbul",
    "Africa/Nairobi",
    "Asia/Dubai",
    "Asia/Karachi",
    "Asia/Kolkata",
    "Asia/Bangkok",
    "Asia/Ho_Chi_Minh",
    "Asia/Shanghai",
    "Asia/Hong_Kong",
    "Asia/Taipei",
    "Asia/Singapore",
    "Asia/Manila",
    "Asia/Tokyo",
    "Asia/Seoul",
  ];
  assert.deepEqual([...timezones.map((zone) => zone.iana)].sort(), [...expected].sort());
  assert.ok(timezones.some((zone) => zone.iana === defaultTimezone));

  for (const zone of timezones) {
    assert.match(zone.offset, /^\+\d{1,2}(:30)?$/);
    assert.equal(zone.label.en.includes(zone.offset), true);
    assert.equal(zone.label.zh.includes(zone.offset), true);
  }
});

test("timezone select is filled with 16 options and pre-selects Asia/Shanghai", async () => {
  const { elements, window } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const { timezones, defaultTimezone, getLocale } = window.__mw;
  const timezoneSelect = elements.get("timezone");

  assert.equal(timezoneSelect.options.length, 16);
  assert.deepEqual(timezoneSelect.options.map((option) => option.value), Array.from(timezones, (zone) => zone.iana));

  const selected = timezoneSelect.options.filter((option) => option.selected);
  assert.equal(selected.length, 1);
  assert.equal(selected[0].value, defaultTimezone);

  const defaultZone = timezones.find((zone) => zone.iana === defaultTimezone);
  assert.equal(defaultZone.label.en, "Shanghai, China (UTC+8)");
  assert.equal(defaultZone.label.zh, "中国上海 (UTC+8)");
  assert.equal(selected[0].textContent, defaultZone.label[getLocale()]);
  // Option labels live per zone and must exist in both languages (test 5 covers dictionary parity).
  for (const zone of timezones) {
    assert.equal(zone.label.en.length > 0, true);
    assert.equal(zone.label.zh.length > 0, true);
  }
});

test("readBusinessConfig carries the selected time zone", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });

  elements.get("telegram-bot-token").value = "bot-token";
  elements.get("telegram-chat-id").value = "123456789";
  elements.get("chat-allowlist").value = "123456789";
  elements.get("telegram-webhook-secret").value = "webhook-secret";
  elements.get("jmap-session-url").value = "https://mail.example.com/jmap";
  elements.get("jmap-username").value = "bot@example.com";
  elements.get("jmap-password").value = "jmap-password";
  elements.get("reconcile-token").value = "reconcile-token";
  elements.get("worker-token").value = "worker-token";
  elements.get("timezone").value = "Asia/Tokyo";

  const payload = pageWindow.__mw.readBusinessConfig();

  assert.equal(payload.timezone, "Asia/Tokyo");
  assert.equal(payload.llm_enabled, false);
  assert.equal(payload.llm_base_url, null);
  assert.equal(payload.llm_model, null);
  assert.equal(payload.llm_api_key, null);
  assert.equal(typeof payload.bot_token, "string");
  assert.equal(typeof payload.reconcile_token, "string");
});
