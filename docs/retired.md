# Abandoned and unused routes (Retired Routes)

> [中文版本 / Chinese version → retired.zh-CN.md](retired.zh-CN.md)

> This article only records **three categories** of things: ① routes that were evaluated but not adopted; ② documents that actually existed but were later deleted; ③ names that were once written into documents but never actually existed or were never implemented.
> The purpose is to retain the cause and prevent recurrence** - when you see these names in the future, you can immediately know what to use.
> See `docs/reference.md` for the current real implementation and verifiable facts; `docs/opengaps.md` for gaps and blocking; see `docs/design.md` for the current design.
> This article has **no action items**. If something becomes something to do, go to `docs/opengaps.md` to register, and the entries in this article will be retained as a basis for decision-making.
> This article **does not cite any non-existent paths**: all paths point to files that actually exist in the warehouse; paths that have been written into documents but have never been implemented will only describe their form in text and will not be repeated verbatim to avoid being copied back into the text or viewed as real files.
> Verify baseline: `b2dbe7c`.

---

## 1. Route not taken

| Entry | Type | Reason | Alternative or Current Status |
|---|---|---|---|
| `teloxide` (Telegram Bot framework) | Not adopted | Telegram requires only a few API calls, does not introduce heavy frameworks (dptree/session middleware), and reduces dependencies and abstraction layers; `reqwest` has been introduced according to `ARCH-DEPS-STAGE4` | `src/channel.rs` is self-developed and implemented with `reqwest` |
| teloxide planned feature set (`macros` / `rustls` / `redis-session` / `throttle` / `webhooks-axum`) | Not implemented | Dead when teloxide is not adopted | — |
| `teloxide-core` downgrade plan | Not adopted | The premise for downgrading is "the use of teloxide is too heavy"; since the whole is not introduced, the premise for downgrading is not valid | — |
| `grammers` / old `telegram-bot` crate | Not adopted | Directly seal the Bot API + self-research to meet the needs | `src/channel.rs` |
| `BotError::Telegram(#[from] teloxide::errors::RequestError)` | Target form variant, not landed | Attached to teloxide, discarded with it | None |
| `BotError::Jmap` / `Storage` / `RateLimited` / `Unauthorized` / `Llm` | Target form variant, not yet implemented | It was designed to uniformly handle the five types of errors of JMAP/Redis/current limiting/authorization/LLM; now each module handles it by itself | Currently `BotError` only has `Config` / `Io` / `Json` / `State` Four variants (full text of `src/error.rs`) |
| Telegram server 30 msg/s current limit bucket | Target design, not implemented | No real Bot stress test data, no preset implementation; server bucketing is undertaken by TG API itself, and the bot side only performs backoff | 429 dedicated branch **implemented** (`src/channel.rs`: parse the response body `parameters.retry_after` and clamp ≤60s, the server-specified value will be used first, otherwise exponential backoff), so this line does not constitute a gap; the current processing of the server-side current limit bucket is still `max_retries` (default 3, hard upper limit 5) Universal retry |
| Multi-step dialogue FSM (`Idle` / `AwaitClarify` / `AwaitConfirm` / `Analyzing` / `AwaitFallback`) | Target design, not implemented | Current AI authorization only needs a Boolean + expiration time, multi-step is over-design | Redis TTL authorization state, key and TTL, see the AI authorization state section of `docs/reference.md` |
| Split each layer of domain / channel / notification / util into independent directories (sub-module split plan) | Target form directory, not implemented | The amount of code has not reached the scale that needs to be split | For the actual structure, see the project structure section of `docs/design.md`: the domain layer is `src/domain.rs` + `src/domain/jmap/` (only `client.rs`); LLM is in `src/ai.rs`; notifications are in `src/notify.rs`; encryption and tool logic are inline in `src/state.rs` / `src/config.rs`, there is no independent util layer and no integration test directory |
| Teloxide style outbound message delivery integration test | Target form test, not yet implemented | Dependent on teloxide test mode | Unit test of `src/channel.rs` (`#[test]`) |
| docker-compose `healthcheck` example (`message-weave health --addr ...`) | Deleted example | `src/main.rs` has no CLI subcommand resolution, the subcommand does not exist, and copying will fail (in the early days, there was also a problem of not having `curl` in the image, and now `curl` is built in) | `/ready` is detected by the platform ingress; for instructions, see `docs/deployment.md` Readiness Detection section |

| Redis ACL password for SPA admin credentials = `REDIS_URL` | Route not taken | Obfuscating infrastructure credentials with UI admin password; `.is_empty()` guard for `bootstrap_token` when Redis has no ACL password (TLS-only managed Redis) makes SPA permanent 401 | Use instead `CONFIG_ENCRYPTION_KEY` (32-byte high-entropy hex required for startup, constant time comparison) |

### 1.1 Teloxide candidate comparison (evaluation record)

The following table and the following reasons are **original evaluation conclusions, and retaining them is the basis for decision-making, not the current technical facts** - "Active, latest 0.17, large download volume" and other framework attributes have not been reviewed in this round and do not represent the current version status of these crates.

| Framework | crate | maintenance status | features | fitness | conclusion |
|---|---|---|---|---|---|
| **teloxide** | `teloxide` | Active, latest 0.17, large downloads | dptree distribution, conversational FSM, webhooks+webhooks-axum, Redis session storage, throttle, macros, tracing, rustls | ★★★★★ | **Preferred** |
| grammers | `grammers` / `grammerslib` | General maintenance | MTProto (not Bot API), no Telegram Bot Token required | ★★ | Only if Bot API is not available |
| telegram-bot (old) | `telegram-bot` | Basically discontinued | reqwest + futures | ★ | Not recommended |

**6 reasons why I originally preferred teloxide**:
1. It is the same tokio + reqwest ecosystem as `jmap-client`, and the runtime and TLS stack (rustls) can be reused.
2. Built-in Dispatcher + `UpdateKind` enumeration matching commands, which is naturally compatible with command routing.
3. Support `webhooks-axum` (production Webhook form; do not use long polling `NG-LONG-POLLING`) and Redis session storage (remember the current folder/pagination cursor, without SQLite).
4. The `throttle` feature naturally fits Telegram’s 30 msg/s rate limit.
5. The `tracing` feature is consistent with the observation of this project.
6. The `macros` feature can use `#[teloxide::command]` to automatically parse command parameters and reduce boilerplate.

**Feature set plan at that time** (when introduced in Phase 2): `macros`, `redis-session`, `throttle`, `tracing`, enable `webhooks-axum` on demand; `rustls` and `rustls-native-roots` on the TLS side **Choose one of the two on demand** (the latter supplements the OS root certificate for the former, which is convenient for local testing but unnecessary for production).

**Replacement/Downgrade**: If teloxide is upgraded or destructively changed, you can retreat to the thinner `teloxide-core` (retaining the core and types, removing the dispatcher abstraction); if you need multi-account high throughput, use `webhooks-axum` + shared `axum::Router`. Both of the above are no longer valid when teloxide is not adopted.

### 1.2 Dialogue FSM state transition table (target design, not yet implemented)

There is currently no FSM: `parse_intent` of `src/worker.rs` is directly parsed into `Intent`, and AI authorization is a Boolean expiration time in Redis. The following 5 states have been designed:

| Status | Meaning | Entering | Leaving |
|---|---|---|---|
| `Idle` | idle | any completion state | message received |
| `AwaitClarify` | The goal/intent is unknown, waiting for the user to choose | The intent or email target is not unique | The user gives a clear choice |
| `AwaitConfirm` | Wait for confirmation (AI analysis/attachment download) | User initiated analysis/download but not confirmed | Confirm/Cancel |
| `Analyzing` | AI request in-flight | User confirmation analysis | Success/Failure |
| `AwaitFallback` | AI failed, waiting for confirmation to fallback | 3 consecutive failed circuit breakers | User confirmation/cancel |

The invariants set at that time: `AwaitConfirm` / `Analyzing` / `AwaitFallback` involve AI or attachment downloading, ** LLM is not allowed to be called or attachments are not pulled until the confirmation state is reached **; the session state is short-term, and the external Redis short-term TTL is unified (SQLite is not used, and loss is acceptable). Channel neutrality requires that FSM status and events use domain types and do not rely on any channel SDK.

The invariant "AI analysis must first be explicitly authorized by the user" is still valid and has been retained in the session state machine section of `docs/design.md`; the state machine itself is not implemented.

---

## 2. Fictional entries (documentation has been written, code has never existed)

These names never exist in the code. Some of them appear in the directory tree of the target form, and some appear in unsubmitted drafts and discussion records. The common risk is that people mistakenly think that "this function has been implemented", so register them here to prevent their recurrence.

| Entry | Type | Reason | Alternative or Current Status |
|---|---|---|---|
| Directory scheme that splits Telegram access into four files "module entry/command parsing/session state/reply rendering" | Fictitious directory scheme | Drawn in the target directory tree, never created | Command parsing is in `parse_intent` (`src/worker.rs`); the session/authorization state is Redis TTL (the AI authorization state section of `docs/reference.md`); the rendering function is in `src/channel.rs` |
| `delivery:pending:{stream}` | Fictional key | Confused with Redis Streams' pending-entries list (PEL) - PEL is maintained internally by Redis and is not a writable key | Real key: `delivery:inflight:{stream}:{id}` (EX 60) and `delivery:committed:{stream}:{id}` (EX 604_800), see Delivery pipeline keys section of `docs/reference.md` |
| `check_config_reload` | Fictional function | Uncommitted name in draft, never made it into code | Hot updated to `refresh_business_config` (`src/notify.rs:1130`) |
| `push:registration:{sha256(callback_url)}` was once written as "360s registration solo lock" | Fact mislabeling | The key is 7d callback → subscription ID mapping | The real 360s solo lock is `lock:push-register:{sha256(callback_url)}` (`src/notify.rs:1328-1330`) |
| `MESSAGWEAVE_DOMAIN` was once written as "Worker whitelist is invalid when production is not configured" | Fictional assertion (unsubmitted draft/appeared in discussion, not documented) | This environment variable has zero hits in all positions; Worker whitelist is **unconditional fail-closed**: unknown path 404, method does not match 405, backend is missing or parsing failure 503 (`cloudflare-worker/src/index.js:77-92`) | No need to configure switches, the whitelist will always take effect |
| `read_batch` / `retry_or_dlq` "Redis may still return `Ok(())` when an error occurs, so the consumption loop will not exit due to a single failure" | Fictional assertion | The consumption entry is HTTP handler `worker` (`notify.rs:390`), **not a background loop**: full position `src/` zero hit `select!`, no signal processing, no resident worker process. `read_batch` (`state.rs:491`) throws the Redis error as it is (`state.rs:531`) via `?`, and the only discarded result is the `BUSYGROUP` idempotent protection of XGROUP `CREATE` (`state.rs:499-507`); `retry_or_dlq` is also thrown up via `?`. The caller returns `503 service_unavailable` + `retryable=true` for every `Err`, so the semantic premise of "the loop does not exit due to a single failure" itself is not true | None - the gap does not exist; `docs/opengaps.md` (original name `docs/roadmap.md`) "Code Gap" 4 → 3, `docs/design.md` "Known Boundary" same sentence has been deleted |

---

| Build LLM capabilities into domain sub-module families (configuration/fallback/policy/audit layering) | Fictitious module tree | There are only `jmap.rs` and `jmap/client.rs` under `domain/`; LLM only has one file `src/ai.rs` (`LlmClient`) | None |
| `ai::config` / `ai::fallback` / `ai::policy` / `ai::audit` | Fictional module | LLM is configured in `config.rs::LlmConfig`, runtime parameters are in `state.rs::OutboundConfig` (delivered by `RuntimeConfigProvider`), no policy/audit concept | None |
| Build the channel layer into a directory module (including module entry file) | Fictitious directory | The channel layer is a file called `src/channel.rs` | None |
| `notify::push_handler` / `notify::worker` / `notify::reconcile` as module paths | Fictitious module paths | These are free functions within `src/notify.rs` (`jmap_push` / `worker` / `reconcile`), not module paths | None |
| `mod_dedup` / `mod_streams` / `mod_sincestate` | Fictional module name | `state.rs` / `notify.rs` are tile files, no submodules; deduplication and Streams logic exist as free functions | None |
| `PushVerification` type | Fictitious type | Undefined; `register_push` (`notify.rs:1315`) handles callback URL and verification code writeback inline | None |
| `CancellationToken` | fictitious type | unused; no graceful shutdown, no signal handling (`src/` zero hits `tokio::signal` / `ctrl_c`) | None |
| `Preview` type / 4000 character long email protection / `[Continue to view original text]` button / `/llm-fallback` button | Fictional type and UI | All not implemented; after authorization, the full text will be handed over to LLM, and if it fails, the first 300 characters will be returned, without any truncation mark or button | None |
| `LlmErr` (5 variants) | Fictional enum | Real is `AiError`, only 3 variants (`InvalidEndpoint` / `Request` / `Response`) | None |
| `LLM_TEMPERATURE` / `LLM_MAX_TOKENS` / `LLM_TIMEOUT_SECS` / `LLM_MAX_RETRIES` environment variables | fictitious environment variables | `src/` zero hits (`LLM_MAX_RETRIES` is the Redis running parameter `max_retries`, non-environment variable); there are 6 real business configuration fields: `LLM_ENABLED` / `LLM_ALLOW_NET` / `LLM_API_KEY` / `LLM_BASE_URL` / `LLM_MODEL` / `LLM_SUMMARY_TARGET_CHARS` (the latter value is hard-coded in the constructor and not entered into the wire), and there are two runtime parameters `llm_timeout_ms` / `max_retries` | See `docs/reference.md` AI Authorization section |
| `llm.call` tracing span | Fictitious observation point | **Zero tracing events, zero span** in `src/` except `main.rs` (5 events); no logs for LLM calls | None |
| Circuit breaker / half-open state / 60s cooling after blowing / rule clearance (Redis share count) | No design implemented | Only timeout + retry; LLM failure will be silently downgraded to the first 300 words of fallback, no user-side prompts, no status records | None |
| Scheduled summary/daily email summary push | Not implemented | Not implemented | None |
| Attachment download (`send_document` / `Blob/get` / download button) | Not implemented | JMAP side read-only; email attachments only appear as `has_attachment: bool` | None |
| `/flag` / `/unseen` / Send command | Not implemented | Currently 6 intents recognized (Help/Consent/Summary/Search/General Messages/Unrecognized); of which `/search` is implemented (`worker.rs:680`) | None |
| `Identity` concept | Not implemented | Account identification only relies on `ACCOUNT_ID`, no identity layer abstraction | None |
| Extract `RUN_MODE` into a separate configuration file | Fictitious splitting scheme | `RUN_MODE` was once read by `config.rs` and verified in `src/main.rs`. **This variable and the verification block have been deleted together** (see §4); there has never been such a function as `validate_env_or_exit` | None |
| docker-compose `message-weave health --addr` example | fictitious command | Applies no CLI subcommand; health check endpoints are `GET /healthz` and `GET /ready` | See the Health-check table in `docs/deployment.md` |
| Early design debate questions (message format/long message threshold/attachment policy/Identity/monitoring/LLM provider/circuit breaker, etc. 17 items) | Answered by code | The answer is given by the code that has been implemented and is no longer a pending item | See the "Early questions answered by code" section of `docs/design.md` |

## 3. Deleted documents

| Entry | Type | Reason | Alternative or Current Status |
|---|---|---|---|
| `docs/todo.md` | Superseded (deleted) | Unclear structure, overlapping with design/deployment, and once carrying historical narratives such as "this round has been closed" | First replaced by `docs/roadmap.md`, which was renamed and narrowed to `docs/opengaps.md` on 2026-09-29 (only unfinished/unverified gaps, blockages and next-stage goals are included) |
| Temporary handover files in the root directory (no such files are left) | Temporary handover files, **never entered the git history** | Handover content should be returned to the permanent document, no temporary files in the root directory are left | Contents are merged into `docs/design.md`, `docs/deployment.md`, `docs/reference.md`, `docs/opengaps.md` |

---

## 4. Deleted compatibility path (code used to exist, now deleted)

Unlike the previous two sections: the following items **actually existed in the code** and are neither fictional nor "unadopted". They are compatible with early
The parsing layer retained by the environment variable deployment form has been deleted entirely - no compatibility commitment is required in the early stages of the project, and deletion is cheaper than retaining.
After deletion, `src/config.rs` does not contain any `std::env`. Read: process startup only reads 2 credentials plus listening port, and other business fields
All use Redis business configuration (`/api/bootstrap` or administrator PUT → `validate_nonblank` fail-closed).

| Entry | Type | Reason for deletion | Current status |
|---|---|---|---|
| `Config::from_env()` (line 311) of old `src/config.rs` | Removed function | Full 20-item environment variable parser. After the credentials are migrated to the Redis business configuration, it becomes the only compatibility layer and there is no longer any caller | The startup period is changed to `main.rs` for direct reading: `PORT`(:31), `REDIS_URL`(:40), `CONFIG_ENCRYPTION_KEY`(:49), `DEBUG_TOKEN`(:83); business fields are carried by `BusinessConfigWire` |
| `required_secret` / `required_nonblank` / `env_bool` | Deleted helper function | Only called by the above parser, deleted together with it | Required semantics changed to `validate_nonblank` in the Redis business configuration path (called at 8 places) |
| `RUN_MODE` variable + `Config.run_mode` field + startup verification block | Deleted identifier | The two values never change any runtime behavior: webhook and reconcile share the same set of routing tables, `POST /reconcile` is an independent endpoint. This variable only creates an additional branch that must be interpreted by the documentation | Zero hit across the board; `NG-SERVER-MODE` remains in `docs/charter.md` as a design non-target |
| `jmap_password()` secure accessor | Deleted method | Provides a non-disclosure read entry for the `SecretString` field, only meaningful in the env parsing path | `jmap_password` is reserved as a business configuration **field name** (`BusinessConfigWire` / `BusinessConfig`), the field itself is `SecretString` |
| `Config.port` / `Config.redis_url` field | Deleted field | `port` is hard-coded `8080` in the two constructors, and the listening binding takes the value directly from the environment variable - the field value can be silently inconsistent with the real listening port, and the full position is zero-read; `redis_url` is also zero-read | `Config` is narrowed to 7 fields: `telegram` / `jmap` / `account_id` / `llm` / `auth` / `worker_token` / `timezone` (the last item is mirrored from `BusinessConfig` for worker to render timestamp) |
| `redis_url` parameters of two constructors | Deleted parameters | The only reader (original line 72 of main.rs) was removed in the previous round of transformation | `redis_only()` and `from_business(value)`; the wrappers `from_business_json` / `from_business_value` remove the first parameter synchronously |
| 19 legacy environment variable names | Read removed | See above; these names no longer have any readers in the code | See `docs/reference.md` §5.3 for list; see `docs/design.md` §7.1 for semantics |
| `Channel` / `Notifier` / `MessageAdapter` (`src/channel.rs`) + `UserCommand` (`src/domain.rs`) | Removed placeholder traits and types | The three **no implementation, no caller, no dyn binding**, the `#[expect(dead_code)]` attribute is the only source of reference; the actual Telegram outbound `channel::telegram::TelegramClient` is held directly by `MetadataWorker` and `notify.rs` of `worker.rs` and never passes through them. The only user of `UserCommand` is the deleted `Channel` | `channel.rs` leaves only `pub mod telegram` (`TelegramClient`, `reqwest` self-developed); `domain.rs` leaves only the domain `Notification` and `pub mod jmap` module declaration (`Notification` is used in worker.rs:330); only `reload_async` remains in src/ 1 `expect(dead_code)` (overloaded API for testing) |

> Note the difference between `LLM_MAX_RETRIES` and `LLM_SUMMARY_TARGET_CHARS`: the former is never an environment variable
> (Redis running parameter `max_retries`, fallback to default, see `docs/reference.md` §6.1), the latter is a constant
> (300 is hard-coded in the two constructors and has not entered the business configuration wire).

### 4.1 Business/management routes mounted in boot mode

`router_configuration_setup` originally directly entrusted the full factory `router_with_worker_state_runtime_bootstrap_config`,
Therefore, all 12 business and management routes are registered in the boot mode, but none of them work: the `admin_token` in the boot mode is empty,
And `constant_time_eq` is guarded with `!expected.is_empty()`. An empty expected value will permanently reject any candidate credentials; even if the authentication passes,
`MemoryState` also cannot persist a bootstrap write.

| Entry | Type | Reason for deletion | Current status |
|---|---|---|---|
| `/api/bootstrap`, `/api/admin/session`, `/api/admin/session/revoke`, `/api/config`, `/api/enabled`, `/api/bus iness-config`, `/webhook/tg`, `/push/jmap`, `/api/push/register`, `/api/push/disable`, `/reconcile`, `/worker` | Deleted route registration | Permanent 401 route reads like "wrong credentials" | Self-built `AppState` in boot mode, only `/api/status`, `/ready`, `/healthz`; the above path returns `404` |

The difference between `401` and `404` is not mysophobia: `{"error":"unauthorized"}` is completely the same as "wrong credentials". Operators and probes will
"These endpoints do not exist at all in this mode" was misread as "try again with different credentials". Now the two are distinguishable - `/api/status` still returns
`{"missing":[...],"mode":"configuration-setup","ready":false}` (HTTP 200), business and management planes directly 404.
Regression guarded by `configuration_setup_mounts_only_status_and_probes`: assert 12 paths 404 one by one, and assert
`/healthz` 200, `/api/status` 200, `/ready` 503.

---

## 5. How to restart one of them

1. First register as a gap in `docs/opengaps.md` and attribute it to the stage.
2. If it involves Redis keys, TTL, HTTP routing or default values, update `docs/reference.md` in the same round.
3. The corresponding entry in this article is **retained** as the basis for decision-making - it records why we did not do this in the first place and do not delete it.
