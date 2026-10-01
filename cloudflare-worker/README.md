# MessageWeave Cloudflare Worker gateway

> [中文版本 / Chinese version → README.zh-CN.md](README.zh-CN.md)

This is the canonical English version. The Chinese version is
[README.zh-CN.md](README.zh-CN.md).

One public HTTPS entry point in front of one or more MessageWeave backends, with bounded
multi-origin failover (HA/LB sub-project). **Pass-through model**: the Worker does not parse
business payloads and forwards the request as-is to the backend https origins; only *timeout /
5xx* triggers a bounded failover to another instance.

> This component is a **safelist-restricted edge load balancer**: the route set is fixed (19
> whitelisted paths), the backend origins are fixed at deploy time and https-only, and every
> unknown path gets a `404`. It only forwards requests to those fixed backends and fails over
> between them. It does not accept an arbitrary target host and provides no general-purpose
> traffic-relay or access-hiding capability.

> Design basis: `docs/design.md §11.3` (`NFR-HA-MULTI-INSTANCE`) and `docs/deployment.md §10`.
> Security baseline and prohibitions: `docs/charter.md §3`, `docs/charter.md §5`; stable-ID
> registry `docs/charter.md §8`.

## 1. What it is, and what it is not

- **Forward only.** Method, headers (including auth headers) and body are forwarded as-is. The
  Worker performs no authentication of its own and rewrites nothing.
- **Secretless.** No Telegram bot token, no JMAP password, no session secret. It forwards
  whatever auth header it is given and checks none of them. All backends must instead share
  the same set of `SAF-AUTH-*` secrets — `RECONCILE_TOKEN`, `TG_WEBHOOK_SECRET` and the
  encrypted business config — because a callback can land on any instance and no instance can
  prove which one was asked (`C-LB-SHARED-SECRETS`). Per-instance secrets mean a random 401.
- **No origin discovery.** Origins come from the deploy-time origin list only.
- **Zero runtime dependencies.** Pure ES2022 plus the platform `fetch`, `Headers`, `Request`,
  `Response` and `URL`. `wrangler` is a devDependency, used only by `check`, `deploy` and
  `dev`.
- **Zero state.** No Redis, no JMAP, no database (`C-NO-DB`, `C-REDIS-ONLY-STATE`). The only
  mutable state is an in-isolate health probe cache.
- **No long-lived connections** (`C-NO-LONG-CONN`). Pure request-response. The request body is
  read once as `arrayBuffer` and re-attached on every attempt, so streaming is never required.
- **Not a queue.** There is no retry beyond the bounded attempts below. When every origin
  fails the gateway answers `503`, and Telegram / Stalwart redelivery is what makes up for it.

## 2. Directory layout

| file | role |
|---|---|
| `wrangler.toml` | `name`, `main`, compatibility flags, `[vars]` |
| `package.json` | `check` / `test` / `deploy` / `dev` scripts only, no runtime dependencies |
| `src/index.js` | entry point: HTTPS redirect, route dispatch, safelist, health aggregate |
| `src/backends.js` | origin parsing and validation, route safelist |
| `src/lb.js` | pass-through forwarding and bounded failover |
| `src/health.js` | TTL-cached health aggregate probe |
| `test/*.test.js` | 38 tests in four files, all offline |

## 3. Pass-through semantics

- Method, headers and body go out unchanged; the backend response status, body and headers are
  returned as-is.
- The only header the gateway adds is best-effort observability: `x-lb-backend`, naming the
  origin that answered. The write sits behind a `try/catch`. A cross-origin response that
  carries no `access-control-allow-*` header comes back with an immutable header guard, and
  `headers.set` throws on those. An observability write that throws must never mask a healthy
  `2xx` as a `503`, so the header is dropped silently when it cannot be written. That exact
  defect was the 2026-09-30 production incident.
- `GET` and `HEAD` never forward a body: a bodyless upstream response is returned as it is,
  and no `Content-Length` is recomputed.
- Backend `3xx` redirects are followed by the gateway itself (`redirect: "follow"`), so a
  redirecting origin never hands the browser a second hop.
- Logs and error strings carry method, path, origin and failure class only. Never headers,
  body, auth secrets or App Password (`SAF-LOG-PURITY`).

## 4. Route safelist

Every inbound **HTTP** request gets a permanent **308** redirect to the same HTTPS URL before
route validation or forwarding (`C-HTTPS-INBOUND`). After that the request must hit one of 19
paths and carry a method that path allows; every other combination is refused at the edge.

| path | methods |
|---|---|
| `/` | GET |
| `/assets/config.js` | GET |
| `/assets/styles.css` | GET |
| `/api/status` | GET |
| `/api/config` | GET PUT |
| `/api/business-config` | GET PUT |
| `/api/business-config/preflight` | POST |
| `/api/admin/session` | POST |
| `/api/admin/session/revoke` | POST |
| `/api/enabled` | GET PUT |
| `/webhook/tg` | POST |
| `/push/jmap` | POST |
| `/api/push/register` | POST |
| `/api/telegram/register-webhook` | POST |
| `/api/push/disable` | POST |
| `/reconcile` | POST |
| `/worker` | POST |
| `/healthz` | GET |
| `/ready` | GET |

- Unknown path: `404`, body `route not forwarded: <path>`. The gateway can therefore never be
  turned into a jump host for arbitrary backend paths.
- Method mismatch on a known path: `405`, body `method not allowed for <path>`, plus an
  `Allow` header.
- Bootstrap (`POST /api/bootstrap`) and the remote diagnostics face (`/debug/*`) are
  deliberately absent, so both stay origin-only.
- `/healthz-worker` is not in the safelist at all: the gateway serves it itself, on `GET`
  only.

## 5. Timeout and retry budget

| variable | required | default | applies to |
|---|---|---|---|
| `BACKEND_ORIGINS_JSON` | yes | — | all routes |
| `LB_REQUEST_TIMEOUT_MS` | no | `10000` | every route except the two below, and the §7.2 probe |
| `LB_MAX_ATTEMPTS` | no | `2` | attempts per request |
| `LB_RECONCILE_TIMEOUT_MS` | no | `320000` | `POST /reconcile` only |
| `LB_WORKER_TIMEOUT_MS` | no | `300000` | `POST /worker` only |
| `LB_HEALTH_TTL_MS` | no | `30000` | `/healthz-worker` probe cache only |
| `LB_VERSION` | no | `unknown` | `/healthz-worker` `version` field |

Integer env values are parsed with `Math.floor`. A non-finite or non-positive value is ignored
and the default applies, so a typo cannot switch the budget off.

`/reconcile` and `/worker` are **single-attempt routes** with per-route timeout overrides.
Both are long synchronous backend jobs: at the 10 s global timeout the gateway would declare
them failed and fail over to a second instance, but that instance answers `/reconcile` with
`409` immediately (the cluster-wide `lock:reconcile` starts with a 300 s lease, renewed by
heartbeat) and a second `/worker` would merely drain the same batch twice. They therefore get
`LB_RECONCILE_TIMEOUT_MS` (default 320 s, headroom over the 300 s lease plus heartbeats) and
`LB_WORKER_TIMEOUT_MS` (default 300 s, equal to the backend per-event floor
`SINGLE_EVENT_CEILING_FLOOR_MS`), both with `max_attempts = 1`.

`LB_WORKER_TIMEOUT_MS` must grow if the `/worker` batch grows: the backend batch default and
its hard cap are both 10.

## 6. Bounded failover

- Each request tries `max(1, min(LB_MAX_ATTEMPTS, origins.length))` origins. The default of
  2 is one first attempt plus one failover.
- Only a timeout or a `5xx` response moves on to the next origin. `2xx`, `3xx` and `4xx` are
  answers and are returned immediately.
- A timeout or transport failure counts as retryable when the error name is `AbortError`, or
  when the message, or the message of a `cause` further down the chain, matches one of:
  `fetch failed`, `network error`, `socket hang up`, `ECONNREFUSED`, `ECONNRESET`,
  `EAI_AGAIN`, `ENOTFOUND`, `ETIMEDOUT`, `EHOSTUNREACH`, `ENETUNREACH`. Anything else fails
  fast.
- With more than one origin and more than one attempt, the starting origin is chosen with
  `rng` (defaulting to `Math.random`) so load spreads over the healthy instances. A single
  origin degrades to a fixed order.
- Every origin fails: `503` with status text `All Backends Unavailable` and body
  `all backends failed: <last reason>`. Telegram and Stalwart then redeliver, which is why
  nothing is lost.

The health cache feeds only the `/healthz-worker` aggregate view. Forwarding does **not**
filter by health; today, failover is the safety net.

## 7. Health endpoints

The two endpoints deliberately do not overlap, so neither can hide the other.

### 7.1 GET /healthz — passthrough

Forwarded like any other safelisted route. The response is the backend's own liveness
envelope, which is exactly why it can read healthy while a different origin is down.

### 7.2 GET /healthz-worker — gateway aggregate

Answered by the gateway itself. It never touches the forwarded route set.

- Probes each origin's `/healthz` with the same request timeout as forwarding
  (`LB_REQUEST_TIMEOUT_MS`, default 10 s). Redirects are followed, so an origin that answers
  with a `3xx` still counts as up.
- `up` is true when the origin answered with a status below 500. A probe failure records
  `up: false` with `status: null`.
- Results are cached per origin for `LB_HEALTH_TTL_MS` (default 30 s) in an in-isolate map,
  to keep probe traffic bounded. A cache miss probes and stores; a fresh entry is served
  without another probe.
- Response body: `{status, version, available, total, backends: [{origin, up, status}]}`.
  - at least one origin up: HTTP `200`, `status: "ok"`
  - every origin down: HTTP `503`, `status: "down"`
  - no origins configured: HTTP `200`, `status: "no-backends"`
  - `version` is `LB_VERSION`, defaulting to the literal string `unknown`
- Any method other than `GET`: `405` with `Allow: GET`.
- The gateway does not proxy Redis or JMAP readiness. End-to-end backend readiness
  (`ARCH-READY-BASELINE`: config completeness, Redis reachable, JMAP session, Telegram
  `getMe`) stays on the backend's `/ready`, which is passed through.

## 8. Callback registration

The SPA registers callback URLs against **one** origin (`C-LB-SINGLE-REG-URL`). Origin only,
no path:

- Gateway enabled: the Worker URL, for example `https://lb.example`.
- Gateway disabled: a backend origin, for example `https://a.example`.

Whichever origin you register is where `/webhook/tg` and `/push/jmap` land. All three
registration endpoints are safelisted and pass through the gateway, so registration works
through either front door. Registration is idempotent, and registering a new address retires
the previous subscription — that is how you switch fronts.

## 9. Configuration

- Defaults live in `wrangler.toml` `[vars]` as a **deliberate plaintext exception**: the
  current value is only publicly reachable https origins, which is not a credential. The
  moment you need an internal address, a URL with embedded credentials, or a hostname that
  should not be public, move it to `npx wrangler secret put BACKEND_ORIGINS_JSON` and delete
  the line from `[vars]`. Git history is irreversible, so deleting it does not un-publish it.
- `BACKEND_ORIGINS_JSON` is a JSON **array of plain strings**. Object shapes such as
  `{"url": "..."}` are rejected: `parseBackendOrigins` throws, and because the config is
  re-parsed on **every** request the failure shows up as `503` on every request rather than
  at boot. The body is `misconfigured backends`. An empty list throws the same way, before
  anything is forwarded.
- Each entry must be an https **origin** only: no http, no path, no query, no fragment, no
  embedded credentials (`C-HTTPS-INBOUND`).
- `wrangler.toml` also pins `name = "messageweave-lb"`, `main = "src/index.js"`,
  `compatibility_date = "2025-01-01"` and
  `compatibility_flags = ["nodejs_compat_v2"]`. `nodejs_compat_v2` is what lets the test
  files import `node:test` and `node:assert/strict`. The runtime code imports no
  node module.
- Comments in `wrangler.toml` must stay single-line `#` comments. TOML has no block comment,
  so a JSDoc `/** */` block makes wrangler fail to parse the file at all.

## 10. Local and CI verification

```bash
cd cloudflare-worker

npm test       # 38 tests, four files, fully offline
npm run check  # node --check on sources and tests, then wrangler deploy --dry-run
```

`npm test` is `node --test`, which picks up `test/*.test.js`: `test/index.test.js` 15,
`test/lb.test.js` 12, `test/backends.test.js` 6, `test/health.test.js` 5. There is no install
step for the tests — the Worker has no runtime dependencies, and the test files import only
`node:test` and `node:assert/strict`.

Two traps:

- `node --test test/` (a directory) exits 0 after running **zero** tests on some Node
  versions. Always pass the file glob.
- A default CI install step of `bun install` ignores the npm lockfile: it re-resolves and can
  move wrangler across major versions. Add an explicit `npm ci` before the test step if you
  need the pinned wrangler.

## 11. Deploy

```bash
cd cloudflare-worker
npm install --no-audit --no-fund
npm run deploy
curl https://<your-worker>.workers.dev/healthz-worker
```

`npm run check` includes `wrangler deploy --dry-run --outdir=.build-check`, which bundles the
four sources and reports the artifact size before anything is published. The gateway is
deployed from push, so a `curl` of `/healthz-worker` and a compare of its `version` is the
acceptance check. Bump `LB_VERSION` in the same commit as any LB logic change, so you can tell
which build answered.

Two production incidents worth keeping in mind:

- **2026-09-30**: a build wrote `x-lb-backend` onto proxied responses. Cross-origin responses
  without `access-control-allow-*` headers have an immutable header guard, so `headers.set`
  threw and the surrounding handler logged a healthy backend as unavailable and burned its
  retries — the gateway returned `503` while `GET /` actually returned `200`.
  `/healthz-worker` did not go through that code path, which is exactly what hid the outage.
  Fixed by making the header write best-effort.
- **Object-shaped origin lists fail silently at parse time.** `parseBackendOrigins` throws and
  you get `503 misconfigured backends` on every request. Keep `weight` and any other
  per-origin tuning out of the list: nothing reads them.

## 12. Design invariants

- `SAF-LB-PASSTHRU` — no auth, header or body rewriting on the path
- `C-HTTPS-INBOUND` — https-only origins, embedded credentials and paths rejected, HTTP gets a
  308
- `C-LB-SINGLE-REG-URL` — one callback origin, unknown paths 404
- `C-LB-SHARED-SECRETS` — every backend carries identical business credentials
- `MOD-HEALTH-AGG` — TTL-cached aggregate probe owned by the gateway
- `C-NO-DB`, `C-REDIS-ONLY-STATE` — no database or Redis access from the gateway
- `C-NO-LONG-CONN` — request-response only
- `ARCH-READY-BASELINE` — end-to-end readiness stays on the backend's `/ready`
- `SAF-LOG-PURITY` — method, path, origin and failure class only
- `ARCH-LB-WORKER` — the gateway is the only edge component
- `NFR-HA-MULTI-INSTANCE` — multi-origin failover is how HA is realised

## References

- `docs/deployment.md §10` — gateway deployment, configuration and rollout
- `docs/reference.md §4` — gateway versus backend route matrix
- `docs/reference.md §9.2` — backend configuration and secrets
- `docs/reference.md §9.4` — health and release checks
- `docs/design.md §11.3` — HA and multi-instance design
- `docs/charter.md §3`, `docs/charter.md §5`, `docs/charter.md §8` — security baseline,
  prohibitions, stable-ID registry
