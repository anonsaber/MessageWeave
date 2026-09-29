# MessageWeave — Reference

> **This file is the single source of truth for verifiable facts.**
> When any other document disagrees with this one, this one wins.
>
> **Verified against commit `b2dbe7c`.** Line numbers in this file were read from that
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
| `config:business` | No EX | `set_business_config` (state.rs:577, key at state.rs:581); first-time write via `SET NX` in `initialize_business_config` (state.rs:592) | `get_business_config` (state.rs:566) |
| `config:business:revision` | No EX | atomic `INCR` on every config write (state.rs:585); `SET 1 NX` on first-time init (state.rs:605) | `business_config_revision` (state.rs:640) |
| `config:outbound` | No EX | `set_outbound_config` (state.rs:553) | `get_outbound_config` (state.rs:537) |
| `config:enabled` | No EX | `set_enabled` (state.rs:658) | `is_enabled` (state.rs:649) |
| `config:admin_session` | EX 900 | `put_admin_session` (state.rs:615, key at state.rs:618) | `admin_session_valid` (state.rs:627); cleared by `revoke_admin_session` (state.rs:636) |

Notes:

- `config:business:revision` is an **atomic Redis `INCR`**, not a read-modify-write. Every
  request-bearing entry point calls `refresh_business_config` (notify.rs:718), which only
  rebuilds the worker when the remote revision exceeds the cached local value
  (notify.rs:723-725) — that guard is what keeps a stale or malformed snapshot from turning
  into a rebuild loop.
- `config:admin_session` carries no TTL metadata beyond the literal `EX 900` passed at write
  time; the handler reports the same window in its response body as `expires_in: 900`
  (notify.rs:788).
- `config:enabled` is consulted as a gate; missing or `false` keeps business processing off.

### 1.2 AI consent

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `consent:ai:{chat_id}` | EX 3600 / 86_400 / 604_800 / 31_536_000 | `set_ai_consent` (state.rs:505; key at state.rs:507, `EX` at state.rs:512) | `ai_consent_until` (state.rs:519); cleared by `clear_ai_consent` (state.rs:528) |

The stored value is an **absolute Unix expiry timestamp**, not a duration; the key's `EX`
carries the same duration, so the key removes itself (state.rs:509-514).

Trigger words are exact Chinese literals with **no English aliases** — the in-app help text
enforces this itself (`worker.rs:614`):

| Input | TTL | Label |
|---|---|---|
| `/ai on`, `/ai yes`, `开启 ai`, `同意摘要`, `允许 ai` | 3600 | `1小时` |
| `临时`, `一次` (substring) | 3600 | `临时1小时` |
| `今天` | 86_400 | `今天` |
| `7天` | 604_800 | `7天` |
| `直到我撤销`, `长期` (substring) | 31_536_000 (365 d) | `直到撤销（最长365天）` |
| `/ai off`, `关闭 ai`, `撤销授权`, `停止摘要` | — | revoke |

The `/ai …` slash forms are exact matches on the whole message; the Chinese phrases are
substring matches anywhere in the message (`worker.rs:546-566`, `:581-586`).

`1小时` / `临时1小时` / `直到撤销（最长365天）` are **category labels, not trigger words**:
typing a label alone grants nothing. Consent never auto-renews.

The same `parse_intent` (worker.rs:523, `Intent` at worker.rs:505) routes `/search <关键词>`
and the Chinese prefixes `搜索`, `查找`, `检索` (prefix-matched only, never a substring) to
`Intent::Search`. Search grants no consent and writes no Redis key.

### 1.3 Delivery pipeline

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `stalwart:jmap`, `stalwart:telegram` | Stream, no EX | `State::enqueue` (state.rs:283, `XADD` at state.rs:285); call sites notify.rs:204 (TG webhook) and notify.rs:860 (JMAP push); reconcile path via `claim_dedup_and_enqueue` (worker.rs:374) | `read_batch` over consumer group `stalwart-workers`, consumer `http-worker` (notify.rs:361, :428) |
| `delivery:inflight:{stream}:{id}` | EX 60 | key built at notify.rs:396, `claim_dedup` at notify.rs:397 | `release_dedup` on completion (notify.rs:410, 416, 425) |
| `delivery:committed:{stream}:{id}` | EX 604_800 | key built at notify.rs:375, `claim_dedup` at notify.rs:409 | `dedup_exists` before send (notify.rs:376) |
| `retry:{stream}:{id}` | EX 86_400 | `retry_or_dlq` Lua script (key state.rs:481, `INCR` state.rs:485, `EXPIRE` state.rs:486) | reclaim path |
| `stalwart:jmap:dlq`, `stalwart:telegram:dlq` | Stream, no EX | Lua `XADD` at state.rs:488; name built at notify.rs:429 (`max_attempts` 3, same call) | **nothing in code reads it** |

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
append and source acknowledgement are atomic (state.rs:485-489). No code path reads the DLQ
back — replay is an operator action, not a service feature.

### 1.4 Idempotency and rate limits

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `dedup:tg:{update_id}` | EX 86_400 | key at notify.rs:199, `claim_dedup` at notify.rs:200, `release_dedup` at :208 | — |
| `dedup:jmap:{account}:{email}` | EX 86_400 | key at notify.rs:855, `claim_dedup` at notify.rs:856, `release_dedup` at notify.rs:865; same key rebuilt at worker.rs:361 and claimed with `claim_dedup_and_enqueue` at worker.rs:374 | — |
| `ratelimit:push-verify:{subscription}` | EX 30 | key at notify.rs:816, `claim_dedup` at notify.rs:817 (`push_verify_rate_limited` at :819) | — |
| `state:jmap:since` | **No EX** | `set_reconcile_state` (state.rs:676; key at state.rs:679, `SET` without `EX`) | `get_reconcile_state` (state.rs:668) |

`state:jmap:since` is the only state key that survives indefinitely without an explicit
delete, by design: it is the reconciliation cursor, and expiring it would force a full
re-baseline on the next restart.

### 1.5 Locks

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `lock:reconcile` | 300 | `acquire_lock` (notify.rs:240) | `renew_lock` heartbeat 90 (notify.rs:253) |
| `lock:push-register:{sha256(callback_url)}` | 360 | `acquire_lock` (notify.rs:907) | `release_lock` (notify.rs:923, 937, 950, 967, 984, 994) |

**Why the registration lock is 360 s and not 300 s.** The lock must outlive the longest
configured JMAP request timeout (300 s), otherwise a slow registration could admit a
duplicate. Source comment, notify.rs:905-906:

> The lock must outlive the configured 300s maximum JMAP request timeout; this prevents a
> slow create from admitting a duplicate.

### 1.6 Push subscription state

| Key | TTL | Writer | Reader |
|---|---|---|---|
| `push:subscription:{id}` | EX 300 (caller-supplied, min 1) | `remember_push_subscription` (state.rs:686, called at notify.rs:834) | `push_subscription_verified` (state.rs:703) |
| `push:subscription:id` | No EX | `remember_push_subscription_id` (state.rs:716) | current-subscription lookup |
| `push:subscription:{id}:status` | 900 / 300 / 86_400 (min 1 enforced) | `set_push_subscription_status` (state.rs:726, key at state.rs:734); pending at notify.rs:961, verified at notify.rs:846, disabled at notify.rs:1032 | **none** |
| `push:registration:{sha256(callback_url)}` | EX 604_800 | `remember_push_subscription_for_callback` (state.rs:779, key at state.rs:786, `EX` at state.rs:788; called at notify.rs:978) | `get_push_subscription_for_callback` (state.rs:768); removed by `remove_push_subscription_for_callback` (state.rs:795) |
| `push:orphan:{subscription_id}` | EX 604_800 | `record_push_orphan` (state.rs:752) | orphan sweep |

**Two distinct keys that are frequently confused. Read this before editing either.**

- `push:registration:{sha256(callback_url)}` is a **mapping from callback URL to
  subscription id**, TTL seven days. It is not a lock.
- `lock:push-register:{sha256(callback_url)}` is the **single-flight registration lock**,
  TTL 360 s. It is not a mapping.

They use the same digest but are separate keys with unrelated lifetimes and purposes.

**Annotation: `push:subscription:{id}:status` is write-only by design.**
The source carries an explicit note at state.rs:211:

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

- `error_response` (`notify.rs:321-329`, inserted at `:324`) sets it **only when its `retry`
  flag is true**. The readiness failure path (`notify.rs:162`) passes `retry = true`, so `/ready`
  is covered by that rule rather than by a special case.
- `error_response_with_id` (`notify.rs:1088-1094`, inserted at `:1090`) sets it
  **unconditionally**. It is used only for the push register/disable failures
  `push_state_unavailable` and `push_destroy_failed` (`notify.rs:953`, `:969`, `:986`,
  `:1023`), which are always `503`.

`GET /api/status` is the exception that reports setup state with a non-envelope body.

---

## 3. Backend routes

All HTTP API routes are registered in `router_with_worker_state_runtime_bootstrap_config`
(`notify.rs:1295`), with the routes wired at `notify.rs:1323-1338`. The three static SPA routes
in the last table row are the exception: they live in `web::router()` (`src/web.rs:19-24`) and
are merged into the router via `.merge(web::router())`: the setup-mode router at `src/main.rs:76`,
the production router at `src/main.rs:180`. The setup-mode router at `src/main.rs:76` mounts only
three of this table — `/healthz`, `/ready` and `/api/status` — and no business or admin route;
§5.1 explains why that is deliberate.

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
| POST | `/api/business-config/preflight` | `preflight_business_config` |
| POST | `/api/bootstrap` | `bootstrap` |
| POST | `/api/admin/session/revoke` | `revoke_admin_session` |
| POST | `/api/admin/session` | `create_admin_session` |
| GET | `/healthz` | `healthz` |
| GET | `/ready` | `ready` |
| GET | `/api/status` | `setup_status` |
| GET | `/`, `/assets/config.js`, `/assets/styles.css` | `index` / `script` / `styles` |

The remote-debug surface (`src/debug.rs:72-80`) is merged **only when the debug surface is
requested — `--debug` on the command line or a truthy `DEBUG_ENABLED` env var — and `DEBUG_TOKEN`
is non-empty** (`SAF-DEBUG-GATE`, gate logic `src/main.rs:15-25`, wired at `src/main.rs:102-114`,
`src/notify.rs:1340-1342`); otherwise none of these routes exist and requests fall through to
axum's generic `404`, not a 401. All seven require `Authorization: Bearer <DEBUG_TOKEN>`,
checked by `debug_authorized` (`src/debug.rs:45`), which delegates to the production
`worker_authorized` so the comparison is constant time (`src/notify.rs:1136`).

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
  (`notify.rs:74`) and run **in parallel** (`tokio::join!`, `notify.rs:148`), so the worst case
  is a single timeout, about 3s. On success the body is a report whose `jmap`/`telegram` fields
  are real probe results. The probes are plain reusable functions (`probe_jmap_session`,
  `probe_telegram_get_me`) also used by the remote-debug path, so they must not be duplicated.
  The JMAP probe deliberately authenticates against the *normalized origin*: probing the raw
  session URL unauthenticated would report not-ready forever and make ingress stop routing.
- `GET /api/status` (**always 200**, `notify.rs:214-223`) returns
  `{"ready": <bool>, "mode": "configured"|"configuration-setup", "missing": [...], "version": "<build-fingerprint>"}`.
  `ready` is `false` and `missing` lists the absent required keys (`REDIS_URL`,
  `CONFIG_ENCRYPTION_KEY`) when a variable is absent — the route itself never errors, so it is
  safe to poll. `version` is the `BUILD_VERSION` string baked in by `build.rs`
  (`<git-sha-or-nogit>+<UTC build time>`) and is how the SPA footer proves a deploy landed.
- `/debug/*` returns **503** in exactly one place: `POST /debug/notify` when the business
  config is not loaded or there is no outbound client — `service_unavailable` with
  `Retry-After: 30` (`src/debug.rs:56-58`). The three probe endpoints instead report failure
  **inside a 200 body** (`{"ok":false,"detail":...}`), so an unreachable upstream never looks
  like an outage. `POST /debug/notify` also returns `403 chat_not_allowed` when the chat is
  outside a *non-empty* allowlist (`src/debug.rs:232-235`) and `502 telegram_send_failed` on
  a send failure (`src/debug.rs:64-66`).
- Static assets are served with a strict CSP; see §7.

---

## 4. Gateway vs backend route matrix

> **Independently verified.** Source: `cloudflare-worker/src/backends.js` `SAFE_ROUTES`
> (backends.js:9-25), 15 entries, alongside `ROUTE_METHODS` (index.js:45-61)
> which fixes one method set per path. The worker entry point is `src/index.js`
> (wrangler.toml:20); `src/lb.js` performs forwarding and bounded failover (`SAF-LB-PASSTHRU`,
> `C-NO-LONG-CONN`).

The gate is **unconditional and fail-closed**. The worker reads no configuration switches at
all: a path missing from `SAFE_ROUTES` (backends.js:9-25) returns **404** (index.js:81-82), a registered
path with the wrong method returns **405** (index.js:84-86), and a missing or unparseable
backend pool returns **503** rather than passing the request through (index.js:89-96).

**Forwarded by the worker (15):**

`/` · `/assets/config.js` · `/assets/styles.css` · `/api/status` · `/api/config` ·
`/api/business-config` · `/api/admin/session` · `/api/admin/session/revoke` ·
`/api/enabled` ·
`/webhook/tg` · `/push/jmap` · `/api/push/register` · `/api/push/disable` · `/reconcile` ·
`/ready`

**Registered on the backend but NOT forwarded (4):**

| Path | Why it is absent from the gateway |
|---|---|
| `POST /api/bootstrap` | One-shot trust bootstrap; kept off the public path |
| `POST /worker` | Queue drain endpoint — driven by the external scheduler, not a human (Bearer-auth'd, `notify.rs:380`; the cron is spelled out in deployment.md §6.3.1); not part of the public gateway path |
| `GET /healthz` | Liveness is aggregated by the gateway itself |
| `/debug/*` (7 routes) | Opt-in remote-debug surface (`SAF-DEBUG-GATE`); absent from `SAFE_ROUTES`, so it is reachable **only** by talking to the backend origin directly |

`GET, PUT /api/enabled` (the `SAF-ENABLE-FLAG` kill switch) **is** forwarded, because the
admin SPA serves it at the Worker URL and toggles it from the service card (`loadEnabled`
reads it at `web/config.js:586`, the toggle writes it at `:958`); both calls are
admin-session Bearer-auth'd, so the exposure is identical to the already-forwarded
`/api/admin/session` pair.

Consequence: none of the four carries external business traffic, so no second ingress is
needed in front of the backend instances. The SPA's first-boot flow still cannot drive
`/api/bootstrap` through the worker — bootstrap must be performed against the backend origin
directly, or the bootstrap path must be added to the gateway allowlist.

`POST /api/push/register` and `POST /api/push/disable` **are** forwarded. Both are safe to
proxy: the callback URL is supplied by the client in the request body (`notify.rs:875`,
validated as a URL at :891), and every push subscription record is written to and read back
from the shared Redis (`lock:push-register:{sha256(url)}`, `get_push_subscription_for_callback`
at `notify.rs:918`), so it does not matter which backend instance the worker picks. Push
registration therefore no longer requires hitting a specific backend address — see
deployment.md §10.5.

---

## 5. Environment variables

Three distinct layers. They are not interchangeable.

### 5.1 Boot-time (2 required, 2 optional with defaults)

| Variable | Required | Default | Source |
|---|---|---|---|
| `REDIS_URL` | yes | — | read at main.rs:40 |
| `CONFIG_ENCRYPTION_KEY` | yes | — | `encryption_key_from_env` at main.rs:64 |
| `PORT` | no | `8080` | `unwrap_or(8080_u16)` at main.rs:45-48. The listener binds it directly (main.rs:49-52); `Config` carries no port field |
| `DEBUG_ENABLED` | no | — | `std::env::var` at main.rs:104; truthy values are `1`/`true`/`TRUE`/`True`/`yes`/`YES` (exact match); enables the debug surface on its own, paired with `--debug` as an alternative (`SAF-DEBUG-GATE`) |
| `DEBUG_TOKEN` | no | — | `std::env::var` at main.rs:107, effective only once the debug surface is requested via `DEBUG_ENABLED` or `--debug` (`SAF-DEBUG-GATE` / `SAF-DEBUG-ORIGIN-ONLY`) |

`RUN_MODE` **no longer exists**: the identifier was removed together with the legacy
`Config::from_env()` environment parser (registration in `docs/retired.md`). Both webhook
and reconcile traffic share one router and reconcile is a standalone `POST /reconcile`
endpoint, so the variable never changed any runtime behaviour — there is now nothing to set.

**Missing a required variable does not crash the process.** It logs a warning and serves
`router_configuration_setup` (`notify.rs:1200-1223`), which mounts only `/api/status`,
`/ready` and `/healthz` on top of the static SPA. The business and admin routes from §3 are not
registered at all: `admin_token` is empty, so `constant_time_eq` (`notify.rs:1136`) would reject
every candidate forever, and `MemoryState` (`state.rs:875`) could not persist a bootstrap
write anyway. Posting to `/api/bootstrap` in this mode yields `404` (route absent), not a `401`
that reads as "retry with a better credential". The container stays up and answers the status
surface while doing no business work. This is a deliberate fail-closed-to-setup posture, and it
is the single most likely cause of "the container is healthy but nothing happens".

### 5.2 Redis-resident business configuration

Written by the SPA through `PUT /api/business-config` (effective within 1 s); the first
successful write creates the configuration, later writes hot-reload it. The same write is
also exposed as a one-shot `POST /api/bootstrap` against the backend origin for automation.
The process never reads these from the environment in normal operation.

**Persist-and-report, not persist-if-connectable.** Validation is the only gate on the write:
`validate_business_wire` runs first and a rejection is a genuine `422`. Once the wire is valid
the configuration is *always* persisted, and only then are the clients built and the running
worker swapped. A failed build therefore returns **200** with `persisted: true`,
`runtime_applied: false` and a `warnings` array of `{component, step, detail}` objects, where
`component` is `jmap` or `llm` and `step` is `connect`, `account`, `config` or `build`, rather
than a 503. The configuration is saved and the outage is reported instead of hidden.
`runtime_applied` is
`true` only when the reload was committed. This is deliberate: an unreachable JMAP must not
turn a valid configuration into a silent data loss. `PUT /api/business-config` and
`POST /api/bootstrap` both follow this contract; the revision header is still returned in both
cases.

`POST /api/business-config/preflight` (`notify.rs:808-871`) runs the identical validation and
client build against the submitted wire and returns the per-component verdict without writing
anything and without touching the running worker:

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
`accountId` request-body field (notify.rs:1122) and is held on the client as
`account_id` (src/domain/jmap/client.rs:14); the only remaining environment reads of these
four names are inside the `#[ignore]` real-server smoke test at
src/domain/jmap/client.rs:514-526, which is not part of the production configuration surface.

The single prefixed name is `TELEGRAM_CHAT_ID`. Anything else documented as
`TELEGRAM_BOT_TOKEN` or similar is a documentation error, not a supported variable.

**Trust root.** The SPA admin credential is `CONFIG_ENCRYPTION_KEY` itself: read at startup
(main.rs:63) and held as `admin_token` (main.rs:92), it is checked in constant time by
`worker_authorized` (notify.rs:446; compare at notify.rs:1136) at the top of both
`POST /api/bootstrap` (notify.rs:605) and `POST /api/admin/session` (notify.rs:775). It is
only compared against the request bearer — never echoed, logged, or stored. The session
issued by `/api/admin/session` is a freshly generated random 32-byte hex token
(notify.rs:779-788), never the credential; only its digest is retained in Redis under
`config:admin_session`. `REDIS_URL` is the Redis connection string alone: an ACL password in
it, if any, authenticates the Redis connection and is not the credential for any HTTP
endpoint.

---

## 6. Outbound and runtime parameters

### 6.1 Outbound

| Parameter | Default | Range / cap | Source |
|---|---|---|---|
| `jmap_timeout_ms` | 15_000 | 100..=300_000 | default state.rs:56; validated notify.rs:541 |
| `telegram_timeout_ms` | 10_000 | 100..=300_000 | default state.rs:57; validated notify.rs:542 |
| `llm_timeout_ms` | 30_000 | 100..=300_000 | default state.rs:58; validated notify.rs:543 |
| `max_retries` | 3 | hard cap 5 | default state.rs:59; rejected at notify.rs:544; re-clamped at channel.rs:138 |

There is **no** `LLM_MAX_RETRIES` environment variable; retry count lives in
`config:outbound` and is bounded at 5 regardless of what is written.

### 6.2 Reconcile budgets

| Constant | Value | Source |
|---|---|---|
| `MAX_BASELINE_PAGES` | 100 | worker.rs:249 |
| `MAX_BASELINE_EMAILS` | 10_000 | worker.rs:250 |
| `RECONCILE_BUDGET` | 20 s | worker.rs:251 |
| `CHANGE_WINDOW_CAP` | 4_096 | worker.rs:307 |
| Lock TTL / heartbeat | 300 s / 90 s | notify.rs:240 / 252 |

`max_changes` is a function parameter, not a constant.

### 6.3 Idle threshold

XAUTOCLAIM's idle threshold is derived from the live outbound config (state.rs:99):

```
count × (max_retries + 1) × (jmap_timeout_ms + telegram_timeout_ms + llm_timeout_ms) × 2
```

floored at the legacy 300 s single-event ceiling (state.rs:66) and capped at 6 h
(state.rs:76). The floor is also the fallback when the config cannot be read, so a
Redis hiccup never collapses the window. Only duplicate-on-multi-instance is at
stake, never loss.

### 6.4 Search limits

| Constant | Value | Source |
|---|---|---|
| `SEARCH_LIMIT` | 10 | worker.rs:619 |
| `SEARCH_SUBJECT_MAX` | 120 chars | worker.rs:621 |
| `SEARCH_PREVIEW_MAX` | 160 chars | worker.rs:623 |

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

- `/search` **is** implemented (`bfe0fd8`). Adapter: `search_emails` at `worker.rs:652` and
  at `src/domain/jmap.rs:268`, backed by `search_emails` at `src/domain/jmap/client.rs:319`.
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
