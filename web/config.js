(() => {
  "use strict";

  // ─────────────────────────────────────────────────────────────────────────
  // I18N — browser-language detection, zh/en message dictionaries, t() helper.
  // Stays vanilla JS; no build step, no extra asset file (src/web.rs only serves
  // config.js + styles.css). HTML holds English default text + data-i18n attrs;
  // applyI18n() swaps in Chinese when the browser prefers zh. Default: English.
  // ─────────────────────────────────────────────────────────────────────────
  const MESSAGES = Object.freeze({
    en: {
      "app.title": "Service Configuration · MessageWeave",
      "brand.aria": "MessageWeave configuration home",
      "brand.tagline": "Service configuration",
      "topbar.note": "Secure configuration channel",
      "intro.eyebrow": "Service management",
      "intro.title": "Manage your service connections",
      "intro.copy": "Configure JMAP, Telegram, LLM, access allowlist and runtime parameters. Business configuration is stored in external Redis and hot-reloaded immediately after a successful save.",
      "notice.bootstrap": "Create a short-lived admin session using Redis management credentials.",
      "setup.eyebrow": "Setup guide",
      "setup.title": "Service startup configuration",
      "setup.checking": "Checking service startup configuration…",
      "setup.checkingBtn": "Checking…",
      "setup.privacy": "Startup secrets must be configured in the deployment environment. Secrets are never saved or echoed in the page.",
      "setup.retry": "Re-check",
      "setup.modeMissing": "The service is in setup mode. Provide the following startup variables in the deployment environment:",
      "setup.modeNoMissing": "The service is in setup mode, but the status endpoint did not return the missing startup variable names.",
      "setup.unknown": "Unable to confirm whether the service has completed startup configuration. The authorization entry stays hidden; re-check the service status.",
      "setup.fetchError": "Unable to read the service startup status right now. To protect management credentials, the authorization entry stays hidden; confirm the service is available and re-check.",
      "auth.eyebrow": "Authorization required",
      "auth.title": "Create admin session",
      "auth.bodyPre": "Enter the password of the Redis ACL user from the deployment ",
      "auth.bodyPost": ". The server issues a 15-minute admin session; the password and session exist only in the current page memory.",
      "auth.label": "Redis management credential",
      "auth.placeholder": "Enter the ACL password from the Redis URL",
      "auth.submit": "Create session",
      "auth.submitBusy": "Authorizing…",
      "auth.hint": "Never written to localStorage, sessionStorage or cookies. You must re-authorize after closing or refreshing the page.",
      "config.aria": "Business configuration and runtime parameters",
      "session.active": "Admin session active",
      "session.expiryDefault": "Session kept only in current page memory",
      "session.expiry": "Admin session expires in about {n} minutes; kept only in page memory",
      "session.expired": "The admin session has expired. The session and form contents have been cleared from the page. Please re-authorize.",
      "session.revoked": "Signed out; the admin session and secrets in the form have been cleared from page memory.",
      "session.invalid": "The admin session is no longer valid. The page contents have been cleared. Please recreate the session.",
      "session.invalidBusiness": "The admin session is no longer valid. The session and configuration have been cleared from the page. Please re-authorize.",
      "service.enable": "Enable business processing",
      "service.loading": "Loading…",
      "service.enabled": "Business entry enabled",
      "service.disabled": "Business entry disabled (fail-closed)",
      "service.enabledToast": "Business processing enabled.",
      "service.disabledToast": "Business processing disabled; the entry will return a not-enabled state.",
      "service.saveFail": "Unable to save the business toggle state.",
      "reload.button": "Reload runtime parameters",
      "reload.busy": "Loading…",
      "logout.button": "Sign out and clear session",
      "business.eyebrow": "Full replacement",
      "business.title": "Business connections and auth",
      "business.persist": "Encrypted in Redis",
      "callout.title": "Business configuration is not read from the server.",
      "callout.bodyPre": " To avoid secret echo, the API does not expose a business configuration GET. Every submission must re-fill the complete configuration, including all required secrets; optional account ID and LLM fields are sent as ",
      "callout.bodyPost": " when left blank.",
      "legend.business": "Full business configuration",
      "jmap.title": "JMAP mailbox",
      "jmap.desc": "The server must use HTTPS; the URL must not contain credentials, query parameters or fragments.",
      "jmap.urlHint": "HTTPS address without username, password, query parameters or #fragment.",
      "jmap.userLabel": "Email username",
      "jmap.passLabel": "App-specific password",
      "jmap.accountPh": "Leave blank to use the session primary account",
      "secret.reenter": "Re-enter on each full save",
      "telegram.title": "Telegram and access auth",
      "telegram.desc": "Only chat IDs in the allowlist may trigger bot operations.",
      "telegram.chatLabel": "Target Chat ID",
      "telegram.chatPh": "e.g. -1001234567890",
      "telegram.chatHint": "Accepts a full signed 64-bit integer.",
      "telegram.allowPh": "One chat ID per line or comma-separated",
      "telegram.allowHint": "Integer list; format and duplicates are checked before saving.",
      "telegram.workerPh": "Used by /worker and compatible admin APIs; re-enter on each full save",
      "telegram.workerHint": "This value differs from the current admin session. The page never reads or echoes an existing token.",
      "llm.title": "LLM service",
      "llm.desc": "When enabled, an HTTPS Base URL, model name and API key are required.",
      "llm.enableLabel": "Enable LLM",
      "llm.enableHint": "Allow the bot to call OpenAI-compatible model services.",
      "llm.netLabel": "Allow network calls",
      "llm.netHint": "When off, no LLM client is created.",
      "llm.urlHint": "HTTPS is required when LLM is enabled.",
      "llm.modelLabel": "Model name",
      "llm.modelPh": "Leave blank for null",
      "llm.keyPh": "Required when LLM is enabled; never echoed",
      "business.saveStateInitial": "You can fill in the full configuration after the admin session is verified.",
      "business.saveStateReady": "Fill in the complete BusinessConfigWire to submit. Secrets are never echoed.",
      "business.save": "Save business config and hot-reload",
      "business.saveBusy": "Saving and hot-reloading…",
      "business.saved": "Business configuration saved and new configuration hot-reloaded.",
      "business.savedToast": "Business configuration saved and hot-reloaded. Secret inputs cleared; re-fill on the next full replacement.",
      "business.dirty": "The form has unsubmitted content; each submission fully replaces the existing business configuration.",
      "business.unsaved": "No save-success confirmation received; check the service status.",
      "business.submittingToast": "Submitting the full business configuration; the backend builds clients first, then saves and swaps the running instance…",
      "runtime.eyebrow": "Outbound requests",
      "runtime.title": "Timeouts and retries",
      "runtime.persist": "Stored in Redis",
      "runtime.bodyPre": "Runtime parameters are read by ",
      "runtime.bodyMid": " and updated by ",
      "runtime.bodyPost": "; new values apply immediately to subsequent requests after saving.",
      "legend.runtime": "Runtime parameters",
      "runtime.jmapLabel": "JMAP timeout",
      "runtime.jmapDesc": "Upper wait limit for a single JMAP request.",
      "runtime.jmapHint": "100–300,000; default 15,000.",
      "runtime.tgLabel": "Telegram timeout",
      "runtime.tgDesc": "Upper wait limit for a single Telegram request.",
      "runtime.tgHint": "100–300,000; default 10,000.",
      "runtime.llmLabel": "LLM timeout",
      "runtime.llmDesc": "Upper wait limit for a single model request.",
      "runtime.llmHint": "100–300,000; default 30,000.",
      "runtime.retriesLabel": "Max retries",
      "runtime.retriesDesc": "Extra attempts after a failure; 0 means no retry.",
      "runtime.retriesHint": "0–5; default 3.",
      "runtime.saveStateInitial": "Runtime parameters become editable after a successful read.",
      "runtime.previewState": "Default-value preview; save is disabled until a read succeeds.",
      "runtime.loaded": "Runtime parameters read from Redis.",
      "runtime.readFailState": "Read failed; save is disabled.",
      "runtime.dirty": "There are unsaved runtime parameter changes.",
      "runtime.defaultsState": "Defaults restored as preview; they are written to Redis only after saving.",
      "runtime.saveUnconfirmed": "Save state unconfirmed; save is disabled until a successful re-read.",
      "runtime.defaults": "Restore defaults",
      "runtime.save": "Save runtime parameters",
      "runtime.saveBusy": "Saving…",
      "runtime.savingToast": "Saving runtime parameters to Redis…",
      "runtime.readingToast": "Reading runtime parameters from Redis…",
      "runtime.readToast": "Runtime parameters read. Business configuration still needs to be filled in manually.",
      "runtime.savedToast": "Runtime parameters saved to Redis and applied immediately to subsequent requests.",
      "runtime.defaultsToast": "Runtime parameters restored to default-value preview. They are written to Redis only after clicking \"Save runtime parameters\".",
      "privacy.title": "Secrets are never echoed or persisted to the browser.",
      "privacy.body": "Business configuration is submitted only to the same-origin API and encrypted into Redis by the backend; this page never reads mailbox contents and uses no local files or database.",
      "footer.note": "Configuration managed by your server",
      "common.required": "Required",
      "common.optional": "Optional",
      "common.atLeastOne": "At least one",
      "common.ms": "ms",
      "common.times": "times",
      "err.unrecognizedFormat": "The configuration service returned an unrecognized data format.",
      "err.connectFail": "Unable to connect to the configuration service. Confirm the service is available and retry.",
      "err.sessionInvalid": "The admin session is no longer valid. Please re-authorize.",
      "err.runtime400": "Invalid runtime parameter request format (HTTP 400).",
      "err.runtime422": "Runtime parameters out of allowed range (timeouts 100–300,000 ms, retries 0–5).",
      "err.runtime503save": "Redis unavailable or initialization incomplete; save state cannot be confirmed. Runtime parameter saving is disabled; restore the connection and re-read.",
      "err.runtime503load": "Redis unavailable or initialization incomplete. Only the default-value preview is shown and runtime parameter saving is disabled; retry shortly.",
      "err.runtimeFailStatus": "Runtime parameter request failed (HTTP {status}). Retry shortly.",
      "err.runtimeFail": "Runtime parameter request failed; check the service status and retry.",
      "err.unrecognizedRuntime": "The configuration service returned unrecognized runtime parameters.",
      "err.runtimeOutOfRange": "The configuration service returned out-of-range runtime parameters; check the backend configuration.",
      "err.invalidInt": "Please enter an integer.",
      "err.rangeInt": "Please enter an integer between {min} and {max}.",
      "val.i64": "{label} must be a decimal integer.",
      "val.i64Range": "{label} exceeds the signed 64-bit integer range.",
      "val.allowlistEmpty": "Enter at least one chat ID. Whitespace and commas both work as separators.",
      "val.allowlistNotInt": "\"{entry}\" in the allowlist is not an integer chat ID.",
      "val.allowlistRange": "\"{entry}\" in the allowlist exceeds the signed 64-bit integer range.",
      "val.allowlistDup": "Duplicate chat ID in the allowlist: {entry}.",
      "val.httpsUrl": "{label} must be a valid HTTPS URL.",
      "val.https": "{label} must use HTTPS.",
      "val.noCreds": "{label} must not contain a username or password.",
      "val.noQuery": "{label} must not contain query parameters or a #fragment.",
      "val.empty": "{label} cannot be empty.",
      "val.llmUrl": "Base URL is required when LLM is enabled.",
      "val.llmModel": "Model name is required when LLM is enabled.",
      "val.llmKey": "API key is required when LLM is enabled.",
      "berr.401": "The admin session is no longer valid. The session and form contents have been cleared from the page; please re-authorize.",
      "berr.400": "The backend could not parse the submitted JSON (HTTP 400); check the fields and retry.",
      "berr.422": "Business configuration validation failed (HTTP 422). Check required fields, HTTPS URLs, LLM settings and the chat ID allowlist. The server does not return secrets or error details.",
      "berr.503": "Business configuration was not confirmed saved (HTTP 503). Redis may not be ready, or the backend could not build the JMAP/LLM client; the page keeps this input for review and retry.",
      "berr.status": "Business configuration request failed (HTTP {status}). Retry shortly.",
      "berr.fail": "{message} The request result may be unconfirmed; after checking, re-submit the complete configuration.",
      "berr.failDefault": "Business configuration request failed.",
      "login.enterCred": "Enter the Redis ACL management credential. The input is never written to browser storage.",
      "login.creatingToast": "Creating a short-lived admin session…",
      "login.invalidResponse": "The admin endpoint returned an invalid session response.",
      "login.successToast": "Admin session created. Runtime parameters are being read; business configuration must be filled in again in full.",
      "login.401": "The Redis management credential is invalid, or admin sessions are not enabled. Check the Redis URL ACL password.",
      "login.503": "Redis unavailable or initialization incomplete; unable to create an admin session right now. Retry shortly.",
      "login.status": "Unable to create an admin session (HTTP {status}).",
      "login.fail": "Unable to connect to the admin service; check the network and retry.",
    },
    zh: {
      "app.title": "业务配置 · MessageWeave",
      "brand.aria": "MessageWeave 配置首页",
      "brand.tagline": "业务配置",
      "topbar.note": "安全配置通道",
      "intro.eyebrow": "服务管理",
      "intro.title": "管理你的服务连接",
      "intro.copy": "配置 JMAP、Telegram、LLM、访问白名单和运行参数。业务配置保存在外部 Redis，成功保存后会立即热加载。",
      "notice.bootstrap": "使用 Redis 管理凭据创建短期管理会话。",
      "setup.eyebrow": "配置引导",
      "setup.title": "服务启动配置",
      "setup.checking": "正在检查服务启动配置…",
      "setup.checkingBtn": "正在检查…",
      "setup.privacy": "启动所需密钥请在部署环境中配置。密钥不会在页面保存或回显。",
      "setup.retry": "重新检查",
      "setup.modeMissing": "服务处于配置引导模式。请在部署环境补齐以下启动变量：",
      "setup.modeNoMissing": "服务处于配置引导模式，但状态接口未返回缺少的启动变量名称。",
      "setup.unknown": "无法确认服务是否已完成启动配置。管理授权入口保持隐藏，请重新检查服务状态。",
      "setup.fetchError": "暂时无法读取服务启动状态。为保护管理凭据，授权入口保持隐藏；请确认服务可用后重新检查。",
      "auth.eyebrow": "需要授权",
      "auth.title": "创建管理会话",
      "auth.bodyPre": "输入部署时 ",
      "auth.bodyPost": " 中 Redis ACL 用户的密码。服务端会签发有效期 15 分钟的管理会话；密码和会话只在当前页面内存中使用。",
      "auth.label": "Redis 管理凭据",
      "auth.placeholder": "输入 Redis URL 中的 ACL 密码",
      "auth.submit": "创建会话",
      "auth.submitBusy": "正在授权…",
      "auth.hint": "不会写入 localStorage、sessionStorage 或 Cookie。关闭或刷新页面后需要重新授权。",
      "config.aria": "业务配置与运行参数",
      "session.active": "管理会话有效",
      "session.expiryDefault": "会话仅保存在当前页面内存",
      "session.expiry": "会话约 {n} 分钟后过期；只保存在页面内存",
      "session.expired": "管理会话已过期，已清除页面中的会话和表单内容。请重新授权。",
      "session.revoked": "已退出；管理会话和表单中的密钥已从页面内存清除。",
      "session.invalid": "管理会话已失效，页面内容已清除。请重新创建会话。",
      "session.invalidBusiness": "管理会话已失效，已清除页面中的会话与配置。请重新授权。",
      "service.enable": "启用业务处理",
      "service.loading": "读取中…",
      "service.enabled": "业务入口已启用",
      "service.disabled": "业务入口已关闭（fail-closed）",
      "service.enabledToast": "业务处理已启用。",
      "service.disabledToast": "业务处理已关闭，入口将返回未启用状态。",
      "service.saveFail": "无法保存业务开关状态。",
      "reload.button": "重新读取运行参数",
      "reload.busy": "正在读取…",
      "logout.button": "退出并清除会话",
      "business.eyebrow": "完整替换",
      "business.title": "业务连接与鉴权",
      "business.persist": "Redis 加密保存",
      "callout.title": "业务配置不会从服务器读取。",
      "callout.bodyPre": " 为避免密钥回显，API 不提供业务配置 GET。每次提交都必须重新填写完整配置，包括所有必填密钥；可选的 account ID 与 LLM 字段留空时会发送为 ",
      "callout.bodyPost": "。",
      "legend.business": "完整业务配置",
      "jmap.title": "JMAP 邮箱",
      "jmap.desc": "服务端必须使用 HTTPS，URL 中不能包含凭据、查询参数或片段。",
      "jmap.urlHint": "HTTPS 地址，不含用户名、密码、查询参数或 #片段。",
      "jmap.userLabel": "邮箱用户名",
      "jmap.passLabel": "应用专用密码",
      "jmap.accountPh": "留空使用 session 主账户",
      "secret.reenter": "每次完整保存时重新输入",
      "telegram.title": "Telegram 与访问鉴权",
      "telegram.desc": "仅允许白名单中的 chat ID 触发机器人操作。",
      "telegram.chatLabel": "目标 Chat ID",
      "telegram.chatPh": "例如 -1001234567890",
      "telegram.chatHint": "接受完整有符号 64 位整数。",
      "telegram.allowPh": "每行或逗号分隔一个 chat ID",
      "telegram.allowHint": "整数列表；保存前会检查格式与重复项。",
      "telegram.workerPh": "/worker 与兼容管理 API 使用；每次完整保存时重新输入",
      "telegram.workerHint": "此值与当前管理会话不同。页面不会读取或回显已有 token。",
      "llm.title": "LLM 服务",
      "llm.desc": "启用后要求 HTTPS Base URL、模型名和 API key。",
      "llm.enableLabel": "启用 LLM",
      "llm.enableHint": "允许机器人调用兼容 OpenAI 的模型服务。",
      "llm.netLabel": "允许网络调用",
      "llm.netHint": "关闭时不会创建 LLM 客户端。",
      "llm.urlHint": "启用 LLM 时必须使用 HTTPS。",
      "llm.modelLabel": "模型名称",
      "llm.modelPh": "留空表示 null",
      "llm.keyPh": "启用 LLM 时必填；不会回显",
      "business.saveStateInitial": "完成管理会话验证后可填写完整配置。",
      "business.saveStateReady": "完整填写 BusinessConfigWire 后可提交。密钥不会回显。",
      "business.save": "保存业务配置并热加载",
      "business.saveBusy": "正在保存并热加载…",
      "business.saved": "业务配置已保存，新配置已热加载。",
      "business.savedToast": "业务配置已保存并热加载。密钥输入已清空；下次完整替换时需要重新填写。",
      "business.dirty": "表单包含未提交内容；每次提交会完整替换现有业务配置。",
      "business.unsaved": "未收到保存成功确认；请检查服务状态。",
      "business.submittingToast": "正在提交完整业务配置；后端会先构建客户端，再保存并切换运行实例…",
      "runtime.eyebrow": "出站请求",
      "runtime.title": "超时与重试",
      "runtime.persist": "Redis 保存",
      "runtime.bodyPre": "运行参数由 ",
      "runtime.bodyMid": " 读取、由 ",
      "runtime.bodyPost": " 更新；新值保存后立即用于后续请求。",
      "legend.runtime": "运行参数",
      "runtime.jmapLabel": "JMAP 超时",
      "runtime.jmapDesc": "单次 JMAP 请求的等待上限。",
      "runtime.jmapHint": "100–300,000；默认 15,000。",
      "runtime.tgLabel": "Telegram 超时",
      "runtime.tgDesc": "单次 Telegram 请求的等待上限。",
      "runtime.tgHint": "100–300,000；默认 10,000。",
      "runtime.llmLabel": "LLM 超时",
      "runtime.llmDesc": "单次模型请求的等待上限。",
      "runtime.llmHint": "100–300,000；默认 30,000。",
      "runtime.retriesLabel": "最大重试次数",
      "runtime.retriesDesc": "失败后额外尝试的次数，0 表示不重试。",
      "runtime.retriesHint": "0–5；默认 3。",
      "runtime.saveStateInitial": "读取成功后可编辑运行参数。",
      "runtime.previewState": "默认值预览；读取后才能保存。",
      "runtime.loaded": "已读取 Redis 中的运行参数。",
      "runtime.readFailState": "读取未成功，保存已禁用。",
      "runtime.dirty": "有尚未保存的运行参数修改。",
      "runtime.defaultsState": "已恢复默认预览值；保存后才会写入 Redis。",
      "runtime.saveUnconfirmed": "保存状态无法确认；重新读取成功前已禁用保存。",
      "runtime.defaults": "恢复默认值",
      "runtime.save": "保存运行参数",
      "runtime.saveBusy": "正在保存…",
      "runtime.savingToast": "正在保存运行参数到 Redis…",
      "runtime.readingToast": "正在从 Redis 读取运行参数…",
      "runtime.readToast": "运行参数已读取。业务配置仍需手动完整填写。",
      "runtime.savedToast": "运行参数已保存到 Redis，并立即应用于后续请求。",
      "runtime.defaultsToast": "运行参数已恢复为默认值预览。点击“保存运行参数”后才会更新 Redis。",
      "privacy.title": "密钥不会回显或持久化到浏览器。",
      "privacy.body": "业务配置只提交至同源 API，并由后端加密写入 Redis；本页面不读取邮箱内容、不使用本地文件或数据库。",
      "footer.note": "配置由你的服务端管理",
      "common.required": "必填",
      "common.optional": "可选",
      "common.atLeastOne": "至少一项",
      "common.ms": "毫秒",
      "common.times": "次",
      "err.unrecognizedFormat": "配置服务返回了无法识别的数据格式。",
      "err.connectFail": "无法连接配置服务。请确认服务可用后重试。",
      "err.sessionInvalid": "管理会话已失效，请重新授权。",
      "err.runtime400": "运行参数请求格式无效（HTTP 400）。",
      "err.runtime422": "运行参数超出允许范围（超时 100–300,000 毫秒，重试 0–5 次）。",
      "err.runtime503save": "Redis 不可用或初始化尚未完成；保存状态无法确认。已禁用运行参数保存，请恢复连接后重新读取。",
      "err.runtime503load": "Redis 不可用或初始化尚未完成。当前只显示默认值预览，运行参数保存已禁用；请稍后重试。",
      "err.runtimeFailStatus": "运行参数请求失败（HTTP {status}）。请稍后重试。",
      "err.runtimeFail": "运行参数请求失败，请检查服务状态后重试。",
      "err.unrecognizedRuntime": "配置服务返回了无法识别的运行参数。",
      "err.runtimeOutOfRange": "配置服务返回了超出范围的运行参数，请检查后端配置。",
      "err.invalidInt": "请输入整数。",
      "err.rangeInt": "请输入 {min} 到 {max} 之间的整数。",
      "val.i64": "{label} 必须是十进制整数。",
      "val.i64Range": "{label} 超出有符号 64 位整数范围。",
      "val.allowlistEmpty": "至少填写一个 chat ID。空白和逗号均可用作分隔符。",
      "val.allowlistNotInt": "白名单中的“{entry}”不是整数 chat ID。",
      "val.allowlistRange": "白名单中的“{entry}”超出有符号 64 位整数范围。",
      "val.allowlistDup": "白名单中有重复的 chat ID：{entry}。",
      "val.httpsUrl": "{label} 必须是有效的 HTTPS URL。",
      "val.https": "{label} 必须使用 HTTPS。",
      "val.noCreds": "{label} 不能包含用户名或密码。",
      "val.noQuery": "{label} 不能包含查询参数或 #片段。",
      "val.empty": "{label} 不能为空。",
      "val.llmUrl": "启用 LLM 时必须填写 Base URL。",
      "val.llmModel": "启用 LLM 时必须填写模型名称。",
      "val.llmKey": "启用 LLM 时必须填写 API key。",
      "berr.401": "管理会话已失效。页面已清除会话和表单内容，请重新授权。",
      "berr.400": "后端无法解析提交的 JSON（HTTP 400）；请检查字段后重试。",
      "berr.422": "业务配置校验失败（HTTP 422）。请检查必填项、HTTPS URL、LLM 设置和 chat ID 白名单。服务端没有返回密钥或错误详情。",
      "berr.503": "业务配置未获成功确认（HTTP 503）。Redis 可能尚未就绪，或后端无法建立 JMAP/LLM 客户端；页面保留本次输入供检查和重试。",
      "berr.status": "业务配置请求失败（HTTP {status}）。请稍后重试。",
      "berr.fail": "{message} 请求结果可能无法确认；检查后可重新提交完整配置。",
      "berr.failDefault": "业务配置请求失败。",
      "login.enterCred": "请输入 Redis ACL 管理凭据。输入不会写入浏览器存储。",
      "login.creatingToast": "正在创建短期管理会话…",
      "login.invalidResponse": "管理接口返回了无效的会话响应。",
      "login.successToast": "管理会话已创建。运行参数正在读取；业务配置需要完整重新填写。",
      "login.401": "Redis 管理凭据无效，或服务尚未启用管理会话。请检查 Redis URL ACL 密码。",
      "login.503": "Redis 不可用或初始化尚未完成，暂时无法创建管理会话。请稍后重试。",
      "login.status": "无法创建管理会话（HTTP {status}）。",
      "login.fail": "无法连接管理服务，请检查网络后重试。",
    },
  });

  function detectLocale() {
    let pref = "";
    try {
      if (typeof navigator !== "undefined" && navigator) {
        pref = (Array.isArray(navigator.languages) && navigator.languages[0]) || navigator.language || "";
      }
    } catch {
      pref = "";
    }
    if (typeof pref === "string" && pref.toLowerCase().startsWith("zh")) return "zh";
    return "en";
  }

  let locale = detectLocale();

  function t(key, vars) {
    const dict = MESSAGES[locale] || MESSAGES.en;
    let str = dict[key];
    if (str === undefined) str = MESSAGES.en[key];
    if (str === undefined) return key;
    if (vars) {
      for (const [name, value] of Object.entries(vars)) {
        str = str.split(`{${name}}`).join(String(value));
      }
    }
    return str;
  }

  function joinList(items) {
    const sep = locale === "zh" ? "、" : ", ";
    return items.join(sep);
  }

  function applyI18n() {
    const doc = (typeof document !== "undefined") ? document : null;
    if (!doc) return;
    const root = doc.documentElement;
    if (root && root.setAttribute) root.setAttribute("lang", locale === "zh" ? "zh-CN" : "en");
    if (typeof doc.querySelectorAll !== "function") return;
    for (const el of doc.querySelectorAll("[data-i18n]")) {
      const key = el.getAttribute("data-i18n");
      if (key) el.textContent = t(key);
    }
    for (const el of doc.querySelectorAll("[data-i18n-placeholder]")) {
      const key = el.getAttribute("data-i18n-placeholder");
      if (key) el.setAttribute("placeholder", t(key));
    }
    for (const el of doc.querySelectorAll("[data-i18n-aria]")) {
      const key = el.getAttribute("data-i18n-aria");
      if (key) el.setAttribute("aria-label", t(key));
    }
  }

  // Expose a tiny debug surface for runtime inspection and tests.
  if (typeof window !== "undefined") {
    window.__mw = { t, detectLocale, getLocale: () => locale, messages: MESSAGES, joinList };
  }

  applyI18n();
  // ─────────────────────────────────────────────────────────────────────────

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
    connectButton.textContent = busy && busyAction === "login" ? t("auth.submitBusy") : t("auth.submit");
    reloadButton.textContent = busy && busyAction === "runtime-load" ? t("reload.busy") : t("reload.button");
    businessSaveButton.querySelector(".button-label").textContent = busy && busyAction === "business-save" ? t("business.saveBusy") : t("business.save");
    runtimeSaveButton.querySelector(".button-label").textContent = busy && busyAction === "runtime-save" ? t("runtime.saveBusy") : t("runtime.save");
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
      throw new Error(t("err.unrecognizedFormat"));
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
      throw new Error(t("err.connectFail"));
    }
    const responseText = await response.text().catch(() => "");
    if (!response.ok) throw new ApiError(response.status, responseText);
    return parseResponseText(responseText, response.status);
  }

  async function showSetupStatus() {
    showSetupView(t("setup.checking"));
    statusRetryButton.disabled = true;
    statusRetryButton.textContent = t("setup.checkingBtn");
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
        showNotice("info", t("notice.bootstrap"));
        return;
      }
      const isSetup = status && status.ready === false && status.mode === "configuration-setup";
      if (isSetup) {
        const knownMissing = Array.isArray(status.missing)
          ? status.missing.filter((name) => name === "REDIS_URL" || name === "CONFIG_ENCRYPTION_KEY")
          : [];
        const message = knownMissing.length ? t("setup.modeMissing") : t("setup.modeNoMissing");
        showSetupView(message, knownMissing);
        return;
      }
      showSetupView(t("setup.unknown"));
    } catch {
      showSetupView(t("setup.fetchError"));
    } finally {
      statusRetryButton.disabled = false;
      statusRetryButton.textContent = t("setup.retry");
    }
  }

  async function loadEnabled() {
    const state = await request("/api/enabled");
    serviceEnabled.checked = state.enabled === true;
    serviceEnabledState.textContent = serviceEnabled.checked ? t("service.enabled") : t("service.disabled");
  }

  function showSetupView(message, missing = []) {
    setupMode = true;
    setupMessage.textContent = message;
    setupMissing.textContent = joinList(missing);
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
    runtimeSaveState.textContent = t("runtime.saveStateInitial");
    businessSaveState.textContent = t("business.saveStateInitial");
    updateLLMRequirements();
  }

  function clearSession() {
    adminSession = "";
    if (expiryTimer) window.clearTimeout(expiryTimer);
    expiryTimer = 0;
    sessionExpiry.textContent = t("session.expiryDefault");
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
    sessionExpiry.textContent = t("session.expiry", { n: Math.ceil(duration / 60) });
    expiryTimer = window.setTimeout(() => {
      expireSession(t("session.expired"));
    }, duration * 1000);
  }

  function normalizeRuntimeConfig(value) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      throw new Error(t("err.unrecognizedRuntime"));
    }
    const config = {};
    for (const [key] of RUNTIME_FIELDS) {
      const item = value[key];
      const limit = RUNTIME_LIMITS[key];
      if (!Number.isInteger(item) || item < limit.min || item > limit.max) {
        throw new Error(t("err.runtimeOutOfRange"));
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
    runtimeSaveState.textContent = t("runtime.previewState");
    updateButtons();
  }

  function activateRuntimeConfig(config) {
    fillRuntimeFields(config);
    runtimeLoaded = true;
    runtimeFields.disabled = false;
    runtimeSaveState.textContent = t("runtime.loaded");
    runtimeSaveState.classList.remove("is-dirty");
    updateButtons();
  }

  function runtimeErrorMessage(error, phase) {
    if (error.status === 401) return t("err.sessionInvalid");
    if (error.status === 400) return t("err.runtime400");
    if (error.status === 422) return t("err.runtime422");
    if (error.status === 503) return phase === "save" ? t("err.runtime503save") : t("err.runtime503load");
    if (error.status !== undefined) return t("err.runtimeFailStatus", { status: error.status });
    return error.message || t("err.runtimeFail");
  }

  async function loadRuntimeConfig() {
    if (!adminSession || busyAction) return;
    showRuntimePreview();
    setBusy("runtime-load");
    showNotice("info", t("runtime.readingToast"));
    try {
      const config = normalizeRuntimeConfig(await request("/api/config"));
      activateRuntimeConfig(config);
      showNotice("success", t("runtime.readToast"));
    } catch (error) {
      if (error.status === 401) {
        expireSession(t("session.invalid"));
        return;
      }
      runtimeLoaded = false;
      runtimeFields.disabled = true;
      runtimeSaveState.textContent = t("runtime.readFailState");
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
      setFieldError(input, t("val.i64", { label }));
      throw new Error("invalid-field");
    }
    const parsed = BigInt(value);
    if (parsed < I64_MIN || parsed > I64_MAX) {
      setFieldError(input, t("val.i64Range", { label }));
      throw new Error("invalid-field");
    }
    return parsed;
  }

  function parseAllowlist(input) {
    const entries = input.value.split(/[\s,]+/).filter(Boolean);
    if (entries.length === 0) {
      setFieldError(input, t("val.allowlistEmpty"));
      throw new Error("invalid-field");
    }
    const ids = [];
    const seen = new Set();
    for (const entry of entries) {
      if (!/^-?\d+$/.test(entry)) {
        setFieldError(input, t("val.allowlistNotInt", { entry }));
        throw new Error("invalid-field");
      }
      const id = BigInt(entry);
      if (id < I64_MIN || id > I64_MAX) {
        setFieldError(input, t("val.allowlistRange", { entry }));
        throw new Error("invalid-field");
      }
      if (seen.has(id.toString())) {
        setFieldError(input, t("val.allowlistDup", { entry }));
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
      setFieldError(input, t("val.httpsUrl", { label }));
      throw new Error("invalid-field");
    }
    if (parsed.protocol !== "https:" || !parsed.hostname) {
      setFieldError(input, t("val.https", { label }));
      throw new Error("invalid-field");
    }
    if (noCredentials && (parsed.username || parsed.password)) {
      setFieldError(input, t("val.noCreds", { label }));
      throw new Error("invalid-field");
    }
    const rawValue = input.value.trim();
    if (noQuery && (rawValue.includes("?") || rawValue.includes("#"))) {
      setFieldError(input, t("val.noQuery", { label }));
      throw new Error("invalid-field");
    }
    return input.value.trim();
  }

  function requireText(id, label, secret = false) {
    const input = document.getElementById(id);
    const value = secret ? input.value : input.value.trim();
    if (isBlank(value)) {
      setFieldError(input, t("val.empty", { label }));
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
      if (enabled && !llmBaseValue) return setFieldError(llmBaseUrl, t("val.llmUrl"));
      if (enabled && !llmModelValue) return setFieldError(llmModel, t("val.llmModel"));
      if (enabled && isBlank(llmKeyValue)) return setFieldError(llmApiKey, t("val.llmKey"));
      const llmBase = llmBaseValue
        ? (enabled ? validateHttpsUrl(llmBaseUrl, "LLM Base URL") : llmBaseValue)
        : null;

      const jmapUrlInput = document.getElementById("jmap-session-url");
      const business = {
        bot_token: requireText("telegram-bot-token", "Telegram Bot Token", true),
        telegram_chat_id: parseI64(document.getElementById("telegram-chat-id"), t("telegram.chatLabel")),
        chat_allowlist: parseAllowlist(document.getElementById("chat-allowlist")),
        telegram_webhook_secret: requireText("telegram-webhook-secret", "Telegram Webhook Secret", true),
        jmap_session_url: validateHttpsUrl(jmapUrlInput, "JMAP Session URL", { noCredentials: true, noQuery: true }),
        jmap_username: requireText("jmap-username", t("jmap.userLabel")),
        jmap_password: requireText("jmap-password", t("jmap.passLabel"), true),
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
    if (error.status === 401) return t("berr.401");
    if (error.status === 400) return t("berr.400");
    if (error.status === 422) return t("berr.422");
    if (error.status === 503) return t("berr.503");
    if (error.status !== undefined) return t("berr.status", { status: error.status });
    return t("berr.fail", { message: error.message || t("berr.failDefault") });
  }

  function runtimePayloadFromForm() {
    clearCustomValidity(runtimeForm);
    const config = {};
    for (const [key, id] of RUNTIME_FIELDS) {
      const input = document.getElementById(id);
      const value = input.value.trim() === "" ? Number.NaN : Number(input.value);
      const limit = RUNTIME_LIMITS[key];
      if (!Number.isInteger(value)) return setFieldError(input, t("err.invalidInt")), null;
      if (value < limit.min || value > limit.max) {
        return setFieldError(input, t("err.rangeInt", { min: limit.min, max: limit.max })), null;
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
    const mark = enabled ? t("common.required") : t("common.optional");
    document.querySelector("#llm-key-required").textContent = mark;
    document.querySelector("#llm-url-required").textContent = mark;
    document.querySelector("#llm-model-required").textContent = mark;
  }

  authForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (busyAction) return;
    let bootstrapCredential = bootstrapInput.value;
    bootstrapInput.value = "";
    if (isBlank(bootstrapCredential)) {
      showNotice("error", t("login.enterCred"));
      bootstrapInput.focus();
      return;
    }

    setBusy("login");
    showNotice("info", t("login.creatingToast"));
    try {
      const sessionInfo = await request("/api/admin/session", "POST", undefined, bootstrapCredential);
      bootstrapCredential = "";
      if (!sessionInfo || typeof sessionInfo.session !== "string" || !sessionInfo.session) {
        throw new Error(t("login.invalidResponse"));
      }
      adminSession = sessionInfo.session;
      scheduleSessionExpiry(sessionInfo.expires_in);
      showConfigView();
      businessFields.disabled = false;
      businessSaveState.textContent = t("business.saveStateReady");
      showRuntimePreview();
      setBusy("");
      showNotice("success", t("login.successToast"));
      await loadRuntimeConfig();
      await loadEnabled();
    } catch (error) {
      bootstrapCredential = "";
      setBusy("");
      if (error.status === 401) showNotice("error", t("login.401"));
      else if (error.status === 503) showNotice("error", t("login.503"));
      else if (error.status !== undefined) showNotice("error", t("login.status", { status: error.status }));
      else showNotice("error", error.message || t("login.fail"));
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
      serviceEnabledState.textContent = desired ? t("service.enabled") : t("service.disabled");
      showNotice("success", desired ? t("service.enabledToast") : t("service.disabledToast"));
    } catch (error) {
      serviceEnabled.checked = !desired;
      serviceEnabledState.textContent = serviceEnabled.checked ? t("service.enabled") : t("service.disabled");
      showNotice("error", error.status === 401 ? t("err.sessionInvalid") : t("service.saveFail"));
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
    showNotice("info", t("session.revoked"));
  });

  statusRetryButton.addEventListener("click", showSetupStatus);

  defaultsButton.addEventListener("click", () => {
    if (!runtimeLoaded || busyAction) return;
    fillRuntimeFields(RUNTIME_DEFAULTS);
    runtimeSaveState.textContent = t("runtime.defaultsState");
    runtimeSaveState.classList.add("is-dirty");
    showNotice("info", t("runtime.defaultsToast"));
  });

  businessForm.addEventListener("input", (event) => {
    event.target.setCustomValidity("");
    if (!adminSession) return;
    businessSaveState.textContent = t("business.dirty");
    businessSaveState.classList.add("is-dirty");
  });

  runtimeForm.addEventListener("input", (event) => {
    event.target.setCustomValidity("");
    if (!runtimeLoaded) return;
    runtimeSaveState.textContent = t("runtime.dirty");
    runtimeSaveState.classList.add("is-dirty");
  });

  llmEnabled.addEventListener("change", updateLLMRequirements);

  businessForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!adminSession || busyAction) return;
    const config = readBusinessConfig();
    if (!config) return;
    setBusy("business-save");
    showNotice("info", t("business.submittingToast"));
    try {
      await request("/api/business-config", "PUT", stringifyWithBigInt(config));
      clearSecretFields();
      businessSaveState.textContent = t("business.saved");
      businessSaveState.classList.remove("is-dirty");
      showNotice("success", t("business.savedToast"));
      updateButtons();
    } catch (error) {
      if (error.status === 401) {
        expireSession(t("session.invalidBusiness"));
        return;
      }
      businessSaveState.textContent = t("business.unsaved");
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
    showNotice("info", t("runtime.savingToast"));
    try {
      const saved = normalizeRuntimeConfig(await request("/api/config", "PUT", config));
      activateRuntimeConfig(saved);
      showNotice("success", t("runtime.savedToast"));
    } catch (error) {
      if (error.status === 401) {
        expireSession(t("session.invalidBusiness"));
        return;
      }
      if (error.status >= 500 || error.status === undefined) {
        runtimeLoaded = false;
        runtimeFields.disabled = true;
        runtimeSaveState.textContent = t("runtime.saveUnconfirmed");
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
