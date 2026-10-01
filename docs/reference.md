# MessageWeave — Reference

> [中文版本 / Chinese version → reference.zh-CN.md](reference.zh-CN.md)

> **This file is the single source of truth for verifiable facts.**
> When any other document disagrees with this one, this one wins.
>
> **This file carries no source line numbers, by design.** They are not part of the
> verification: this file documents *which symbol* does *what*, and line numbers are
> renumbered by every unrelated edit to the same file. `docs/deployment.md` keeps its
> line numbers, because that file is an operator runbook read mid-incident.
>
> **Maintenance responsibility.** Any change to the public API of `src/config.rs`,
> `src/state.rs`, `src/worker.rs`, or `src/notify.rs`, or to **any Redis key name or TTL**,
> must update this file in the same change. Reviewers: reject a change that touches those
> files without touching this one.

Conventions used throughout:

- **TTL** is in seconds. "No EX" means the key never expires and lives until explicitly deleted.
- **Writer / Reader** name the code symbol that performs the operation. Symbol names are
  the identity; this file deliberately records no line numbers (see the note above), so
  it stays true across renumbering.

---



---

## 1. Redis keys

### 1.1 Configuration

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `config:business` | No EX | `set_business_config`; first-time write via `SET NX` in `initialize_business_config` | `get_business_config` |
| `config:business:revision` | No EX | atomic `INCR` on every config write; `SET 1 NX` on first-time init | `business_config_revision` |
| `config:outbound` | No EX | `set_outbound_config` | `get_outbound_config` |
| `config:enabled` | No EX | `set_enabled` | `is_enabled` |
| `config:admin_session` | EX 1800 (30 minutes) | `put_admin_session` | `admin_session_valid`; cleared by `revoke_admin_session` |

Notes:

- `config:business:revision` is an **atomic Redis `INCR`**, not a read-modify-write. Every
  request-bearing entry point calls `refresh_business_config`, which only
  rebuilds the worker when the remote revision exceeds the cached local value — that
  guard is what keeps a stale or malformed snapshot from turning into a rebuild loop.
- `config:admin_session` carries no TTL metadata beyond the literal `EX 1800` passed at write
  time; the handler reports the same window in its response body as `expires_in: 1800`
  on the handler side.
- `config:enabled` is consulted as a gate; missing or `false` keeps business processing off.

### 1.2 AI consent

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `consent:ai:{chat_id}` | EX 3600 / 86_400 / 604_800 / 31_536_000 | `set_ai_consent` | `ai_consent_until`; cleared by `clear_ai_consent` |

The stored value is an **absolute Unix expiry timestamp**, not a duration; the key's `EX`
carries the same duration, so the key removes itself.

Trigger words are exact Chinese literals with **no English aliases** — the in-app help text
enforces this itself (in-app help text in `worker.rs`):

| Input | TTL | Label |
|---|---|---|
| `/ai on`, `/ai yes`, `开启 ai`, `同意摘要`, `允许 ai` | 3600 | `1小时` |
| `临时`, `一次` (substring) | 3600 | `临时1小时` |
| `今天` | 86_400 | `今天` |
| `7天` | 604_800 | `7天` |
| `直到我撤销`, `长期` (substring) | 31_536_000 (365 d) | `直到撤销（最长365天）` |
| `/ai off`, `关闭 ai`, `撤销授权`, `停止摘要` | — | revoke |

The `/ai …` slash forms are exact matches on the whole message; the Chinese phrases are
substring matches anywhere in the message (both inside `parse_intent`, `worker.rs`).

`1小时` / `临时1小时` / `直到撤销（最长365天）` are **category labels, not trigger words**:
typing a label alone grants nothing. Consent never auto-renews.

The same `parse_intent` (`worker.rs`, variants in `Intent`) routes `/search <关键词>`
and the Chinese prefixes `搜索`, `查找`, `检索` (prefix-matched only, never a substring) to
`Intent::Search`. Search grants no consent and writes no Redis key.

### 1.3 Delivery pipeline

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `stalwart:jmap`, `stalwart:telegram` | Stream, no EX | `State::enqueue` (a `XADD`), called from `telegram_webhook` and `jmap_push`; reconcile path via `claim_dedup_and_enqueue` (`worker.rs`) | `read_batch` over consumer group `stalwart-workers`, consumer `http-worker` |
| `delivery:inflight:{stream}:{id}` | EX 60 | key built and claimed with `claim_dedup` in the delivery path | `release_dedup` on completion |
| `delivery:committed:{stream}:{id}` | EX 604_800 | key built and claimed with `claim_dedup` in the delivery path | `dedup_exists` before send |
| `retry:{stream}:{id}` | EX 86_400 | `retry_or_dlq`'s Lua script (`INCR` then `EXPIRE` on the same key) | reclaim path |
| `stalwart:jmap:dlq`, `stalwart:telegram:dlq` | Stream, no EX | the same Lua script's `XADD`; the name is passed in from `retry_or_dlq`'s call site in the worker (with `max_attempts` 3) | **nothing in code reads it** |
| `state:jmap:since` | none (durable) | `set_reconcile_state` | `get_reconcile_state` before each pass; encoding per §6.5 |

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
append and source acknowledgement are atomic, all inside that one Lua script. No code path reads the DLQ
back — replay is an operator action, not a service feature.

### 1.4 Idempotency and rate limits

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `dedup:tg:{update_id}` | EX 86_400 | built and claimed with `claim_dedup` in `telegram_webhook`, released with `release_dedup` on a failed enqueue | — |
| `dedup:jmap:{account}:{email}` | EX 86_400 | built and claimed with `claim_dedup` in `jmap_push`; the same key is rebuilt on the reconcile path and claimed with `claim_dedup_and_enqueue` (`worker.rs`) | — |
| `ratelimit:push-verify:{subscription}` | EX 30 | built and claimed with `claim_dedup` in `register_push`; a `false` claim is the `429 push_verify_rate_limited` branch | — |
| `state:jmap:since` | **No EX** | `set_reconcile_state` (a `SET` with no `EX`) | `get_reconcile_state` |

`state:jmap:since` is the only state key that survives indefinitely without an explicit
delete, by design: it is the reconciliation cursor, and expiring it would force a full
re-baseline on the next restart.

### 1.5 Locks

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `lock:reconcile` | 300 | `acquire_lock` in `reconcile` | `renew_lock` from the heartbeat task (90 s) |
| `lock:push-register:{sha256(callback_url)}` | 360 | `acquire_lock` in `register_push` | `release_lock` on every exit path of `register_push` |

**Why the registration lock is 360 s and not 300 s.** The lock must outlive the longest
configured JMAP request timeout (300 s), otherwise a slow registration could admit a
duplicate. Source comment:

> The lock must outlive the configured 300s maximum JMAP request timeout; this prevents a
> slow create from admitting a duplicate.

### 1.6 Push subscription state

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `push:subscription:{id}` | EX 300 (caller-supplied, min 1) | `remember_push_subscription`, called from `register_push` | `push_subscription_verified` |
| `push:subscription:id` | No EX | `remember_push_subscription_id` | current-subscription lookup |
| `push:subscription:{id}:status` | 900 / 300 / 86_400 (min 1 enforced) | `set_push_subscription_status`; `pending` on verification request, `verified` on success, `disabled` on disable | **none** |
| `push:registration:{sha256(callback_url)}` | EX 604_800 | `remember_push_subscription_for_callback`, called from `register_push` | `get_push_subscription_for_callback`; removed by `remove_push_subscription_for_callback` |
| `push:orphan:{subscription_id}` | EX 604_800 | `record_push_orphan` | orphan sweep |

**Two distinct keys that are frequently confused. Read this before editing either.**

- `push:registration:{sha256(callback_url)}` is a **mapping from callback URL to
  subscription id**, TTL seven days. It is not a lock.
- `lock:push-register:{sha256(callback_url)}` is the **single-flight registration lock**,
  TTL 360 s. It is not a mapping.

They use the same digest but are separate keys with unrelated lifetimes and purposes.

**Annotation: `push:subscription:{id}:status` is write-only by design.**
The source carries an explicit note on `state.rs`:

> `push:subscription:{id}:status` is currently write-only

It is an ops / observability trail intended for `redis-cli` inspection; it is **not** a gate
and nothing reads it for authorization. The three values ever written are `pending` (900 s),
`verified` (300 s) and `disabled` (86400 s) — see the table above for their call sites.
Do not infer correctness from its presence.

---

## 2. Error envelope

Every error response uses one envelope shape:

```json
{ "error": "<machine_code>", "request_id": "<id>" }
```

`error` is a **plain machine-readable string**, not a nested object — there is no `message`
field. `request_id` is always present.

| Status | Code | Retry-After |
|---|---|---|
| 400 | `invalid_request`, `missing` | — |
| 401 | `unauthorized` | — |
| 403 | `forbidden`, `chat_not_allowed` (`/debug/*` only) | — |
| 404 | `push_subscription_not_found` | — |
| 409 | `conflict` | — |
| 422 | `invalid_configuration` | — |
| 429 | `push_verify_rate_limited` | `30` |
| 500 | `internal_error` | — |
| 502 | `telegram_send_failed` (`/debug/*` only) | — |
| 503 | `service_unavailable`, `disabled`, `reconcile_retry`, `push_verify_failed`, `push_state_unavailable`, `push_destroy_failed` | `30` when retryable, or always for the push register/disable failures |

`Retry-After` has two emission sites, both in `notify.rs`, and the value is always the literal
string `"30"`:

- `error_response` sets it **only when its `retry` flag is true**. The readiness failure
  path passes `retry = true`, so `/ready` is covered by that rule rather than by a
  special case.
- `error_response_with_id` sets it **unconditionally**. It is used only for the push
  register/disable failures `push_state_unavailable` and `push_destroy_failed`, which are
  always `503`.

`GET /api/status` is the exception that reports setup state with a non-envelope body.

---

## 3. Backend routes

All HTTP API routes are registered in `router_with_worker_state_runtime_bootstrap`.
The three static SPA routes in the last table row are the exception: they live in
`web::router()` (`src/web.rs`) and are merged into the router via `.merge(web::router())` —
in the setup-mode router and again in the production router. The setup-mode router mounts
only three of this table — `/healthz`, `/ready` and `/api/status` — and no business or
admin route; §5.1 explains why that is deliberate.

| Method | Path | Handler |
|---|---|---|
| POST | `/webhook/tg` | `telegram_webhook` |
| POST | `/push/jmap` | `jmap_push` |
| POST | `/api/push/register` | `register_push` |
| POST | `/api/telegram/register-webhook` | `register_telegram_webhook` |
| POST | `/api/push/disable` | `disable_push` |
| POST | `/reconcile` | `reconcile` |
| POST | `/worker` | `worker` |
| GET, PUT | `/api/config` | `get_config` / `put_config` |
| GET, PUT | `/api/enabled` | `get_enabled` / `put_enabled` |
| GET, PUT | `/api/business-config` | `get_business_config` / `put_business_config` |
| POST | `/api/business-config/preflight` | `preflight_business_config` |
| POST | `/api/bootstrap` | `bootstrap` |
| POST | `/api/admin/session/revoke` | `revoke_admin_session` |
| POST | `/api/admin/session` | `create_admin_session` |
| GET | `/healthz` | `healthz` |
| GET | `/ready` | `ready` |
| GET | `/api/status` | `setup_status` |
| GET | `/`, `/assets/config.js`, `/assets/styles.css` | `index` / `script` / `styles` |

The remote-debug surface (`debug_router`, `src/debug.rs`) is merged **only when the
debug surface is requested — `--debug` on the command line or a truthy `DEBUG_ENABLED`
env var — and `DEBUG_TOKEN` is non-empty** (`SAF-DEBUG-GATE`; the gate is read in
`src/main.rs` before either router is built). Otherwise none of these routes exist and
requests fall through to axum's generic `404`, not a 401. All seven require
`Authorization: Bearer <DEBUG_TOKEN>`, checked by `debug_authorized`, which delegates to
the production `worker_authorized` so the comparison is constant time.

| Method | Path | Handler |
|---|---|---|
| GET | `/debug/ping` | `debug_ping` |
| GET | `/debug/config` | `debug_config` |
| GET | `/debug/redis` | `debug_redis` |
| GET | `/debug/jmap` | `debug_jmap` |
| GET | `/debug/telegram` | `debug_telegram` |
| GET | `/debug/worker` | `debug_worker` |
| POST | `/debug/notify` | `debug_notify` |

Response conventions:

- `GET /healthz` returns **200 unconditionally**. It is a liveness probe and must not be
  used to decide whether to route traffic.
- `GET /ready` returns **503** when configuration, Redis, or an upstream probe is not ready,
  and carries `Retry-After: 30` for not-ready responses. It checks four things: config
  completeness (`setup_missing` empty), Redis reachability, and two upstream probes —
  `GET {jmap_origin}/.well-known/jmap` with the configured Basic credentials, and
  `GET https://api.telegram.org/bot<token>/getMe`. Both probes share `PROBE_TIMEOUT` = 3000ms
  and run **in parallel** (`tokio::join!`), so the worst case
  is a single timeout, about 3s. On success the body is a report whose `jmap`/`telegram` fields
  are real probe results. The probes are plain reusable functions (`probe_jmap_session`,
  `probe_telegram_get_me`) also used by the remote-debug path, so they must not be duplicated.
  The JMAP probe deliberately authenticates against the *normalized origin*: probing the raw
  session URL unauthenticated would report not-ready forever and make ingress stop routing.
- `GET /api/status` (**always 200**) returns
  `{"ready": <bool>, "mode": "configured"|"configuration-setup", "missing": [...], "version": "<build-fingerprint>"}`.
  `ready` is `false` and `missing` lists the absent required keys (`REDIS_URL`,
  `CONFIG_ENCRYPTION_KEY`) when a variable is absent — the route itself never errors, so it is
  safe to poll. `version` is the `BUILD_VERSION` string baked in by `build.rs`
  (`<git-sha-or-nogit>+<UTC build time>`) and is how the SPA footer proves a deploy landed.
- `/debug/*` returns **503** in exactly one place: `POST /debug/notify` when the business
  config is not loaded or there is no outbound client — `service_unavailable` with
  `Retry-After: 30`. The three probe endpoints instead report failure
  **inside a 200 body** (`{"ok":false,"detail":...}`), so an unreachable upstream never looks
  like an outage. `POST /debug/notify` also returns `403 chat_not_allowed` when the chat is
  outside a *non-empty* allowlist, and `502 telegram_send_failed` on
  a send failure.
- Static assets are served with a strict CSP; see §7.

---

## 4. Gateway vs backend route matrix

> **Independently verified.** Source: `cloudflare-worker/src/backends.js` `SAFE_ROUTES`,
> 18 entries, alongside `ROUTE_METHODS` (`index.js`)
> which fixes one method set per path. The worker entry point is `src/index.js`
> (`wrangler.toml`); `src/lb.js` performs forwarding and bounded failover (`SAF-LB-PASSTHRU`,
> `C-NO-LONG-CONN`).

The gate is **unconditional and fail-closed**. The worker reads no configuration switches at
all: a path missing from `SAFE_ROUTES` returns **404**, a registered
path with the wrong method returns **405**, and a missing or unparseable
backend pool returns **503** rather than passing the request through.

**Forwarded by the worker (19):**

`/` · `/assets/config.js` · `/assets/styles.css` · `/api/status` · `/api/config` ·
`/api/business-config` · `/api/business-config/preflight` · `/api/admin/session` ·
`/api/admin/session/revoke` ·
`/api/enabled` ·
`/webhook/tg` · `/push/jmap` · `/api/push/register` · `/api/telegram/register-webhook` · `/api/push/disable` · `/reconcile` ·
`/worker` · `/ready` · `/healthz`

**Registered on the backend but NOT forwarded (2):**

| Path | Why it is absent from the gateway |
|---|---|
| `POST /api/bootstrap` | One-shot trust bootstrap; kept off the public path |
| `/debug/*` (7 routes) | Opt-in remote-debug surface (`SAF-DEBUG-GATE`); absent from `SAFE_ROUTES`, so it is reachable **only** by talking to the backend origin directly |

`GET, PUT /api/enabled` (the `SAF-ENABLE-FLAG` kill switch) **is** forwarded, because the
admin SPA serves it at the Worker URL and toggles it from the service card (`loadEnabled`
reads it, the toggle writes it); both calls are
admin-session Bearer-auth'd, so the exposure is identical to the already-forwarded
`/api/admin/session` pair.

Consequence: neither remaining route carries external business traffic, so no second ingress is
needed in front of the backend instances. The SPA's first-boot flow still cannot drive
`/api/bootstrap` through the worker — bootstrap must be performed against the backend origin
directly, or the bootstrap path must be added to the gateway allowlist.

`POST /api/push/register`, `POST /api/telegram/register-webhook` and `POST /api/push/disable` **are** forwarded. These are safe to
proxy: the callback URL is supplied by the client in the request body and
validated as a URL before anything is written, and every push subscription record is
written to and read back from the shared Redis (`lock:push-register:{sha256(url)}`, `get_push_subscription_for_callback`),
so it does not matter which backend instance the worker picks. Push
registration therefore no longer requires hitting a specific backend address. After an operator saves business configuration, the SPA submits both callback URLs through these protected endpoints.

---

## 5. Environment variables

Three distinct layers. They are not interchangeable.

### 5.1 Boot-time (2 required, 2 optional with defaults)

| Variable | Required | Default | Source |
|---|---|---|---|
| `REDIS_URL` | yes | — | read at startup in `src/main.rs` |
| `CONFIG_ENCRYPTION_KEY` | yes | — | `encryption_key_from_env` |
| `PORT` | no | `8080` | `unwrap_or(8080_u16)`. The listener binds it directly; `Config` carries no port field |
| `DEBUG_ENABLED` | no | — | truthy values are `1`/`true`/`TRUE`/`True`/`yes`/`YES` (exact match); enables the debug surface on its own, paired with `--debug` as an alternative (`SAF-DEBUG-GATE`) |
| `DEBUG_TOKEN` | no | — | effective only once the debug surface is requested via `DEBUG_ENABLED` or `--debug` (`SAF-DEBUG-GATE` / `SAF-DEBUG-ORIGIN-ONLY`) |

`RUN_MODE` **no longer exists**: the identifier was removed together with the legacy
`Config::from_env()` environment parser (registration in `docs/retired.md`). Both webhook
and reconcile traffic share one router and reconcile is a standalone `POST /reconcile`
endpoint, so the variable never changed any runtime behaviour — there is now nothing to set.

**Missing a required variable does not crash the process.** It logs a warning and serves
`router_configuration_setup`, which mounts only `/api/status`,
`/ready` and `/healthz` on top of the static SPA. The business and admin routes from §3 are not
registered at all: `admin_token` is empty, so `constant_time_eq` would reject
every candidate forever, and `MemoryState` could not persist a bootstrap
write anyway. Posting to `/api/bootstrap` in this mode yields `404` (route absent), not a `401`
that reads as "retry with a better credential". The container stays up and answers the status
surface while doing no business work. This is a deliberate fail-closed-to-setup posture, and it
is the single most likely cause of "the container is healthy but nothing happens".

### 5.2 Redis-resident business configuration

Read by the SPA through `GET /api/business-config` and written through
`PUT /api/business-config` (effective within 1 s); the first successful write creates the
configuration, later writes hot-reload it. `GET` returns four keys — `configured`, `revision`,
`values`, `secrets_present` — holding the 10 non-secret fields plus one presence boolean per
secret (`bot_token`, `jmap_password`, `telegram_webhook_secret`, `reconcile_token`,
`worker_token`, `llm_api_key`), never the secret values themselves (`SAF-NO-SECRET-ECHO`). With
nothing saved yet it returns `200`, `configured: false`, `revision: 0`, an empty `values` and
every flag false, so the SPA needs no special-case code path. The PUT body is a **partial
patch** that replaces only the fields it names and keeps the stored value for the rest (`apply`
in `config.rs`); a secret omitted from the patch keeps the stored secret, so prefilled form
values are safe to resubmit. An explicitly submitted empty string is stored as-is and really
clears the secret — "blank means unchanged" is a client-side contract, enforced by the SPA
dropping blank secret fields before it sends, not by the server.

The increment semantics only hold once a configuration exists. With nothing stored there is no
value to fall back to, so an incomplete patch is rejected with **422 `invalid_configuration`**
— the first save has to be complete. `POST /api/bootstrap` against the backend origin is the
one-shot write path that creates a configuration from scratch, for automation. The process never
reads these from the environment in normal operation.

**Persist-and-report, not persist-if-connectable.** Validation is the only gate on the write,
and on a `PUT` it runs against the **merged** configuration, not the submitted body: the patch
is never validated on its own, and the full stored wire is never discarded (`apply` in
`config.rs`). `validate_business_wire` runs first and a rejection is a genuine `422`. Once
the merged wire is valid it is *always* persisted — the merged wire, not the request body — and
only then are the clients built and the running
worker swapped. A failed build therefore returns **200** with `persisted: true`,
`runtime_applied: false` and a `warnings` array of `{component, step, detail}` objects, where
`component` is `jmap` or `llm` and `step` is `connect`, `account`, `config` or `build`, rather
than a 503. The configuration is saved and the outage is reported instead of hidden.
`runtime_applied` is
`true` only when the reload was committed. This is deliberate: an unreachable JMAP must not
turn a valid configuration into a silent data loss. `PUT /api/business-config` and
`POST /api/bootstrap` both follow this contract; the revision is returned in both cases as the
`x-business-config-revision` header, and only `PUT /api/business-config` also returns it in the
response body, as `"revision"` (a `u64`). The SPA surfaces it in its status line and sends it
back in the request body as the `revision` control field, which the backend compares against the
stored revision and answers `409 conflict` on a mismatch. Bootstrap's
body carries no `revision` field, and a `PUT` body that omits it keeps last-write-wins.

`POST /api/business-config/preflight` (`preflight_business_config`) runs the identical
validation and client build against the submitted wire and returns the per-component verdict
without writing anything and without touching the running worker:

```json
{
  "persisted": false,
  "validation": { "ok": true, "errors": [] },
  "components": {
    "jmap": { "ok": false, "errors": [{ "component": "jmap", "step": "connect", "detail": "..." }] },
    "llm": null
  }
}
```

`components` is `null` when validation already failed (there is no point probing clients from a
wire that was rejected), and `llm` is `null` when `llm_enabled`/`llm_allow_net` are false
rather than vacuously `ok`. A preflight whose answer is "the configuration is bad" is a
**200**, because that is the answer the caller asked for. Authentication is the same
`config_authorized` gate as the write path; it is deliberately **not** behind `DEBUG_TOKEN`.

### 5.3 No legacy environment path

`Config::from_env()` and its helpers (`required_secret`, `required_nonblank`, `env_bool`) are
**deleted** — `config.rs` performs no `std::env` read at all. The names below were the legacy
env surface and are listed only so a stale deployment script can be recognised as stale
(registration in `docs/retired.md`):

`RUN_MODE` · `CHAT_ALLOWLIST` · `TELEGRAM_CHAT_ID` · `LLM_ENABLED` · `LLM_ALLOW_NET` ·
`LLM_API_KEY` · `LLM_BASE_URL` · `LLM_MODEL` · `LLM_SUMMARY_TARGET_CHARS` · `BOT_TOKEN` ·
`JMAP_SESSION_URL` · `JMAP_USERNAME` · `JMAP_PASSWORD` · `ACCOUNT_ID` · `RECONCILE_TOKEN` ·
`TG_WEBHOOK_SECRET` · `WORKER_TOKEN`

The `JMAP_*` family is exactly three names. In production the account id arrives as the
`accountId` request-body field on `jmap_push` and is held on the client as
`account_id`; the only remaining environment reads of these
four names are inside the `#[ignore]` real-server smoke test in
`src/domain/jmap/client.rs`, which is not part of the production configuration surface.

The single prefixed name is `TELEGRAM_CHAT_ID`. Anything else documented as
`TELEGRAM_BOT_TOKEN` or similar is a documentation error, not a supported variable.

**Trust root.** The SPA admin credential is `CONFIG_ENCRYPTION_KEY` itself: read at startup
and held as `admin_token`, it is checked in constant time by
`worker_authorized` at the top of both
`POST /api/bootstrap` and `POST /api/admin/session`. It is
only compared against the request bearer — never echoed, logged, or stored. The session
issued by `/api/admin/session` is a freshly generated random 32-byte hex token
(generated per call), never the credential; only its digest is retained in Redis under
`config:admin_session`. `REDIS_URL` is the Redis connection string alone: an ACL password in
it, if any, authenticates the Redis connection and is not the credential for any HTTP
endpoint.

### 5.4 Notification rendering

`send_notification` (`src/channel.rs`) is the only function that renders a notification into
Telegram text, and it renders exactly three metadata lines — never the body, and never an
LLM summary:

```text
From: Zhang San <zhang@example.com>
Subject: Q4 budget
Received: 2026-09-28 09:15
```

`From:` is the JMAP `EmailAddress` in standard display form: `Name <address>` when the
sender carries a display name, the bare address otherwise. Both `From:` and `Subject:`
fall back to a literal (`unknown` / `(no subject)`) only when JMAP did not return the
property.

`Received:` is rendered at notification time, not stored as text. The server keeps the
JMAP `receivedAt` as a Unix second and converts it to wall-clock time in the configured
timezone using `%Y-%m-%d %H:%M` — 24-hour clock, minute precision, no AM/PM, no seconds.
When `receivedAt` is absent, or falls outside the representable instant range for the zone,
the literal `unknown` is shown rather than a clipped timestamp.

The timezone is a business-configuration field, `timezone` (`REQ-TIMEZONE-DISPLAY`): an
IANA identifier, default `Asia/Shanghai`. Only 16 zones are accepted, every one of them
without daylight saving transitions — `Etc/UTC`, `Africa/Cairo`, `Europe/Istanbul`,
`Africa/Nairobi`, `Asia/Dubai`, `Asia/Karachi`, `Asia/Kolkata`, `Asia/Bangkok`,
`Asia/Ho_Chi_Minh`, `Asia/Shanghai`, `Asia/Hong_Kong`, `Asia/Taipei`, `Asia/Singapore`,
`Asia/Manila`, `Asia/Tokyo`, `Asia/Seoul`. Any other identifier is rejected with **422**
rather than silently falling back to a guess. This is a deliberate limitation: the offline
build has no IANA tz database (`chrono-tz` is unavailable), so each zone resolves to a
fixed offset and never tracks a transition. None of the sixteen needs one, which is why
`Asia/Shanghai` can be the default. Like the rest of business configuration it is edited
in the SPA and applied hot, with no restart.

---

## 6. Outbound and runtime parameters

### 6.1 Outbound

| Parameter | Default | Range / cap | Source |
|---|---|---|---|
| `jmap_timeout_ms` | 15_000 | 100..=300_000 | default in `state.rs`; validated in the write path |
| `telegram_timeout_ms` | 10_000 | 100..=300_000 | default in `state.rs`; validated in the write path |
| `llm_timeout_ms` | 30_000 | 100..=300_000 | default in `state.rs`; validated in the write path |
| `max_retries` | 3 | hard cap 5 | default in `state.rs`; rejected in the write path; re-clamped in `channel.rs` |

There is **no** `LLM_MAX_RETRIES` environment variable; retry count lives in
`config:outbound` and is bounded at 5 regardless of what is written.

### 6.2 Reconcile budgets

| Constant | Value | Source |
|---|---|---|
| `BASELINE_PAGE_SIZE` | 100 | `worker.rs` |
| `BASELINE_MAX_PAGES` | 100 | `worker.rs` |
| `BASELINE_MAX_EMAILS` | 10_000 | `worker.rs` |
| `RECONCILE_MAX_PAGES` | 100 | `worker.rs` |
| `RECONCILE_BUDGET` | 20 s | `worker.rs` |
| `CHANGE_WINDOW_CAP` | 4_096 | `worker.rs` |
| `RECONCILE_INITIAL_CHANGES` | 100 | `notify.rs` |
| Lock TTL / heartbeat | 300 s / 90 s | `reconcile` and its heartbeat task |

`initial_changes` is a function parameter, not a constant: it sizes only the
first `/changes` call. The replay phase may widen the window afterwards, up to
`CHANGE_WINDOW_CAP`, before it advances the cursor.

### 6.3 Idle threshold

XAUTOCLAIM's idle threshold is derived from the live outbound config:

```
count × (max_retries + 1) × (jmap_timeout_ms + telegram_timeout_ms + llm_timeout_ms) × 2
```

Floored at the legacy 300 s single-event ceiling and capped at 6 h. The floor is also the
fallback when the config cannot be read, so a
Redis hiccup never collapses the window. Only duplicate-on-multi-instance is at
stake, never loss.

### 6.4 Search limits

| Constant | Value | Source |
|---|---|---|
| `SEARCH_LIMIT` | 10 | `worker.rs` |
| `SEARCH_SUBJECT_MAX` | 120 chars | `worker.rs` |
| `SEARCH_PREVIEW_MAX` | 160 chars | `worker.rs` |

### 6.5 Reconcile cursor

`/reconcile` keeps one cursor, `state:jmap:since`, and it is bimodal:

- `baseline:{hex-encoded-state}:{position}` — a position walk is in flight. The
  walk enumerates the collection by position (`list_emails_page`) and `/changes`
  has not been replayed from it yet.
- A bare state string — the walk is done; this is a token to resume
  `Email/changes` from.

The two modes are an explicit `ReconcileCursor` variant in `worker.rs`, not a
reserved position value. The page cap (`BASELINE_MAX_PAGES`) is a hard stop on
the walk: if the walk is cut there, the cursor is persisted **as a walk**, never
as a replay, because replay mode asserts the listing is exhausted and replaying
`/changes` from a half-listed state would skip whatever still remains.

Two token classes are in play and they are not interchangeable:

- `Session.state` (RFC 8620 §2.1) is minted per session and is **not** an
  `Email`-collection token. A fresh or re-baselined cursor carries it, because
  the jmap-client `Session` type exposes no per-collection state.
- `newState` out of a `Changes` response **is** a collection token.

A strictly conforming server may therefore reject the very first `/changes` call
after a re-baseline with a `sinceState` error. `replay_changes` absorbs it by
re-baselining to a fresh token and returning, so the mismatch can only ever bite
one call and the next pass restarts the walk; the 24h dedup key
(`ttl::DEDUP_JMAP_SECONDS`) bounds the resulting replay to at most one duplicate.

`Email/changes` also returns `destroyed` and `oldState`. Both are dropped by
`EmailChanges`: a deleted email has nothing left to look up, so it drives no
notification and does not move the cursor, and `oldState` echoes the
`sinceState` the server accepted, which the caller already holds. Keeping either
would have meant storing a value nothing reads.

---

## 7. Static assets and response headers

Served from `src/web.rs`.

- Three routes only: `GET /`, `GET /assets/config.js`, `GET /assets/styles.css`.
  Everything else returns 404.
- Assets are embedded at compile time with `include_str!`. There is no
  filesystem access at runtime.
- Every response carries `Content-Type`, `Cache-Control: no-store`,
  `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
- CSP is applied **only to the HTML document**: inserted inside
  an `if html` block:

  ```
  default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self';
  form-action 'self'; base-uri 'none'; frame-ancestors 'none'
  ```

- `X-Frame-Options: DENY` is set alongside the CSP, on the HTML response only.

`default-src 'none'` is deliberately stricter than `'self'`: the page is closed by default
and only `script-src`, `style-src` and `connect-src` re-open a same-origin channel.

---

## 8. Not implemented

Recorded here so that references elsewhere cannot be mistaken for shipped features.

- `/search` **is** implemented (`bfe0fd8`). Adapter: `Intent::Search` in `worker.rs` and
  in `src/domain/jmap.rs`, backed by `search_emails` in `src/domain/jmap/client.rs`.
  What is *not* possible: **body-level** snippets. jmap-client `0.4.2` only exposes
  `emailId`/`subject`/`preview` from `SearchSnippet/get`, and its `Filter` type has no comparator syntax, so
  per-part body highlight cannot be modelled through the locked crate. Search degrades to
  `subject`/`preview` highlights plus a pure-ID list when snippets are unsupported. Design
  note in `docs/design.md` §5.7.
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

## 9. Deployment platform details

The operator-facing sequence is in [`deployment.md`](deployment.md). This section keeps
platform-specific behavior and settings that are useful when configuring or diagnosing a
deployment.

### 9.1 Backend deployment paths

**HostStack production path.** The repository's `hoststack.yaml` is the production
configuration for HostStack's native Rust runtime. It runs `cargo fetch --locked`, builds
with `cargo build --release --locked`, starts `./target/release/message-weave`, and uses
`/healthz` for liveness. Set `REDIS_URL` and `CONFIG_ENCRYPTION_KEY` as encrypted service
secrets. The service commands declared in YAML override stored dashboard commands; removing
the file can restore HostStack's default `./target/release/app`, which is not this package's
binary. Keep the YAML checked in unless all dashboard commands have been corrected and a
replacement deployment has been verified.

The HostStack path does not execute the repository Dockerfile. Its builder and runner are
managed by the platform. The Dockerfile is for local container runs and Docker-based hosts.

**Docker image path.** The root Dockerfile has two stages:

1. `rust:1-slim-bookworm` builds the release binary with the locked Cargo dependencies.
2. `debian:bookworm-slim` receives that binary, CA certificates, `tini`, and read-only
   diagnostic tools (`curl`, `procps`, `iproute2`, `jq`, and `netcat-openbsd`). It creates
   and runs as the `messageweave` system user, sets `PORT=8080`, exposes only that port, and
   starts the service through `tini`.

The image has no Redis process, database, or persistent data volume. Runtime configuration is
injected by the host; secrets do not enter build arguments or image layers. Docker-platform
health checks should call the HTTP endpoints described in [`deployment.md`](deployment.md).

### 9.2 Backend configuration and secrets

The process reads only `REDIS_URL` and `CONFIG_ENCRYPTION_KEY` at startup. The optional
remote-debug surface uses the independent `DEBUG_ENABLED` switch and `DEBUG_TOKEN`; leave
both unset in production unless remote diagnostics are deliberately enabled. Full startup
variable semantics and Redis business configuration fields are in [§5](#5-environment-variables).

Telegram, JMAP, allowlist, worker, reconcile, and LLM business settings are Redis-resident,
not process environment variables. The field list and validation rules in [§5.2](#52-redis-resident-business-configuration)
are authoritative. A successful initial save can use the one-shot `/api/bootstrap` endpoint
on the backend origin; that route is not exposed through the Worker. Subsequent edits use the
protected configuration API.

### 9.3 Callback registration and scheduled work

Telegram's `setWebhook` request must use the configured webhook secret and include `message`
in `allowed_updates`. Check the resulting URL with `getWebhookInfo`; Telegram does not return
the secret in that response. Re-run `setWebhook` after rotating the secret.

Register Stalwart push with `POST /api/push/register` and an HTTPS `callback_url`. The backend
creates the subscription and completes Stalwart's verification callback. Repeating the same
callback URL is idempotent. Use `POST /api/push/disable` with that URL to remove it.

The backend has no internal scheduler. [`scripts/cron-drain.sh`](../scripts/cron-drain.sh)
calls `/reconcile` (when `MW_RECONCILE_TOKEN` is set) and then `/worker`. Configure
`MW_APP_URL`, `MW_WORKER_TOKEN`, and optionally `MW_RECONCILE_TOKEN` in the external
scheduler's secret store. `MW_WORKER_URL` can override the default when the operator needs
to call a backend origin directly. `--once` is for cron; `--loop` is for a managed process.
The script also provides `--diagnose` and `--test-notify` for troubleshooting. Both successful
drain endpoints return `204` with an empty response body; `/reconcile` can return `409` while
another reconciliation owns the lock.

### 9.4 Health and release checks

- `GET /healthz` is backend liveness. It does not prove Redis or upstream services are ready.
- `GET /ready` checks configuration, Redis, the JMAP session, and Telegram's `getMe` endpoint.
  It returns `503` with `Retry-After` when a dependency is unavailable. The upstream checks
  need outbound HTTPS access from the backend.
- `GET /api/status` reports boot readiness and missing required boot-variable names.
- `GET /healthz-worker` is generated by the Worker. Check both `available >= 1` and `version`;
  HTTP 200 alone can mean `no-backends` when the origin configuration is missing.

For the backend code gate, use the locked commands and container environment in
[`charter.md`](charter.md). The documentation gate must run on the host so its
path audit can read Git history.

### 9.5 Cloudflare Worker and Dashboard

The Worker is an optional HTTPS gateway. The backend origins must be HTTPS strings, with no
path, query, fragment, or embedded credentials. The current public origin list and
`LB_VERSION` are declared in `cloudflare-worker/wrangler.toml` under `[vars]`. Move
`BACKEND_ORIGINS_JSON` to an encrypted secret only when its values are private; never store
credentials in that list. The Worker itself does not hold Telegram, JMAP, Redis, or backend
business credentials.

| Setting | Dashboard value | Notes |
|---|---|---|
| Root directory | `cloudflare-worker` | The repository root contains the Rust service, not the Worker configuration. |
| Application name | `messageweave-lb` | Match the `name` in `wrangler.toml`. |
| Build command | Leave empty | Wrangler bundles the Worker during deploy; this field is not the test command. |
| Deploy command | `npx wrangler deploy` | Keep this command in the required deploy field. |
| Preview command | `npx wrangler dev --ip 0.0.0.0 --port 8787` | Use when preview builds are enabled; `wrangler preview` is not a valid command. |

In the Dashboard, add Worker tuning values under **Settings → Variables & Secrets → Add
Variable**. Defaults are `LB_REQUEST_TIMEOUT_MS=10000`, `LB_MAX_ATTEMPTS=2`,
`LB_RECONCILE_TIMEOUT_MS=320000`, `LB_WORKER_TIMEOUT_MS=300000`, and
`LB_HEALTH_TTL_MS=30000`. These are adjustable variables, not secrets. `LB_VERSION` belongs
in `[vars]` so the deployed version stays visible in source control.

If a private origin list must be stored in the Dashboard, use **Encrypt** and add
`BACKEND_ORIGINS_JSON` under **Settings → Variables & Secrets**; remove its `[vars]` entry.
Changes to encrypted variables take effect without a code rebuild. Preview environments only
receive secrets configured for that environment; a preview without the origin list can start
but cannot verify backend forwarding.

For Git integration, push the selected commit to the GitHub repository connected to
Cloudflare after setting the `cloudflare-worker` root directory. The build command can be
empty; run `npm test` separately as the Worker code check. After deploy, request
`/healthz-worker`, confirm `available` is at least one, and check that `version` matches
`LB_VERSION`. `status: no-backends` with HTTP 200 means the Worker has no usable origin list;
`status: down` with HTTP 503 means all configured backends failed their health check.
