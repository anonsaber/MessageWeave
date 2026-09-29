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
    const classes = new Set();
    this.classList = {
      toggle(name, force) {
        const on = force === true || (force === undefined && !classes.has(name));
        if (on) classes.add(name); else classes.delete(name);
      },
      add(name) { classes.add(name); },
      remove(name) { classes.delete(name); },
      contains(name) { return classes.has(name); },
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

// Objects built inside vm.runInNewContext carry that realm's Object.prototype,
// so assert.deepEqual — which compares prototypes — rejects them even when they
// are structurally identical. Re-shape them into host objects, sorted, and then
// compare.
function canonical(value) {
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return Array.from(value, (item) => canonical(item));
  if (value !== null && typeof value === "object") {
    const out = {};
    for (const key of Object.keys(value).sort()) out[key] = canonical(value[key]);
    return out;
  }
  return value;
}

function assertShape(actual, expected, message) {
  assert.deepEqual(canonical(actual), canonical(expected), message);
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

test("readback pre-fills stored values, blanks secrets, and marks which secrets exist", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;

  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      llm_base_url: "https://llm.example.com/v1",
      llm_model: "gpt-4o",
      chat_allowlist: ["123456789", "987654321"],
      telegram_chat_id: "123456789",
      llm_enabled: true,
      llm_allow_net: true,
    },
    secrets_present: {
      bot_token: true,
      jmap_password: true,
      telegram_webhook_secret: true,
      reconcile_token: true,
      worker_token: false,
      llm_api_key: true,
    },
  });

  // Non-secret values are filled in so the operator can change only what matters.
  assert.equal(elements.get("jmap-session-url").value, "https://mail.example.com/jmap");
  assert.equal(elements.get("jmap-username").value, "bot@example.com");
  assert.equal(elements.get("timezone").value, "Asia/Tokyo");
  assert.equal(elements.get("llm-base-url").value, "https://llm.example.com/v1");
  assert.equal(elements.get("llm-model").value, "gpt-4o");
  assert.equal(elements.get("llm-enabled").checked, true);
  assert.equal(elements.get("llm-allow-net").checked, true);
  // The allowlist is re-serialised comma-separated so parseAllowlist round-trips it.
  assert.equal(elements.get("chat-allowlist").value, "123456789, 987654321");
  assert.equal(elements.get("telegram-chat-id").value, "123456789");

  // Secrets are never echoed: every secret input is blank and no longer required,
  // because a blank secret now means "keep the stored value".
  const secrets = mw.fields.secrets;
  for (const field of secrets) {
    assert.equal(elements.get(field.id).value, "", `secret ${field.key} must stay blank`);
    assert.equal(elements.get(field.id).required, false, `secret ${field.key} must not be required`);
  }

  // Presence markers are driven by secrets_present, never by the stored value.
  const marker = (key) => elements.get(`secret-marker-${key}`);
  const marked = (key) => marker(key).textContent;
  assert.equal(marker("jmap_password").classList.contains("is-unset"), false);
  assert.equal(marker("worker_token").classList.contains("is-unset"), true);
  assert.notEqual(marked("jmap_password"), marked("worker_token"));
  assert.equal(marked("jmap_password").includes("Set"), true);
  assert.equal(marked("worker_token").includes("Not set"), true);

  assert.equal(mw.businessBaseline.jmap_session_url, "https://mail.example.com/jmap");
  assert.equal(mw.businessSecretPresence.worker_token, false);
});

test("no saved configuration leaves the baseline null so full submission is used", async () => {
  const { window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  assert.equal(pageWindow.__mw.businessBaseline, null);
  assert.equal(pageWindow.__mw.businessSecretPresence, null);
});

test("an untouched form produces an empty patch", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789"],
      telegram_chat_id: "123456789",
      llm_enabled: false,
      llm_allow_net: false,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: false },
  });

  const patch = mw.businessPatchFromForm();
  assertShape(patch, {}, "an unchanged form must not resubmit anything");
});

test("only changed text fields enter the patch", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789"],
      telegram_chat_id: "123456789",
      llm_enabled: false,
      llm_allow_net: false,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: false },
  });

  elements.get("timezone").value = "America/New_York";
  const patch = mw.businessPatchFromForm();
  assertShape(patch, { timezone: "America/New_York" });
});

test("blank secrets are omitted and typed secrets replace the stored value", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789"],
      telegram_chat_id: "123456789",
      llm_enabled: false,
      llm_allow_net: false,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: false },
  });

  // Every secret is blank after readback, so nothing is submitted for them.
  assertShape(mw.businessPatchFromForm(), {});

  // Typing a new value replaces the stored one. Assigning .value directly does
  // not fire the input event the app listens to, so re-render the marker by hand.
  elements.get("jmap-password").value = "new-password";
  mw.renderSecretMarkers();
  assertShape(mw.businessPatchFromForm(), { jmap_password: "new-password" });

  // The presence marker flips to "will replace" while the input is non-blank.
  assert.equal(elements.get("secret-marker-jmap_password").textContent.includes("replace"), true);

  // Clearing the input again restores "keep the stored value".
  elements.get("jmap-password").value = "";
  mw.renderSecretMarkers();
  assertShape(mw.businessPatchFromForm(), {});
  assert.equal(elements.get("secret-marker-jmap_password").textContent.includes("stays"), true);
});

test("clearing an optional LLM field submits null", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789"],
      telegram_chat_id: "123456789",
      llm_base_url: "https://llm.example.com/v1",
      llm_model: "gpt-4o",
      llm_enabled: true,
      llm_allow_net: true,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: true },
  });

  elements.get("llm-model").value = "";
  const patch = mw.businessPatchFromForm();
  assertShape(patch, { llm_model: null });
});

test("the allowlist is compared as an unordered set", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789", "987654321"],
      telegram_chat_id: "123456789",
      llm_enabled: false,
      llm_allow_net: false,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: false },
  });

  // Reordering the same ids is not a change.
  elements.get("chat-allowlist").value = "987654321 123456789";
  assertShape(mw.businessPatchFromForm(), {});

  // Adding an id is. The patch carries the operator's current order, not the
  // stored order, and plain strings rather than BigInt objects.
  elements.get("chat-allowlist").value = "987654321, 123456789, 555000000";
  const patch = mw.businessPatchFromForm();
  assertShape(patch, { chat_allowlist: ["987654321", "123456789", "555000000"] });

  // A blank textarea keeps the stored list rather than deleting it.
  elements.get("chat-allowlist").value = "";
  assertShape(mw.businessPatchFromForm(), {});
});

test("flipping a boolean enters the patch and resetBusinessReadback clears the baseline", async () => {
  const { elements, window: pageWindow } = await startPage({ body: { ready: true, mode: "configured", missing: [] } });
  const mw = pageWindow.__mw;
  mw.applyBusinessReadback({
    values: {
      jmap_session_url: "https://mail.example.com/jmap",
      jmap_username: "bot@example.com",
      timezone: "Asia/Tokyo",
      chat_allowlist: ["123456789"],
      telegram_chat_id: "123456789",
      llm_enabled: true,
      llm_allow_net: true,
    },
    secrets_present: { bot_token: true, jmap_password: true, telegram_webhook_secret: true, reconcile_token: true, worker_token: true, llm_api_key: true },
  });

  elements.get("llm-allow-net").checked = false;
  assertShape(mw.businessPatchFromForm(), { llm_allow_net: false });

  mw.resetBusinessReadback();
  assert.equal(mw.businessBaseline, null);
  assert.equal(mw.businessSecretPresence, null);
  // Markers fall back to "not set", so the operator knows the page has no view.
  assert.equal(elements.get("secret-marker-bot_token").textContent.includes("Not set"), true);
});

