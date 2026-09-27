# MessageWeave — Reference

> **This file is the single source of truth for verifiable facts.**
> When any other document disagrees with this one, this one wins.
>
> **Verified against commit `ed93358`.** Line numbers in this file were read from that
> commit with the working tree clean.
>
> **Maintenance responsibility.** Any change to the public API of `src/config.rs`,
> `src/state.rs`, `src/worker.rs`, or `src/notify.rs`, or to **any Redis key name or TTL**,
> must update this file in the same change. Reviewers: reject a change that touches those
> files without touching this one.

Conventions used throughout:

- **TTL** is in seconds. "No EX" means the key never expires and lives until explicitly deleted.
- **Writer / Reader** name the code symbol that performs the operation; the line number is
  an annotation for fast navigation, not an identity.
- Only this file carries line numbers. All other documents reference symbols by name.

---

## 1. Redis keys

### 1.1 Configuration

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `config:business` | No EX | `set_business_config` (state.rs:530, key at state.rs:534); first-time write via `SET NX` in `initialize_business_config` (state.rs:545) | `get_business_config` (state.rs:519) |
| `config:business:revision` | No EX | atomic `INCR` on every config write (state.rs:538); `SET 1 NX` on first-time init (state.rs:558) | `business_config_revision` (state.rs:593) |
| `config:outbound` | No EX | `set_outbound_config` (state.rs:506) | `get_outbound_config` (state.rs:490) |
| `config:enabled` | No EX | `set_enabled` (state.rs:611) | `is_enabled` (state.rs:602) |
| `config:admin_session` | EX 900 | `put_admin_session` (state.rs:568, key at state.rs:571) | `admin_session_valid` (state.rs:580); cleared by `revoke_admin_session` (state.rs:589) |

Notes:

- `config:business:revision` is an **atomic Redis `INCR`**, not a read-modify-write. Hot reload
  compares it: the refresh loop only rebuilds the worker when the remote revision exceeds the
  cached local value, which is what keeps a single bad poll from turning into a hot retry loop
  (notify.rs:627-650).
- `config:admin_session` carries no TTL metadata beyond the literal `EX 900` passed at write
  time; the handler reports the same window in its response body as `expires_in: 900`
  (notify.rs:700).
- `config:enabled` is consulted as a gate; missing or `false` keeps business processing off.

### 1.2 AI consent

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `consent:ai:{chat_id}` | EX 3600 / 86_400 / 604_800 / 31_536_000 | `set_ai_consent` (state.rs:458; key at state.rs:460, `EX` at state.rs:466) | `ai_consent_until` (state.rs:472); cleared by `clear_ai_consent` (state.rs:481) |

The stored value is an **absolute Unix expiry timestamp**, not a duration; the key's `EX`
carries the same duration, so the key removes itself (state.rs:461-466).

Trigger words are exact Chinese literals with **no English aliases** — the in-app help text
enforces this itself (`worker.rs:569`):

| Input | TTL | Label |
|---|---|---|
| `/ai on`, `临时一次`, `临时`, `一次` | 3600 | `1小时` |
| `今天` | 86_400 | `今天` |
| `7天` | 604_800 | `7天` |
| `直到我撤销`, `长期` | 31_536_000 (365 d) | `直到我撤销` |
| `/ai off`, `关闭 ai`, `撤销授权`, `停止摘要` | — | revoke |

`1小时` is a **category label, not a trigger word**: typing `1小时` alone grants nothing.
Consent never auto-renews.

### 1.3 Delivery pipeline

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `stalwart:jmap`, `stalwart:telegram` | Stream, no EX | `State::enqueue` (state.rs:241, `XADD` at state.rs:243); call sites notify.rs:123 (TG webhook) and notify.rs:772 (JMAP push); reconcile path via `claim_dedup_and_enqueue` (worker.rs:373) | `read_batch` over consumer group `stalwart-workers`, consumer `http-worker` (notify.rs:265-266, :280) |
| `delivery:inflight:{stream}:{id}` | EX 60 | key built at notify.rs:315, `claim_dedup` at notify.rs:316 | `release_dedup` on completion (notify.rs:329) |
| `delivery:committed:{stream}:{id}` | EX 604_800 | key built at notify.rs:294, `claim_dedup` at notify.rs:328 | `dedup_exists` before send (notify.rs:295) |
| `retry:{stream}:{id}` | EX 86_400 | `retry_or_dlq` Lua script (key state.rs:434, `INCR` state.rs:438, `EXPIRE` state.rs:439) | reclaim path |
| `stalwart:jmap:dlq`, `stalwart:telegram:dlq` | Stream, no EX | Lua `XADD` at state.rs:441; name built at notify.rs:348, `max_attempts` 3 at notify.rs:349 | **nothing in code reads it** |

There is no `delivery:pending:{stream}` key — "pending" refers to the Redis Streams
pending-entries list (PEL), which Redis maintains internally.
The stream names above are concrete: `stalwart:jmap` and `stalwart:telegram`;
`{stream}` in the remaining keys is one of these two.
The mistake is recorded in `docs/retired.md`.

Delivery is at-most-once per event: the `delivery:inflight` key is claimed with a 60 s window
before the outbound send, and `delivery:committed` extends the guarantee to seven days after
acknowledgement.

The DLQ is **append-and-ack**: the same Lua script that appends to `stalwart:jmap:dlq` /
`stalwart:telegram:dlq` also `XACK`s the message out of the source stream, so increment, DLQ
append and source acknowledgement are atomic (state.rs:438-442). No code path reads the DLQ
back — replay is an operator action, not a service feature.

### 1.4 Idempotency and rate limits

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `dedup:tg:{update_id}` | EX 86_400 | key at notify.rs:118, `claim_dedup` at notify.rs:119 | — |
| `dedup:jmap:{account}:{email}` | EX 86_400 | key at notify.rs:767, `claim_dedup` at notify.rs:768; same key rebuilt at worker.rs:360 and claimed with `claim_dedup_and_enqueue` at worker.rs:373 | — |
| `ratelimit:push-verify:{subscription}` | EX 30 | key at notify.rs:728, `claim_dedup` at notify.rs:729 | — |
| `state:jmap:since` | **No EX** | `set_reconcile_state` (state.rs:629; key at state.rs:632, `SET` without `EX`) | `get_reconcile_state` (state.rs:621) |

`state:jmap:since` is the only state key that survives indefinitely without an explicit
delete, by design: it is the reconciliation cursor, and expiring it would force a full
re-baseline on the next restart.

### 1.5 Locks

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `lock:reconcile` | 300 | `acquire_lock` (notify.rs:159) | `renew_lock` heartbeat 90 (notify.rs:172) |
| `lock:push-register:{sha256(callback_url)}` | 360 | `acquire_lock` (notify.rs:819) | `release_lock` (notify.rs:835, 907) |

**Why the registration lock is 360 s and not 300 s.** The lock must outlive the longest
configured JMAP request timeout (300 s), otherwise a slow registration could admit a
duplicate. Source comment, notify.rs:816:

> The lock must outlive the configured 300s maximum JMAP request timeout; this prevents a
> slow create from admitting a duplicate.

### 1.6 Push subscription state

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `push:subscription:{id}` | EX 300 (caller-supplied, min 1) | `remember_push_subscription` (state.rs:647, called at notify.rs:746) | `push_subscription_verified` (state.rs:663) |
| `push:subscription:id` | No EX | `remember_push_subscription_id` (state.rs:672) | current-subscription lookup |
| `push:subscription:{id}:status` | 900 / 300 / 86_400 (min 1 enforced) | `set_push_subscription_status` (state.rs:679, key at state.rs:687); pending at notify.rs:873, verified at notify.rs:758, disabled at notify.rs:944 | **none** |
| `push:registration:{sha256(callback_url)}` | EX 604_800 | `remember_push_subscription_for_callback` (state.rs:732, key at state.rs:739, `EX` at state.rs:742; called at notify.rs:890) | `get_push_subscription_for_callback` (state.rs:721); removed by `remove_push_subscription_for_callback` (state.rs:748) |
| `push:orphan:{subscription_id}` | EX 604_800 | `record_push_orphan` (state.rs:712) | orphan sweep |

**Two distinct keys that are frequently confused. Read this before editing either.**

- `push:registration:{sha256(callback_url)}` is a **mapping from callback URL to
  subscription id**, TTL seven days. It is not a lock.
- `lock:push-register:{sha256(callback_url)}` is the **single-flight registration lock**,
  TTL 360 s. It is not a mapping.

They use the same digest but are separate keys with unrelated lifetimes and purposes.

**Annotation: `push:subscription:{id}:status` is write-only by design.**
The source carries an explicit note at state.rs:169:

> `push:subscription:{id}:status` is currently write-only

It is an ops / observability trail intended for `redis-cli` inspection; it is **not** a gate
and nothing reads it for authorization. The three values ever written are `pending` (900 s),
`verified` (300 s) and `disabled` (86400 s) — see the table above for their call sites.
Do not infer correctness from its presence.

---

## 2. Error envelope

Every error response uses one envelope shape:

```json
{ "error": { "code": "<machine_code>", "message": "<human readable>" } }
```

| Status | Code | Retry-After |
|---|---|---|
| 400 | `invalid_request`, `missing` | — |
| 401 | `unauthorized` | — |
| 403 | `forbidden` | — |
| 409 | `conflict` | — |
| 422 | `invalid_configuration` | — |
| 500 | `internal_error` | — |
| 503 | `service_unavailable`, `disabled`, `push_subscription_not_found`, `push_verify_rate_limited` | `30` when retryable |

`Retry-After` is set only when the `retry` flag is true, inside `error_response`
(notify.rs:243-244), and is emitted unconditionally by the readiness handler for a not-ready
503 (notify.rs:1003). The header value is always the literal string `"30"`.

`GET /api/status` is the exception that reports setup state with a non-envelope body.

---

## 3. Backend routes

All registered in `router_with_worker_state_runtime_bootstrap_config` (notify.rs:1187-1201).

| Method | Path | Handler |
|---|---|---|
| POST | `/webhook/tg` | `telegram_webhook` |
| POST | `/push/jmap` | `jmap_push` |
| POST | `/api/push/register` | `register_push` |
| POST | `/api/push/disable` | `disable_push` |
| POST | `/reconcile` | `reconcile` |
| POST | `/worker` | `worker` |
| GET, PUT | `/api/config` | `get_config` / `put_config` |
| GET, PUT | `/api/enabled` | `get_enabled` / `put_enabled` |
| PUT | `/api/business-config` | `put_business_config` |
| POST | `/api/bootstrap` | `bootstrap` |
| POST | `/api/admin/session/revoke` | `revoke_admin_session` |
| POST | `/api/admin/session` | `create_admin_session` |
| GET | `/healthz` | `healthz` |
| GET | `/ready` | `ready` |
| GET | `/api/status` | `setup_status` |

Response conventions:

- `GET /healthz` returns **200 unconditionally**. It is a liveness probe and must not be
  used to decide whether to route traffic.
- `GET /ready` returns **503** when configuration or dependencies are not ready, and carries
  `Retry-After: 30` for not-ready responses.
- `GET /api/status` returns **503** with body `{"status":"configuration-setup","missing":[...]}`
  when a required environment variable is absent.
- Static assets are served with a strict CSP; see §7.

---

## 4. Gateway vs backend route matrix

> **Independently verified.** Source: `cloudflare-worker/src/backends.js` `SAFE_ROUTES`
> (backends.js:9-22), 12 entries. The worker entry point is `src/index.js`
> (wrangler.toml:20); `src/lb.js` performs forwarding and bounded failover (`SAF-LB-PASSTHRU`,
> `C-NO-LONG-CONN`).

The gate is **unconditional and fail-closed**. The worker reads no configuration switches at
all: a path missing from `SAFE_ROUTES` returns **404** (index.js:69-72), a registered path
with the wrong method returns **405** (index.js:73-76), and a missing or unparseable backend
pool returns **503** rather than passing the request through (index.js:80-82).

**Forwarded by the worker (12):**

`/` · `/assets/config.js` · `/assets/styles.css` · `/api/status` · `/api/config` ·
`/api/business-config` · `/api/admin/session` · `/api/admin/session/revoke` ·
`/webhook/tg` · `/push/jmap` · `/reconcile` · `/ready`

**Registered on the backend but NOT forwarded (6):**

| Path | Why it is absent from the gateway |
|---|---|
| `POST /api/push/register` | Push registration requires the callback URL exchange to complete directly against the backend |
| `POST /api/push/disable` | Same reasoning as registration |
| `GET, PUT /api/enabled` | Internal operational switch, not exposed through the public gateway |
| `POST /api/bootstrap` | One-shot trust bootstrap; kept off the public path |
| `POST /worker` | Internal worker endpoint |
| `GET /healthz` | Liveness is aggregated by the gateway itself |

Consequence: the SPA's first-boot flow cannot drive `/api/bootstrap` through the worker.
Bootstrap must be performed against the backend origin directly, or the bootstrap path must
be added to the gateway allowlist.

---

## 5. Environment variables

Three distinct layers. They are not interchangeable.

### 5.1 Boot-time (2 required, 2 optional with defaults)

| Variable | Required | Default | Source |
|---|---|---|---|
| `REDIS_URL` | yes | — | read at main.rs:39 |
| `CONFIG_ENCRYPTION_KEY` | yes | — | `encryption_key_from_env` at main.rs:46 |
| `PORT` | no | `8080` | `unwrap_or(8080_u16)` at main.rs:33 |
| `RUN_MODE` | no | `webhook` | `unwrap_or_else` at config.rs:338 |

`RUN_MODE` accepts only `webhook` or `reconcile`; anything else is rejected at boot
(main.rs:71-78). Both modes currently route to the same no-op router; the distinction is
load-bearing for future splits, not for behaviour today.

**Missing a required variable does not crash the process.** It logs a warning and serves
`router_configuration_setup`, which builds a router over `MemoryState` with empty tokens and
a `NoopWorker` (notify.rs:1097-1109). The container stays up and answers requests while doing
no business work. This is a deliberate fail-closed-to-setup posture, and it is the single
most likely cause of "the container is healthy but nothing happens".

### 5.2 Redis-resident business configuration

Written by the SPA through `PUT /api/bootstrap`, then hot-reloaded by
`PUT /api/business-config` (effective within 1 s). The process never reads these from the
environment in normal operation.

### 5.3 Legacy compatibility (read but not required)

`config.rs` still reads an environment-shaped business configuration for backward
compatibility. Recognised names, unprefixed unless noted:

`BOT_TOKEN` · `CHAT_ALLOWLIST` · `TG_WEBHOOK_SECRET` · `TELEGRAM_CHAT_ID` ·
`JMAP_*` · `RECONCILE_TOKEN`

The single prefixed name is `TELEGRAM_CHAT_ID`. Anything else documented as
`TELEGRAM_BOT_TOKEN` or similar is a documentation error, not a supported variable.

**Trust root.** The bootstrap one-shot token is the password component of `REDIS_URL`
(main.rs:87-91). The Redis ACL credential is therefore simultaneously the application
connection credential and the trust root for bootstrap.

---

## 6. Outbound and runtime parameters

### 6.1 Outbound

| Parameter | Default | Range / cap | Source |
|---|---|---|---|
| `jmap_timeout_ms` | 15_000 | 100..=300_000 | default state.rs:55; validated notify.rs:411 |
| `telegram_timeout_ms` | 10_000 | 100..=300_000 | default state.rs:56; validated notify.rs:412 |
| `llm_timeout_ms` | 30_000 | 100..=300_000 | default state.rs:57; validated notify.rs:413 |
| `max_retries` | 3 | hard cap 5 | default state.rs:58; rejected at notify.rs:414; re-clamped at channel.rs:87 |

There is **no** `LLM_MAX_RETRIES` environment variable; retry count lives in
`config:outbound` and is bounded at 5 regardless of what is written.

### 6.2 Reconcile budgets

| Constant | Value | Source |
|---|---|---|
| `MAX_BASELINE_PAGES` | 100 | worker.rs:248 |
| `MAX_BASELINE_EMAILS` | 10_000 | worker.rs:249 |
| `RECONCILE_BUDGET` | 20 s | worker.rs:250 |
| `CHANGE_WINDOW_CAP` | 4_096 | worker.rs:306 |
| Lock TTL / heartbeat | 300 s / 90 s | notify.rs:159 / 172 |

`max_changes` is a function parameter, not a constant.

### 6.3 Idle threshold

XAUTOCLAIM's idle threshold is **batch size × 300 s**, not a fixed 300 s.

---

## 7. Static assets and response headers

Served from `src/web.rs`.

- Three routes only: `GET /`, `GET /assets/config.js`, `GET /assets/styles.css` (web.rs:21-23).
  Everything else returns 404.
- Assets are embedded at compile time with `include_str!` (web.rs:15-17). There is no
  filesystem access at runtime.
- Every response carries `Content-Type`, `Cache-Control: no-store`,
  `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer` (web.rs:43-51).
- CSP is applied **only to the HTML document**: the constant is at web.rs:39, inserted inside
  an `if html` block (web.rs:53-58):

  ```
  default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self';
  form-action 'self'; base-uri 'none'; frame-ancestors 'none'
  ```

- `X-Frame-Options: DENY` is set alongside the CSP, on the HTML response only (web.rs:58).

`default-src 'none'` is deliberately stricter than `'self'`: the page is closed by default
and only `script-src`, `style-src` and `connect-src` re-open a same-origin channel.

---

## 8. Not implemented

Recorded here so that references elsewhere cannot be mistaken for shipped features.

- `/search` is not implemented. `SearchSnippet` and `search_snippet` have zero occurrences in
  `src/`.
- There is no `Search` variant in the worker's `Intent` enum, so the search path cannot be
  entered even indirectly.
- The Cloudflare Worker contains no Rust: `cloudflare-worker/src/` holds only `index.js`,
  `lb.js`, `backends.js` and `health.js`. Any `*.rs` path written under `cloudflare-worker/`
  is a documentation error, not a source file.

---

## References

External dependencies, all reachable and returning HTTP 200 at the time of verification:

- axum — https://docs.rs/axum
- redis (Rust crate, 0.27) — https://docs.rs/redis/0.27/redis/
- reqwest (Rust crate, 0.13) — https://docs.rs/reqwest/0.13/reqwest/
- jmap-client (0.4.2) — https://docs.rs/jmap-client/0.4.2
- JMAP specification — https://jmap.io/spec/
- JMAP RFC 8620 — https://datatracker.ietf.org/doc/rfc8620/
- Redis Streams — https://redis.io/docs/latest/develop/data-types/streams/
- Redis persistence — https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/
- Cloudflare Workers — https://developers.cloudflare.com/workers/
- Cloudflare Workers runtime APIs — https://developers.cloudflare.com/workers/runtime-apis/
- RFC 8030 (Web Push) — https://www.rfc-editor.org/rfc/rfc8030
- RFC 7231 §7.2.3 — https://www.rfc-editor.org/rfc/rfc7231#section-7.2.3
- Dockerfile reference — https://docs.docker.com/reference/dockerfile/
- rust Docker image — https://hub.docker.com/_/rust
- Uptime Kuma — https://uptime.kuma.pet/
- OWASP Logging Cheat Sheet — https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html

Not linked because they do not resolve to documentation: `crates.io/crates/jmap-client`
(404) and `platform.openai.com` documentation paths (403). The project describes the LLM
dependency as OpenAI-compatible and documents it through `reqwest` instead.
