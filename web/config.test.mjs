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

  vm.runInNewContext(source, { document, window, fetch, Headers });
  await new Promise((resolve) => setImmediate(resolve));
  await Promise.resolve();
  return { elements, fetchCalls };
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
  assert.equal(elements.get("setup-missing").textContent, "REDIS_URL、CONFIG_ENCRYPTION_KEY");
  assert.equal(elements.get("setup-message").textContent.includes("配置引导模式"), true);
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
  assert.match(elements.get("setup-message").textContent, /授权入口保持隐藏/);
});

test("setup card inherits desktop layout width and follows the existing mobile layout width", () => {
  const setupRules = [...styles.matchAll(/\.setup-panel[^{}]*\{([^}]*)\}/g)].map((match) => match[1]);
  const setupDeclarations = setupRules.join("\n");

  assert.doesNotMatch(setupDeclarations, /\b(?:width|max-width)\s*:/);
  assert.match(styles, /\.layout\s*\{[^}]*width:\s*min\(960px,\s*calc\(100% - 40px\)\)/);
  assert.match(styles, /@media \(max-width: 720px\)\s*\{[\s\S]*?\.layout\s*\{[^}]*width:\s*calc\(100% - 32px\)/);
  assert.match(styles, /\.auth-panel\s*\{[^}]*padding:\s*27px 29px 25px/);
});
