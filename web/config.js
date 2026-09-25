(() => {
  "use strict";

  const RUNTIME_DEFAULTS = Object.freeze({
    jmap_timeout_ms: 15000,
    telegram_timeout_ms: 10000,
    llm_timeout_ms: 30000,
    max_retries: 3,
  });
  const RUNTIME_LIMITS = Object.freeze({
    jmap_timeout_ms: { min: 100, max: 300000 },
    telegram_timeout_ms: { min: 100, max: 300000 },
    llm_timeout_ms: { min: 100, max: 300000 },
    max_retries: { min: 0, max: 5 },
  });
  const RUNTIME_FIELDS = Object.freeze([
    ["jmap_timeout_ms", "jmap-timeout"],
    ["telegram_timeout_ms", "telegram-timeout"],
    ["llm_timeout_ms", "llm-timeout"],
    ["max_retries", "max-retries"],
  ]);
  const I64_MIN = -(2n ** 63n);
  const I64_MAX = (2n ** 63n) - 1n;

  const bootstrapInput = document.querySelector("#bootstrap-input");
  const authForm = document.querySelector("#auth-form");
  const intro = document.querySelector("#intro");
  const authCard = document.querySelector("#auth-card");
  const setupCard = document.querySelector("#setup-card");
  const setupMessage = document.querySelector("#setup-message");
  const setupMissing = document.querySelector("#setup-missing");
  const statusRetryButton = document.querySelector("#status-retry-button");
  const privacyNote = document.querySelector("#privacy-note");
  const configPanel = document.querySelector("#config-panel");
  const businessForm = document.querySelector("#business-form");
  const businessFields = document.querySelector("#business-fields");
  const runtimeForm = document.querySelector("#runtime-form");
  const runtimeFields = document.querySelector("#runtime-fields");
  const connectButton = document.querySelector("#connect-button");
  const reloadButton = document.querySelector("#reload-button");
  const logoutButton = document.querySelector("#logout-button");
  const defaultsButton = document.querySelector("#defaults-button");
  const businessSaveButton = document.querySelector("#business-save-button");
  const runtimeSaveButton = document.querySelector("#runtime-save-button");
  const businessSaveState = document.querySelector("#business-save-state");
  const runtimeSaveState = document.querySelector("#runtime-save-state");
  const serviceEnabled = document.querySelector("#service-enabled");
  const serviceEnabledState = document.querySelector("#service-enabled-state");
  const sessionExpiry = document.querySelector("#session-expiry");
  const llmEnabled = document.querySelector("#llm-enabled");
  const llmApiKey = document.querySelector("#llm-api-key");
  const llmBaseUrl = document.querySelector("#llm-base-url");
  const llmModel = document.querySelector("#llm-model");
  const notice = document.querySelector("#notice");

  // C-NO-LOCAL-WRITE / C-REDIS-ONLY-STATE: credentials and configuration stay in page memory.
  let adminSession = "";
  let expiryTimer = 0;
  let busyAction = "";
  let runtimeLoaded = false;
  let setupMode = true;

  class ApiError extends Error {
    constructor(status, message) {
      super(message);
      this.status = status;
    }
  }

  function showNotice(kind, message) {
    notice.hidden = false;
    notice.className = `notice notice-${kind}`;
    notice.textContent = message;
    notice.setAttribute("role", kind === "error" ? "alert" : "status");
  }

  function updateButtons() {
    const busy = Boolean(busyAction);
    connectButton.disabled = busy;
    reloadButton.disabled = busy || !adminSession;
    logoutButton.disabled = busy || !adminSession;
    businessSaveButton.disabled = busy || !adminSession;
    runtimeSaveButton.disabled = busy || !runtimeLoaded || !adminSession;
    defaultsButton.disabled = busy || !runtimeLoaded || !adminSession;
    businessSaveButton.classList.toggle("is-busy", busy && busyAction === "business-save");
    runtimeSaveButton.classList.toggle("is-busy", busy && busyAction === "runtime-save");
    connectButton.textContent = busy && busyAction === "login" ? "正在授权…" : "创建会话";
    reloadButton.textContent = busy && busyAction === "runtime-load" ? "正在读取…" : "重新读取运行参数";
    businessSaveButton.querySelector(".button-label").textContent = busy && busyAction === "business-save" ? "正在保存并热加载…" : "保存业务配置并热加载";
    runtimeSaveButton.querySelector(".button-label").textContent = busy && busyAction === "runtime-save" ? "正在保存…" : "保存运行参数";
  }

  function setBusy(action = "") {
    busyAction = action;
    updateButtons();
  }

  function parseResponseText(text, status) {
    if (status === 204 || text.trim() === "") return null;
    try {
      return JSON.parse(text);
    } catch {
      throw new Error("配置服务返回了无法识别的数据格式。");
    }
  }

  async function request(path, method = "GET", body = undefined, bearer = adminSession) {
    const headers = new Headers({ Accept: "application/json" });
    if (bearer) headers.set("Authorization", `Bearer ${bearer}`);
    const options = {
      method,
      headers,
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
      referrerPolicy: "no-referrer",
    };
    if (body !== undefined) {
      headers.set("Content-Type", "application/json");
      options.body = typeof body === "string" ? body : JSON.stringify(body);
    }

    let response;
    try {
      response = await fetch(path, options);
    } catch {
      throw new Error("无法连接配置服务。请确认服务可用后重试。");
    }
    const responseText = await response.text().catch(() => "");
    if (!response.ok) throw new ApiError(response.status, responseText);
    return parseResponseText(responseText, response.status);
  }

  async function showSetupStatus() {
    showSetupView("正在检查服务启动配置…");
    statusRetryButton.disabled = true;
    statusRetryButton.textContent = "正在检查…";
    try {
      const status = await request("/api/status", "GET", undefined, "");
      if (status && status.ready === true) {
        setupMode = false;
        setupCard.hidden = true;
        notice.hidden = false;
        intro.hidden = false;
        authCard.hidden = false;
        configPanel.hidden = true;
        privacyNote.hidden = false;
        showNotice("info", "使用 Redis 管理凭据创建短期管理会话。");
        return;
      }
      const isSetup = status && status.ready === false && status.mode === "configuration-setup";
      if (isSetup) {
        const knownMissing = Array.isArray(status.missing)
          ? status.missing.filter((name) => name === "REDIS_URL" || name === "CONFIG_ENCRYPTION_KEY")
          : [];
        const message = knownMissing.length
          ? "服务处于配置引导模式。请在部署环境补齐以下启动变量："
          : "服务处于配置引导模式，但状态接口未返回缺少的启动变量名称。";
        showSetupView(message, knownMissing);
        return;
      }
      showSetupView("无法确认服务是否已完成启动配置。管理授权入口保持隐藏，请重新检查服务状态。");
    } catch {
      showSetupView("暂时无法读取服务启动状态。为保护管理凭据，授权入口保持隐藏；请确认服务可用后重新检查。");
    } finally {
      statusRetryButton.disabled = false;
      statusRetryButton.textContent = "重新检查";
    }
  }

  async function loadEnabled() {
    const state = await request("/api/enabled");
    serviceEnabled.checked = state.enabled === true;
    serviceEnabledState.textContent = serviceEnabled.checked ? "业务入口已启用" : "业务入口已关闭（fail-closed）";
  }

  function showSetupView(message, missing = []) {
    setupMode = true;
    setupMessage.textContent = message;
    setupMissing.textContent = missing.join("、");
    setupMissing.hidden = missing.length === 0;
    setupCard.hidden = false;
    intro.hidden = true;
    notice.hidden = true;
    authCard.hidden = true;
    configPanel.hidden = true;
    privacyNote.hidden = true;
  }

  function showConfigView() {
    setupCard.hidden = true;
    intro.hidden = false;
    privacyNote.hidden = false;
    authCard.hidden = true;
    configPanel.hidden = false;
  }

  function showAuthView() {
    setupCard.hidden = true;
    intro.hidden = false;
    privacyNote.hidden = false;
    configPanel.hidden = true;
    authCard.hidden = setupMode;
    bootstrapInput.value = "";
    if (!setupMode) bootstrapInput.focus();
  }

  function clearSecretFields() {
    for (const input of document.querySelectorAll('input[type="password"]')) input.value = "";
  }

  function clearForms() {
    businessForm.reset();
    runtimeForm.reset();
    runtimeLoaded = false;
    runtimeFields.disabled = true;
    businessFields.disabled = true;
    runtimeSaveState.textContent = "读取成功后可编辑运行参数。";
    businessSaveState.textContent = "完成管理会话验证后可填写完整配置。";
    updateLLMRequirements();
  }

  function clearSession() {
    adminSession = "";
    if (expiryTimer) window.clearTimeout(expiryTimer);
    expiryTimer = 0;
    sessionExpiry.textContent = "会话仅保存在当前页面内存";
    clearForms();
    clearSecretFields();
    setBusy("");
    showAuthView();
  }

  function expireSession(message) {
    clearSession();
    showNotice("error", message);
  }

  function scheduleSessionExpiry(seconds) {
    if (expiryTimer) window.clearTimeout(expiryTimer);
    const duration = Math.max(1, Math.min(Number(seconds) || 900, 900));
    sessionExpiry.textContent = `会话约 ${Math.ceil(duration / 60)} 分钟后过期；只保存在页面内存`;
    expiryTimer = window.setTimeout(() => {
      expireSession("管理会话已过期，已清除页面中的会话和表单内容。请重新授权。");
    }, duration * 1000);
  }

  function normalizeRuntimeConfig(value) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      throw new Error("配置服务返回了无法识别的运行参数。");
    }
    const config = {};
    for (const [key] of RUNTIME_FIELDS) {
      const item = value[key];
      const limit = RUNTIME_LIMITS[key];
      if (!Number.isInteger(item) || item < limit.min || item > limit.max) {
        throw new Error("配置服务返回了超出范围的运行参数，请检查后端配置。");
      }
      config[key] = item;
    }
    return config;
  }

  function fillRuntimeFields(config) {
    for (const [key, id] of RUNTIME_FIELDS) document.getElementById(id).value = String(config[key]);
  }

  function showRuntimePreview() {
    fillRuntimeFields(RUNTIME_DEFAULTS);
    runtimeLoaded = false;
    runtimeFields.disabled = true;
    runtimeSaveState.textContent = "默认值预览；读取后才能保存。";
    updateButtons();
  }

  function activateRuntimeConfig(config) {
    fillRuntimeFields(config);
    runtimeLoaded = true;
    runtimeFields.disabled = false;
    runtimeSaveState.textContent = "已读取 Redis 中的运行参数。";
    runtimeSaveState.classList.remove("is-dirty");
    updateButtons();
  }

  function runtimeErrorMessage(error, phase) {
    if (error.status === 401) return "管理会话已失效，请重新授权。";
    if (error.status === 400) return "运行参数请求格式无效（HTTP 400）。";
    if (error.status === 422) return "运行参数超出允许范围（超时 100–300,000 毫秒，重试 0–5 次）。";
    if (error.status === 503) return phase === "save"
      ? "Redis 不可用或初始化尚未完成；保存状态无法确认。已禁用运行参数保存，请恢复连接后重新读取。"
      : "Redis 不可用或初始化尚未完成。当前只显示默认值预览，运行参数保存已禁用；请稍后重试。";
    if (error.status !== undefined) return `运行参数请求失败（HTTP ${error.status}）。请稍后重试。`;
    return error.message || "运行参数请求失败，请检查服务状态后重试。";
  }

  async function loadRuntimeConfig() {
    if (!adminSession || busyAction) return;
    showRuntimePreview();
    setBusy("runtime-load");
    showNotice("info", "正在从 Redis 读取运行参数…");
    try {
      const config = normalizeRuntimeConfig(await request("/api/config"));
      activateRuntimeConfig(config);
      showNotice("success", "运行参数已读取。业务配置仍需手动完整填写。");
    } catch (error) {
      if (error.status === 401) {
        expireSession("管理会话已失效，页面内容已清除。请重新创建会话。");
        return;
      }
      runtimeLoaded = false;
      runtimeFields.disabled = true;
      runtimeSaveState.textContent = "读取未成功，保存已禁用。";
      updateButtons();
      showNotice("error", runtimeErrorMessage(error, "load"));
    } finally {
      if (adminSession) setBusy("");
    }
  }

  function setFieldError(input, message) {
    input.setCustomValidity(message);
    input.reportValidity();
    input.focus();
    return null;
  }

  function clearCustomValidity(form) {
    for (const input of form.querySelectorAll("input, textarea")) input.setCustomValidity("");
  }

  function isBlank(value) {
    return value.trim() === "";
  }

  function parseI64(input, label) {
    const value = input.value.trim();
    if (!/^-?\d+$/.test(value)) {
      setFieldError(input, `${label} 必须是十进制整数。`);
      throw new Error("invalid-field");
    }
    const parsed = BigInt(value);
    if (parsed < I64_MIN || parsed > I64_MAX) {
      setFieldError(input, `${label} 超出有符号 64 位整数范围。`);
      throw new Error("invalid-field");
    }
    return parsed;
  }

  function parseAllowlist(input) {
    const entries = input.value.split(/[\s,]+/).filter(Boolean);
    if (entries.length === 0) {
      setFieldError(input, "至少填写一个 chat ID。空白和逗号均可用作分隔符。");
      throw new Error("invalid-field");
    }
    const ids = [];
    const seen = new Set();
    for (const entry of entries) {
      if (!/^-?\d+$/.test(entry)) {
        setFieldError(input, `白名单中的“${entry}”不是整数 chat ID。`);
        throw new Error("invalid-field");
      }
      const id = BigInt(entry);
      if (id < I64_MIN || id > I64_MAX) {
        setFieldError(input, `白名单中的“${entry}”超出有符号 64 位整数范围。`);
        throw new Error("invalid-field");
      }
      if (seen.has(id.toString())) {
        setFieldError(input, `白名单中有重复的 chat ID：${entry}。`);
        throw new Error("invalid-field");
      }
      seen.add(id.toString());
      ids.push(id);
    }
    return ids;
  }

  function validateHttpsUrl(input, label, { noCredentials = false, noQuery = false } = {}) {
    let parsed;
    try {
      parsed = new URL(input.value.trim());
    } catch {
      setFieldError(input, `${label} 必须是有效的 HTTPS URL。`);
      throw new Error("invalid-field");
    }
    if (parsed.protocol !== "https:" || !parsed.hostname) {
      setFieldError(input, `${label} 必须使用 HTTPS。`);
      throw new Error("invalid-field");
    }
    if (noCredentials && (parsed.username || parsed.password)) {
      setFieldError(input, `${label} 不能包含用户名或密码。`);
      throw new Error("invalid-field");
    }
    const rawValue = input.value.trim();
    if (noQuery && (rawValue.includes("?") || rawValue.includes("#"))) {
      setFieldError(input, `${label} 不能包含查询参数或 #片段。`);
      throw new Error("invalid-field");
    }
    return input.value.trim();
  }

  function requireText(id, label, secret = false) {
    const input = document.getElementById(id);
    const value = secret ? input.value : input.value.trim();
    if (isBlank(value)) {
      setFieldError(input, `${label} 不能为空。`);
      throw new Error("invalid-field");
    }
    return value;
  }

  function readBusinessConfig() {
    clearCustomValidity(businessForm);
    const enabled = llmEnabled.checked;
    llmApiKey.required = enabled;
    llmBaseUrl.required = enabled;
    llmModel.required = enabled;

    try {
      const llmBaseValue = llmBaseUrl.value.trim();
      const llmModelValue = llmModel.value.trim();
      const llmKeyValue = llmApiKey.value;
      if (enabled && !llmBaseValue) return setFieldError(llmBaseUrl, "启用 LLM 时必须填写 Base URL。");
      if (enabled && !llmModelValue) return setFieldError(llmModel, "启用 LLM 时必须填写模型名称。 ");
      if (enabled && isBlank(llmKeyValue)) return setFieldError(llmApiKey, "启用 LLM 时必须填写 API key。");
      const llmBase = llmBaseValue
        ? (enabled ? validateHttpsUrl(llmBaseUrl, "LLM Base URL") : llmBaseValue)
        : null;

      const jmapUrlInput = document.getElementById("jmap-session-url");
      const business = {
        bot_token: requireText("telegram-bot-token", "Telegram Bot Token", true),
        telegram_chat_id: parseI64(document.getElementById("telegram-chat-id"), "目标 Chat ID"),
        chat_allowlist: parseAllowlist(document.getElementById("chat-allowlist")),
        telegram_webhook_secret: requireText("telegram-webhook-secret", "Telegram Webhook Secret", true),
        jmap_session_url: validateHttpsUrl(jmapUrlInput, "JMAP Session URL", { noCredentials: true, noQuery: true }),
        jmap_username: requireText("jmap-username", "邮箱用户名"),
        jmap_password: requireText("jmap-password", "JMAP 应用专用密码", true),
        jmap_push_verification: requireText("jmap-push-verification", "JMAP Push 验证码", true),
        account_id: document.getElementById("account-id").value.trim() || null,
        llm_enabled: enabled,
        llm_allow_net: document.getElementById("llm-allow-net").checked,
        llm_api_key: isBlank(llmKeyValue) ? null : llmKeyValue,
        llm_base_url: llmBase,
        llm_model: llmModelValue || null,
        reconcile_token: requireText("reconcile-token", "Reconcile Token", true),
        worker_token: requireText("worker-token", "Worker Token", true),
      };
      return business;
    } catch (error) {
      if (error.message === "invalid-field") return null;
      throw error;
    }
  }

  function stringifyWithBigInt(value) {
    if (typeof value === "bigint") return value.toString();
    if (Array.isArray(value)) return `[${value.map(stringifyWithBigInt).join(",")}]`;
    if (value !== null && typeof value === "object") {
      return `{${Object.entries(value).map(([key, item]) => `${JSON.stringify(key)}:${stringifyWithBigInt(item)}`).join(",")}}`;
    }
    return JSON.stringify(value);
  }

  function businessErrorMessage(error) {
    if (error.status === 401) return "管理会话已失效。页面已清除会话和表单内容，请重新授权。";
    if (error.status === 400) return "后端无法解析提交的 JSON（HTTP 400）；请检查字段后重试。";
    if (error.status === 422) return "业务配置校验失败（HTTP 422）。请检查必填项、HTTPS URL、LLM 设置和 chat ID 白名单。服务端没有返回密钥或错误详情。";
    if (error.status === 503) return "业务配置未获成功确认（HTTP 503）。Redis 可能尚未就绪，或后端无法建立 JMAP/LLM 客户端；页面保留本次输入供检查和重试。";
    if (error.status !== undefined) return `业务配置请求失败（HTTP ${error.status}）。请稍后重试。`;
    return `${error.message || "业务配置请求失败。"} 请求结果可能无法确认；检查后可重新提交完整配置。`;
  }

  function runtimePayloadFromForm() {
    clearCustomValidity(runtimeForm);
    const config = {};
    for (const [key, id] of RUNTIME_FIELDS) {
      const input = document.getElementById(id);
      const value = input.value.trim() === "" ? Number.NaN : Number(input.value);
      const limit = RUNTIME_LIMITS[key];
      if (!Number.isInteger(value)) return setFieldError(input, "请输入整数。"), null;
      if (value < limit.min || value > limit.max) {
        return setFieldError(input, `请输入 ${limit.min} 到 ${limit.max} 之间的整数。`), null;
      }
      config[key] = value;
    }
    return config;
  }

  function updateLLMRequirements() {
    const enabled = llmEnabled.checked;
    llmApiKey.required = enabled;
    llmBaseUrl.required = enabled;
    llmModel.required = enabled;
    document.querySelector("#llm-key-required").textContent = enabled ? "必填" : "可选";
    document.querySelector("#llm-url-required").textContent = enabled ? "必填" : "可选";
    document.querySelector("#llm-model-required").textContent = enabled ? "必填" : "可选";
  }

  authForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (busyAction) return;
    let bootstrapCredential = bootstrapInput.value;
    bootstrapInput.value = "";
    if (isBlank(bootstrapCredential)) {
      showNotice("error", "请输入 Redis ACL 管理凭据。输入不会写入浏览器存储。");
      bootstrapInput.focus();
      return;
    }

    setBusy("login");
    showNotice("info", "正在创建短期管理会话…");
    try {
      const sessionInfo = await request("/api/admin/session", "POST", undefined, bootstrapCredential);
      bootstrapCredential = "";
      if (!sessionInfo || typeof sessionInfo.session !== "string" || !sessionInfo.session) {
        throw new Error("管理接口返回了无效的会话响应。");
      }
      adminSession = sessionInfo.session;
      scheduleSessionExpiry(sessionInfo.expires_in);
      showConfigView();
      businessFields.disabled = false;
      businessSaveState.textContent = "完整填写 BusinessConfigWire 后可提交。密钥不会回显。";
      showRuntimePreview();
      setBusy("");
      showNotice("success", "管理会话已创建。运行参数正在读取；业务配置需要完整重新填写。 ");
      await loadRuntimeConfig();
      await loadEnabled();
    } catch (error) {
      bootstrapCredential = "";
      setBusy("");
      if (error.status === 401) showNotice("error", "Redis 管理凭据无效，或服务尚未启用管理会话。请检查 Redis URL ACL 密码。 ");
      else if (error.status === 503) showNotice("error", "Redis 不可用或初始化尚未完成，暂时无法创建管理会话。请稍后重试。 ");
      else if (error.status !== undefined) showNotice("error", `无法创建管理会话（HTTP ${error.status}）。`);
      else showNotice("error", error.message || "无法连接管理服务，请检查网络后重试。");
      bootstrapInput.focus();
    }
  });

  reloadButton.addEventListener("click", loadRuntimeConfig);

  serviceEnabled.addEventListener("change", async () => {
    if (!adminSession || busyAction) return;
    const desired = serviceEnabled.checked;
    serviceEnabled.disabled = true;
    try {
      await request("/api/enabled", "PUT", { enabled: desired });
      serviceEnabledState.textContent = desired ? "业务入口已启用" : "业务入口已关闭（fail-closed）";
      showNotice("success", desired ? "业务处理已启用。" : "业务处理已关闭，入口将返回未启用状态。 ");
    } catch (error) {
      serviceEnabled.checked = !desired;
      serviceEnabledState.textContent = serviceEnabled.checked ? "业务入口已启用" : "业务入口已关闭（fail-closed）";
      showNotice("error", error.status === 401 ? "管理会话已失效，请重新授权。" : "无法保存业务开关状态。 ");
    } finally {
      serviceEnabled.disabled = false;
    }
  });

  logoutButton.addEventListener("click", async () => {
    if (!adminSession || busyAction) return;
    setBusy("logout");
    const session = adminSession;
    try {
      await request("/api/admin/session/revoke", "POST", undefined, session);
    } catch {
      // Local credentials are cleared even when the revocation request cannot reach Redis.
    }
    clearSession();
    showNotice("info", "已退出；管理会话和表单中的密钥已从页面内存清除。");
  });

  statusRetryButton.addEventListener("click", showSetupStatus);

  defaultsButton.addEventListener("click", () => {
    if (!runtimeLoaded || busyAction) return;
    fillRuntimeFields(RUNTIME_DEFAULTS);
    runtimeSaveState.textContent = "已恢复默认预览值；保存后才会写入 Redis。";
    runtimeSaveState.classList.add("is-dirty");
    showNotice("info", "运行参数已恢复为默认值预览。点击“保存运行参数”后才会更新 Redis。");
  });

  businessForm.addEventListener("input", (event) => {
    event.target.setCustomValidity("");
    if (!adminSession) return;
    businessSaveState.textContent = "表单包含未提交内容；每次提交会完整替换现有业务配置。";
    businessSaveState.classList.add("is-dirty");
  });

  runtimeForm.addEventListener("input", (event) => {
    event.target.setCustomValidity("");
    if (!runtimeLoaded) return;
    runtimeSaveState.textContent = "有尚未保存的运行参数修改。";
    runtimeSaveState.classList.add("is-dirty");
  });

  llmEnabled.addEventListener("change", updateLLMRequirements);

  businessForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!adminSession || busyAction) return;
    const config = readBusinessConfig();
    if (!config) return;
    setBusy("business-save");
    showNotice("info", "正在提交完整业务配置；后端会先构建客户端，再保存并切换运行实例…");
    try {
      await request("/api/business-config", "PUT", stringifyWithBigInt(config));
      clearSecretFields();
      businessSaveState.textContent = "业务配置已保存，新配置已热加载。";
      businessSaveState.classList.remove("is-dirty");
      showNotice("success", "业务配置已保存并热加载。密钥输入已清空；下次完整替换时需要重新填写。");
      updateButtons();
    } catch (error) {
      if (error.status === 401) {
        expireSession("管理会话已失效，已清除页面中的会话与配置。请重新授权。");
        return;
      }
      businessSaveState.textContent = "未收到保存成功确认；请检查服务状态。";
      showNotice("error", businessErrorMessage(error));
    } finally {
      if (adminSession) setBusy("");
    }
  });

  runtimeForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!adminSession || !runtimeLoaded || busyAction) return;
    const config = runtimePayloadFromForm();
    if (!config) return;
    setBusy("runtime-save");
    showNotice("info", "正在保存运行参数到 Redis…");
    try {
      const saved = normalizeRuntimeConfig(await request("/api/config", "PUT", config));
      activateRuntimeConfig(saved);
      showNotice("success", "运行参数已保存到 Redis，并立即应用于后续请求。");
    } catch (error) {
      if (error.status === 401) {
        expireSession("管理会话已失效，已清除页面中的会话与配置。请重新授权。");
        return;
      }
      if (error.status >= 500 || error.status === undefined) {
        runtimeLoaded = false;
        runtimeFields.disabled = true;
        runtimeSaveState.textContent = "保存状态无法确认；重新读取成功前已禁用保存。";
        updateButtons();
      }
      showNotice("error", runtimeErrorMessage(error, "save"));
    } finally {
      if (adminSession) setBusy("");
    }
  });

  window.addEventListener("pagehide", () => {
    adminSession = "";
    if (expiryTimer) window.clearTimeout(expiryTimer);
    clearForms();
    clearSecretFields();
    configPanel.hidden = true;
    authCard.hidden = setupMode;
    bootstrapInput.value = "";
    setBusy("");
  });

  showRuntimePreview();
  showSetupStatus();
  updateLLMRequirements();
  updateButtons();
})();
