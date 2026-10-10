# MessageWeave — Project Charter

> [中文版本 / Chinese version → charter.zh-CN.md](charter.zh-CN.md)

> This document is an exclusive specification for the **project: project goals, technology selection, safety invariants, implementation stages, prohibited matters,
> Test acceptance, document boundaries and **stable ID registry**.
>
> For general, language-independent code writing and environment construction specifications, see [`../AGENTS.md`](../AGENTS.md);
> The single authoritative source of verifiable facts (routing / Redis keys and TTL / configuration items / error codes / outbound constants) see
> [`reference.md`](reference.md); for gaps and blocking, see [`opengaps.md`](opengaps.md).
>
> When there is a conflict between the three: **The verifiable facts shall prevail in `reference.md`, the project constraints shall prevail in this document, and the quality rules shall prevail in `../AGENTS.md`. **

> **Code Baseline** `fc686ab` (src/ line number anchor)｜The document is based on the current `main`, and the anchor point must be reviewed along with the code baseline.

---

## 1. Project goals

A JMAP email notification service deployed on Docker + externally hosted Redis:

- Read-only pull email changes through JMAP to determine whether notification is needed.
- Optional LLM analysis (AI switch + user-level external authorization), the analysis results are **not stored or entered into the message**.
- Push **metadata only** notifications to **whitelisted chat** via Telegram.
- Users interact with `/help` through Telegram commands (`/summary`, `/search`).
- Configure business parameters through browser SPA; switch business processing on and off through protected interfaces.

**Not done**: Multi-tenancy, account system, clients other than Web UI, message persistence, and two-way email synchronization.

## 2. Technology selection (locked)

| Layer | Selection | Version |
|---|---|---|
| Language | Rust | edition 2021 |
| Web framework | axum | 0.8 |
| asynchronous | tokio | 1 |
| State Storage | Redis | redis 0.27 |
| JMAP Client | jmap-client | 0.4.2 (`default-features = false` + features `["async","rustls"]`) |
| Telegram | Self-built lightweight client (`src/channel.rs`), **not introducing** bot framework | — |
| HTTPS client | reqwest | 0.13 |
| key type | secrecy | 0.10 |
| encryption | ring | 0.17 |
| Log | tracing + tracing-subscriber | 0.1 / 0.3 |
| Scheduling | External cron → `POST /reconcile` | — |
| Front-end | Native HTML + single-file JS + single-file CSS, **No build tools, no frameworks** | — |
| Load balancer | Cloudflare Worker (Pure JS, zero dependencies) | — |

## 3. Security boundary (cannot be violated)

23, numbered `SAF-*` / `REQ-*` / `C-*`, all registered in the §8 register.

1. **JMAP read-only**: The Send class interface is not allowed, and any modification of the mailbox content is prohibited.
2. **No outbound sending**: `src/channel.rs` is the only Telegram outbound exit; direct sending of messages is prohibited at the domain layer.
3. **Long connections are prohibited**: WebSocket/SSE/long polling must not be introduced; all communications are short request-response.
4. **Stateless Recovery**: Do not use local files or memory states for recovery; only rely on replayable records in Redis after a crash.
5. **No database**: Relational or document databases are not allowed.
6. **No local writing**: The process does not write any files except the stdout log.
7. **The log only goes to stdout**: no log file is written, and no external log agent is connected.
8. **State only in Redis**: All mutable states only exist in externally managed Redis.
9. **Keys only come from environment variables**: Process configuration is not read from Redis.
10. **Keys are not echoed**: Any logs, error responses, and Debug output must not contain the key body.
11. **Key constant time comparison**: Admin credentials and admin session digest must be compared in constant time.
12. **Notifications only contain metadata**: Telegram notifications only contain sender/subject/time (+ number of attachments), and the text never enters the notification.
13. **AI results do not enter messages**: LLM output is only used for decision-making and does not enter Redis or Telegram messages.
14. **AI requires user-level external authorization**: Only chats that have been explicitly authorized and have not expired will trigger LLM; authorization is an external behavior and the process does not authorize on behalf of it.
15. **Business Whitelist**: Outbound Telegram chat must hit `CHAT_ALLOWLIST`.
16. **The diagnostic interface is closed by default**: `/debug/*` requires `DEBUG_ENABLED` (or `--debug`) + `DEBUG_TOKEN` to be satisfied at the same time.
17. **The diagnostic interface does not enter the load balancer**: `/debug/*` is not in Worker `SAFE_ROUTES` and can only be directly connected to the backend origin.
18. **The load balancer only transparently transmits and does not make decisions**: Worker does not parse the request body, does not verify the business logic, and does not issue `Retry-After`.
19. **No retry for AI results**: Failure of LLM analysis will be regarded as "no notification", no retry, and no downgrade to no AI notification.
20. **Stateless process**: Two container instances are safe to run concurrently; no local writes.
21. **No long polling scheduling**: Scheduling is triggered by external cron `POST /reconcile`.
22. **AI analysis is not persistent**: The analysis results and intermediate states other than the authorized state are not recorded.
23. **JMAP capability dynamic discovery**: Hard-coded JMAP capability list is prohibited and must be read at runtime.

## 4. Implementation phase (historical sequence, used to explain code structure)

| Stage | Content | Stable ID |
|---|---|---|
| 0 | Configuration/state storage interface abstraction, HTTP skeleton | `ARCH-DEPS-STAGE0` |
| 1 | JMAP read-only adapter | `ARCH-DEPS-STAGE1`, `MOD-JMAP-CLIENT` |
| 2 | Telegram channel self-built client (without introducing bot framework) | `ARCH-DEPS-STAGE4`, `MOD-TELEGRAM-NOTIFY` |
| 3 | AI optional access | `REQ-AI-EXTERNAL-CONSENT`, `REQ-AI-FUSE` |
| 4 | Redis implementation | `ARCH-DEPS-STAGE4` |
| 5 | Production (debug surface, load balancer, reconciliation) | `MOD-DEBUG`, `SAF-DEBUG-GATE` |

**Phase numbers are historical order, not current status statements. ** The current status is subject to `reference.md` and code.

## 5. Prohibited matters

1. **Dependencies not registered in §2 of this document may not be introduced. ** New dependencies must be registered in this document first and the reasons given.
2. **Do not delete tests, skip verification, relax inspection levels, or delete access controls in order to pass the access control. **
3. **Do not reference non-existent dependencies, functions, types, Redis keys or line numbers in documentation or comments. **
4. **You are not allowed to write "plan to implement X" comments** in the code; placeholder implementations must be explicitly marked as placeholders.
5. **No build tools or frameworks may be introduced in SPA/Load balancer**; `web/` maintains single-file JS + single-file CSS.
6. **The SPA must not be allowed to persist any credentials** (no `localStorage` / `sessionStorage` / Cookie assignments).
7. **Unverified code shall not be submitted**; the access control results must come from the actual execution of this round.
8. **Do not claim to have passed the real environment verification unless it is really connected this round. **
9. **Front-end resources must not be introduced outside `web/` and `cloudflare-worker/`**.
10. **Business configuration must not be allowed to silently fall back to environment variables. **

## 6. Test acceptance and access control

### 6.1 Change verification closed loop

Any changes involving runtime behavior or verifiable facts are done within the same change: Implementation → Testing → Documentation.

You must not change the code without changing the documentation, and you must not change the documentation without changing the code.

### 6.2 GATE-P0 (code access control)

Executed in the Debian minimum release container, the container must explicitly `export PATH=/usr/local/cargo/bin:$PATH`:

```bash
cd /home/okabe/Repo/messageweave && docker run --rm --user 1000:1000 \
  -e HOME=/tmp -e RUSTUP_HOME=/app/.gate-cache/rustup -e CARGO_HOME=/app/.gate-cache \
  -v "$PWD":/app -w /app rust:1-slim-bookworm \
  bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo fmt --all -- --check && cargo check --locked && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked 2>&1 | tail -12'
```

**Current baseline: 93 passed / 0 failed / 4 ignored** (Add 4 new cursor parsing and walk paging and retest after single test; ignored: `real_server_tests::session_list_and_read_smoke` and `debug::tests::debug_config_reports_timezone_of_business_configured_app` require external real JMAP Server credentials, `real_redis_ttl_tests::ttl_claim_dedup_sets_the_exactly_requested_expiry` and `real_redis_ttl_tests::ttl_consent_and_retry_landing_on_real_redis` require `REDIS_TEST_URL`).

**Note**: The bash `-lc` script of `docker run` must be wrapped in **single quotes**. Using double quotes will first expand `$PWD` / `$PATH` on the host, and cargo will not be found in the container.

### 6.3 GATE-DOCS (Document Access Control)

```bash
cd /home/okabe/Repo/messageweave && bash scripts/docs_check/run_all.sh
```

Pass all 6 validators and exit 0. Read only, never write files.

Added `check_file_size`: Source files with more than 500 lines under `src/`, `web/`, `cloudflare-worker/src/` must have
`SPLIT-EVAL:` tag (AGENTS.md §2.1). It proves that the mark exists, not that the written reasons for the split are tenable.

Added `audit_paths`: The path tokens in backticks in `docs/*.md` and `AGENTS.md` must actually exist.
The reason for its existence is that the document once appeared with a path that was never created, and the fictitious path read exactly like the real path, and the reader could not tell the difference.
"We decided not to do it" and "This is wrongly written". `docs/retired.md` is also within the scope of the check - the path in the abandonment registration must also be true,
The ungrounded form is only described in words. There are only three types of exemptions, all visible in the script: Owner prefix pointing to external repository
(`stalwartlabs/...`), git history to document deleted files, and a justified whitelist of well-known filenames.
It does not check the prose outside the backticks, and the bare file name is only parsed according to the same name verification.

**`0 error` only proves "reachability"**: the line number exists, the reference can be resolved, the number of table columns is consistent, and the mark is in place.
**It does not prove semantically correct. ** This distinction must be stated when reporting verification results.

### 6.4 Front-end testing

```bash
cd /home/okabe/Repo/messageweave/web && node --test *.test.mjs
cd /home/okabe/Repo/messageweave/cloudflare-worker && node --test test/*.test.js
```

### 6.5 Invariants must have assertions

Each safety invariant in §3 must have a test or code-level assertion that proves it holds.
Rather than just writing it in the document. Pure refactoring must not change the total number of tests.

### 6.6 External dependency testing

The business credentials of Stalwart and Telegram have been passed through real machine joint debugging - outbound, query and **inbound callback** directions
All have been verified (the registration, callback, and verification of `setWebhook` and `PushSubscription` are all passed), hosted Redis
Connected. Real environment integration tests must be marked with `#[ignore]` and will be silently skipped if credentials are missing (credentials will not be leaked and failure will not be judged).

**Environmental blocking items: None. ** The last two items have also been verified: the TTL value was measured on a real Upstash instance (`SET … NX EX` / `SET … EX 3600` / Lua `EXPIRE 86400` three atomic write points, running for 6 consecutive rounds, all green), `newState` of `Email/changes` was triggered in a real Stalwart environment (see docs/design.md §10.5). [`opengaps.md`](opengaps.md) The "blocking" area is currently empty.

## 7. Document boundaries

Answer only one question per document; **tables, lists and values must not be duplicated across documents.**

| Documentation | What to answer | Scope of authority |
|---|---|---|
| [`../AGENTS.md`](../AGENTS.md) | Universal, language-independent code writing and environment building specifications | Quality rules |
| `README.md` / `README.zh-CN.md` | User-oriented: what is it, how to run it, how to configure it, and operational and multi-instance reference | User-visible facts (information must be equivalent in Chinese and English) |
| `docs/design.md` | Why it is designed this way: data flow, module boundaries, state machines, error handling | Architectural intent |
| `docs/reference.md` | The single authoritative source of verifiable facts: routing, Redis keys and TTLs, configuration items, error codes, outbound constants | **verifiable facts** |
| `docs/opengaps.md` | Gaps, Blockages, Next Stage Goals | Open Items |
| `docs/retired.md` | Records and alternative links to abandoned or renamed plans | Historical decisions |
| `docs/charter.md` | This document: project constraints, technology selection, implementation stage, security invariants, stable ID registry | Project constraints |
| `web/` (no documentation, 4 files) | Manage SPA sources: static configuration pages and front-end logic, covered by `web/config.test.mjs` | Front-end behavior (authoritative facts documented in `docs/design.md` and `docs/reference.md`) |
| `cloudflare-worker/README.md` / `cloudflare-worker/README.zh-CN.md` | Configuration and semantics of the load balancer itself | Load balancer |

**Cross-document citation rules**:

- Disable the use of `§x.y` chapter numbers across documents (chapter numbers drift with editing); use stable IDs.
- **Within the same document** `§x.y` is allowed.
- New stable IDs must first be registered in the §8 registry; unregistered IDs may not appear in comments, documents, or identifiers.
- **When changing the public API/Redis key and TTL/configuration items/error codes/user-visible copy, all related documents will be synchronized in the same round. **
- **Newly added documents must be added to the `scripts/docs_check/` document list of each validator**, otherwise it will never be verified.

## 8. Stable ID registry

**The only authoritative index table. ** Always use stable IDs for cross-document references (no `§x.y` section numbers).

- ID is semantically stable, unique across the warehouse, and grepable; this table registers the **definition file** and one-sentence description of each ID.
- When adding a new constraint/component, first register a row in this table, and then write the `**ID**` tag at the definition point.
- New IDs must be registered before appearing in comments, documents or identifiers; the validator will reject unregistered IDs.
- When the content is moved, only the "Definition File" column in this table will be updated, and all references will be unchanged.

85 IDs in total, grouped by prefix: `C-*` (deployment constraints) · `NG-*` (non-target) · `MOD-*` (module) ·
`FLOW-*` (data flow) · `REQ-*` (requirement) · `SAF-*` (security) · `NFR-*` (non-functional) · `GATE-*` (gate control) ·
`BOUND-*` (boundary) · `ARCH-*` (architecture).

| ID | Definition file | One sentence | Category |
|---|---|---|---|
| `C-DOCKER` | README.md §3.0 | Docker deployment required | Deployment constraints |
| `C-DEBIAN-SLIM` | README.md §3.0 | Debian slim, disable Alpine | Deployment constraints |
| `C-NO-SECRET-IN-IMAGE` | README.md §3.0 | secrets are not included in the image | Deployment constraints |
| `C-RUSTLS` | README.md §3.0 | rustls + native-roots | Deployment constraints |
| `C-HTTPS-INBOUND` | README.md §3.0 | HTTPS-only inbound, plaintext HTTP within the container | Deployment constraints |
| `C-HTTPS-URL` | README.md §3.0 | Public HTTPS URL is provided by the platform (bot does not hold a certificate) | Deployment constraints |
| `C-AUTH-APP-BASIC` | docs/design.md §3.1 | Stalwart Authentication = App Password + Basic | Deployment Constraints |
| `C-NO-TCP-EXPOSE` | README.md §3.0 | Single listener `PORT`, no additional TCP ports exposed | Deployment constraints |
| `C-NO-LONG-CONN` | README.md §1 | No SSE/WS/long-polling equal-length connections | Deployment constraints |
| `C-REDIS-ONLY-STATE` | README.md §3.0 | State only external Redis, not SQLite/local volumes | Deployment constraints |
| `C-REDIS-MANAGED-AOF` | README.md §3.0 | Redis user hosting + enable AOF persistence | Deployment constraints |
| `C-PORT` | README.md §3.0 | Common PORT conventions | Deployment constraints |
| `NG-SERVER-MODE` | README.md §1 | `RUN_MODE=server` resident, non-target (this variable has been removed with `Config::from_env()`, this identifier is no longer in the code) | non-target |
| `NG-POLLING-SSE` | README.md §1 | EventSource/SSE long-lived, non-target | non-target |
| `NG-LONG-POLLING` | README.md §1 | Telegram long polling, non-target | non-target |
| `NG-SQLITE-PERSIST` | README.md §1 | SQLite persistence, non-target | non-target |
| `NG-LOCAL-VOLUME` | README.md §1 | Local volume persistence, non-target | non-target |
| `NG-SERVERLESS-BIND` | README.md §1 | Bind to specific serverless platform, non-target | non-target |
| `MOD-DEDUP` | README.md §6.1 | Redis `SET NX` idempotent key | Components |
| `MOD-STREAMS` | README.md §6.1 | Redis Streams queue + worker | Components |
| `MOD-SINCESTATE` | README.md §6.1 | sinceState cursor (stored in Redis) | Components |
| `FLOW-NEW-MAIL` | docs/design.md §5.4 | Push new mail flow | Data flow |
| `FLOW-RECONCILE` | README.md §3.10 | External Cron reconciliation + Redis loss recovery | Data flow |
| `REQ-AI-CONFIRM` | docs/design.md §12 | AI only touches text after explicit request + confirmation | Requirements |
| `REQ-VIEW-DIRECT` | docs/design.md §12 | View original text always JMAP direct fetch | Requirements |
| `REQ-LONG-EMAIL` | docs/design.md §12 | It is forbidden to send the full text of long emails, AI abstract ~300 words | Requirements |
| `REQ-ANALYSIS-EPHEMERAL` | docs/design.md §12 | Analysis/summary results are not persisted | Requirements |
| `REQ-ATTACH-ONDEMAND` | docs/design.md §12 | Pull attachments on demand | Requirements |
| `REQ-AI-FUSE` | docs/design.md §12 | AI failed about 3 times → circuit breaker + user confirmation fallback | Requirements |
| `REQ-LLM-OPENAI-COMPAT` | docs/design.md §12 | OpenAI-compatible environment variables | Requirements |
| `REQ-AI-EXTERNAL-CONSENT` | docs/design.md §12 / §3 | Only send text to external AI with explicit permission from the user | Requirements |
| `REQ-AI-CONSENT` | docs/design.md §12.3.1 | AI authorization period and Redis short-term TTL (authorization validity period, reask after expiration) | Requirements |
| `REQ-SINGLE-ACCOUNT` | docs/design.md §3.1/§11.3 | Single account; multiple accounts = multiple bot instances | Requirements |
| `REQ-PUSH-TYPES` | src/domain/jmap/client.rs:110 Comments | `PushSubscription/set` create does not have the `types` parameter in jmap-client 0.4.2; the subscription id must be narrowed to `Email` + `EmailDelivery` by `push_subscription_update_types` before being exposed to the outside world | Requirements |
| `REQ-RECONCILE-IDEMPOTENCY` | src/state.rs `claim_dedup` + `get_reconcile_state` / docs/design.md §8.2 | JMAP reconciliation cursor is only advanced after all paging events are successfully enqueued (XADD); a single reconciliation is guaranteed to fly solo (TTL) by Redis SET NX EX lock `lock:reconcile` 300s, owner token is renewed for 90s, only the holder can renew/release); the processing end then goes through `claim_dedup` (SET NX EX, 86400s) to ensure that the same flow message is not delivered repeatedly | Requirements |
| `REQ-TIMEZONE-DISPLAY` | src/config.rs `SUPPORTED_TIMEZONES` / docs/reference.md §5.4 | Notification receipt time is configured by business `timezone` (IANA, default `Asia/Shanghai`) is rendered as `%Y-%m-%d %H:%M`; only 16 non-daylight saving time zones are accepted, no match is returned 422, no time zone library inference | Requirements |
| `NFR-NOTIFY-SLA` | README.md §6.3 | Notification availability ≥99.9%, minor latency allowed | Non-functional |
| `NFR-RECONCILE-INTERVAL` | README.md §3.10 | External cron reconciliation interval 5–10 minutes | Non-functional |
| `SAF-NOTIFY-META` | §3 | New email notifications only contain metadata, and the text is not included in the notification | Security |
| `SAF-CHAT-ALLOWLIST` | §3/docs/design.md §7.3 | CHAT_ALLOWLIST hard constraint, reject non-whitelist before processing | Security |
| `SAF-AUTH-RECONCILE` | §3/docs/design.md §7.3 | `/reconcile` requires `Authorization: Bearer RECONCILE_TOKEN`, fail-closed | Security |
| `SAF-AUTH-TG-WEBHOOK` | §3/docs/design.md §7.3 | `/webhook/tg` requires header `X-Telegram-Bot-Api-Secret-Token == TG_WEBHOOK_SECRET` | Security |
| `SAF-AUTH-JMAP-PUSH` | §3 / docs/design.md §7.3 | `/push/jmap` Press `pushSubscriptionId` to check the Redis short-term verification status (save `session_digest` digest), if it is missing or the digest does not match, it will be rejected; if the status is missing, it will fall back to worker re-verification (fail-closed) | Security |
| `SAF-ADMIN-SESSION` | src/config.rs `session_digest` / §3 | Returns the SHA-256 digest used for Redis admin-session records; the bearer token itself is never written to Redis | Security |
| `SAF-NO-SECRET-ECHO` | src/config.rs `SecretString` fields and the public configuration API | The secret field does not implement `Debug` and never enters the public configuration API response; the Redis wire format type is private and must not be serialized into the HTTP response | Security |
| `SAF-PROBE-PUBLIC` | §3 / docs/design.md §7.3 | `/healthz`, `/ready` Public probes: no authentication, no sensitive information | Security |
| `ARCH-HEALTHZ` | docs/design.md §7.3/§10.0 | `/healthz` liveness (process survival), long-term semantic stability | Architecture |
| `ARCH-READY-BASELINE` | docs/design.md §7.3/src/notify.rs `ready`+`probe_jmap_session`+`probe_telegram_get_me` | `/ready` End-to-end ready: configuration integrity + Redis reachability + **outbound read-only probe** (JMAP session `GET`, Telegram `getMe`, each `PROBE_TIMEOUT`=3000ms, parallel, worst about 3s), all four exceed `200` (readiness report JSON contains real `jmap`/`telegram` fields), any one fails `503` (standard error envelope `{"error":"service_unavailable","request_id":<id>}` + `Retry-After: 30`); the probe is read-only, stateless write (`refresh_business_config` is only read from Redis), the bot token is only used to spell URL | Schema |
| `GATE-P0` | docs/design.md §10.0 | Stage 0 P0 gate control (fmt/clippy/test passed Debian container, etc. 8 items) | Process |
| `BOUND-STAGE1` | docs/design.md §10.0 | Stage 1 Pushing the Boundary (Enter after passing GATE-P0; R1 entrance authentication has been implemented in Stage 0) | Process |
| `ARCH-CONFIG-ENV` | docs/design.md §7.1 | Configuration = environment variables manually parsed, no figment/TOML | Architecture |
| `ARCH-AXUM-08` | docs/design.md §10.0 | Single-port axum 0.8 entry (version is subject to Cargo.toml) | Architecture |
| `ARCH-STAGE0` | docs/design.md §10.0 / §5.3 | Phase 0 Actual Delivery and Current Status | Architecture |
| `ARCH-DEPS-STAGE0` | docs/design.md §10.0 / §4 | Stage 0 actual dependency set (axum/serde/secrecy/subtle/tokio/tracing…) | Architecture |
| `ARCH-DEPS-STAGE1` | docs/design.md §10.0-1 / §4 | Phase 1 dependency status: `jmap-client 0.4.2` has been introduced, version/features shall be subject to Cargo.toml, WebSocket feature is disabled | Architecture |
| `ARCH-DEPS-STAGE4` | docs/design.md §10.0-1 / §4 | Current dependency status after stage 3.5/4: `redis 0.27`, `reqwest 0.13` have been introduced and actually used; `teloxide` has not been introduced (Telegram is self-developed by `src/channel.rs` and implemented by reqwest); the version must be `Cargo.toml` Subject | Architecture |
| `MOD-JMAP-CLIENT` | docs/design.md §6-7 / §4 | `domain::jmap::client` true read-only adapter (session/account/mailbox/email read-only) | Components |
| `MOD-TELEGRAM-NOTIFY` | src/worker.rs / docs/design.md §12 | Bounded metadata notification worker: consume outbound queue, deliver masked notification via Telegram (does not carry body/AI response) | Components |
| `REQ-JMAP-SESSION-URL` | docs/design.md §7.1 / .env.example | `JMAP_SESSION_URL` accepts the service base address or complete /.well-known/jmap, normalizes it to origin/base and then submits it jmap-client (no duplicate paths); **The code has been implemented (D-G1-1) and has been verified on a real machine with a real account** (2026-09-28: `fetchChanges` persistence `baseline:` cursor, `Email/query` search replies, all confirmed by the user) | Requirements |
| `SAF-JMAP-URL` | docs/design.md §7.1 / §4 | JMAP URL constraints: HTTPS only, disallow inline credentials, deny dangerous query | Security |
| `REQ-JMAP-RAW-MULTIPART` | docs/design.md §3.2/§10.1 | `read_email` Multi-part Original text: Splice the parts "with part_id and bodyValue" in text_body order; no available part → clear error | Requirements |
| `GATE-G1-JMAP-READONLY` | docs/design.md §10.1 / §6 | G1 access control: read-only adapter **Code has been implemented** (mock + `#[ignore]` real machine test), **to be real `cargo test -- --ignored jmap::` verification** | Process |
| `ARCH-LB-WORKER` | README.md §2 | Multi-instance LB/HA: a free-plan Cloudflare Worker as the only *registered* entrance, plus failover. The backends stay directly reachable and it adds no access control | Architecture |
| `C-LB-SINGLE-REG-URL` | README.md §2 | Telegram/Push/Cron only registers the stable URL of the Worker; the backend platform entrance does not register externally | Constraints |
| `C-LB-SHARED-SECRETS` | README.md §2 | Multiple instances must share the same set of `SAF-AUTH-*` secrets, otherwise a random 401 | Constraints |
| `SAF-LB-PASSTHRU` | §3 | Trust model = transparent transmission: Worker does not overwrite authentication; the backend must continue fail-closed verification (the backend may be directly connected to the public network) | Security |
| `SAF-RECONCILE-LOCK` | README.md §6.1 | `/reconcile` does not fan out, Redis lock guarantees single instance execution and avoids repeated reconciliation | Security |
| `MOD-STREAMS-GROUP` | README.md §6.1 | Multiple instances use the same Streams consumer group name, and Redis will automatically allocate it (at-least-once will not repeat processing) | Components |
| `MOD-HEALTH-AGG` | README.md §2 | Worker aggregate health view, reporting the survival of each backend for external monitoring | Components |
| `MOD-DEBUG` | src/debug.rs / docs/design.md §7.6 / docs/reference.md §3 | Remote joint debugging read-only surface: `DEBUG_ENABLED` (or `--debug`) + `DEBUG_TOKEN` mounted after double factor is enabled `/debug/*`, otherwise it will not be mounted | component |
| `SAF-DEBUG-GATE` | src/main.rs / src/debug.rs / docs/design.md §7.6 | Two-factor gate: "`DEBUG_ENABLED` is true or command line with `--debug`" **and** `DEBUG_TOKEN` Routes are mounted only if they are not empty; if any are missing, they are not mounted at all (the request fails with `404`), and it is absolutely closed by default. The start signal goes through env instead of argv, so that the startup command remains static, and the switch can be switched at a single point on the platform console | Security |
| `SAF-DEBUG-AUTH` | src/debug.rs | After mounting `/debug/*` requires `Authorization: Bearer DEBUG_TOKEN` constant time comparison, fails with `401` and has no side effects | Security |
| `REQ-DEBUG-ENDPOINTS` | src/debug.rs / docs/reference.md §3 | Endpoint contracts: `GET /debug/ping`, `/config`, `/redis`, `/jmap`, `/telegram`, `/worker` are all read-only; `POST /debug/notify` Send a test message through the real outbound link; the response body does not contain the original text of secret (the credential field only returns `*_configured` Boolean, and the non-ciphertext identity and budget fields are still returned in plain text) | Requirements |
| `SAF-DEBUG-ORIGIN-ONLY` | docs/reference.md §4 | `/debug/*` is not among the 19 safe routes of the load balancer, and the Worker will always receive `404 route not forwarded`; it can only be directly connected to the backend origin, and the public network is unreachable | Security |
| `SAF-DEBUG-ALLOWLIST` | src/debug.rs | `POST /debug/notify` only verifies `chat_id` when the chat whitelist** is not empty**; it will not intercept when the whitelist is not configured (empty), so to enable this page, you must confirm that the business whitelist has been configured | Security |
| `NFR-HA-MULTI-INSTANCE` | README.md §6.1 | Multi-instance high availability semantics; both active-active or active-standby; Redis single point of failure is not within the scope of the solution (user external solution) | Non-functional |
| `C-NO-DB` | §3 | Production does not use any database (no SQLite/Postgres/MySQL/embedded), Redis is the only state store; the application does not connect to a second database | Constraints |
| `C-NO-LOCAL-WRITE` | §3 | Disable local file/directory writes (log/data/tempcache/local volumes) | Constraints |
| `C-LOG-STDOUT-ONLY` | §3 | The log only writes stdout/stderr, collected by the platform; disables the file log backend | Constraints |
| `SAF-LOG-PURITY` | §3 | Log and Redis write content is limited to structured events/counts/timestamps/masked summaries; keys/email bodies/AI request responses/attachment content are prohibited | Security |
| `SAF-ENABLE-FLAG` | src/notify.rs `put_enabled` / src/state.rs `config:enabled` | The global switch is Redis single key `config:enabled`; if it is not written, it is regarded as closed, and `business_enabled` will be treated as closed if an error occurs (fail-closed); writing requires admin-session Bearer | Security |
| `C-NO-STATEFUL-RECOVERY` | §3 | It is prohibited to rely on in-process status for production recovery; all recovery will use Redis + JMAP reconciliation; in-process caching is only for performance optimization, and loss must be safe and reentrant | Constraints |
| `ARCH-STATE-REDIS` | docs/design.md §10.0 / Redis is the only state source | The state layer uses Redis (Streams/SET NX/Lock/Abstract), and the process does not hold recoverable state | Architecture |
| `C-REDIS-EXTERNAL` | README.md §3.0 | Redis is provided by an external certified instance and is not in the same container as this service | Deployment constraints |
| `GATE-UPTIME-KUMA` | docs/reference.md §9.4 | `/healthz` stable semantics can be directly connected to external probes such as Uptime Kuma | Access Control |
| `GATE-DOCS` | scripts/docs_check/run_all.sh | Document access control: All validators must have 0 error and exit 0 to be considered passed | Access control |
