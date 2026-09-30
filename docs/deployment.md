# MessageWeave deployment guide

> [中文版本 / Chinese version → deployment.zh-CN.md](deployment.zh-CN.md)

Use this guide to deploy a backend, configure mail delivery, and verify notifications.
Choose any platform that can run the backend container, provide encrypted environment
secrets, and connect to an externally managed Redis service. The optional Cloudflare Worker
gateway adds a stable URL in front of one or more backends. Platform-specific settings are
in the [deployment platform reference](reference.md#9-deployment-platform-details).

## 0. Before deployment

Prepare an externally managed Redis service with persistence enabled and a public HTTPS URL
that reaches the backend. The platform or proxy terminates TLS; MessageWeave listens on one
HTTP port (`PORT`, default `8080`). The process needs two encrypted boot secrets:
`REDIS_URL` and `CONFIG_ENCRYPTION_KEY` (a random 32-byte key encoded as 64 hexadecimal
characters). Never put real values in source control, an image, or logs. Business settings
are saved in Redis through the protected configuration API; adding them as environment
variables does not configure the service.

Production has no local database, data volume, or internal scheduler. Full security boundaries
and prohibited modes are in [`charter.md`](charter.md).

## 1. Deploy a backend

### Container platform

Build the root Dockerfile and configure the required secrets in the platform. For a local
container smoke run, copy `.env.example` to an untracked `.env`, replace both placeholders,
then run:

```sh
docker build -t messageweave:latest .
docker run --rm --env-file .env -p 8080:8080 messageweave:latest
```

The image uses a Rust builder and a Debian slim runtime, runs as a non-root user, and exposes
only port `8080`. Do not mount a data or log volume. Full image contents are in the
[platform reference](reference.md#91-backend-deployment-paths).

## 2. First-time configuration

Open the backend's configuration page at its origin for initial setup. Use
`CONFIG_ENCRYPTION_KEY` to establish the admin session, then complete the one-time bootstrap
form. The bootstrap route is not exposed by the Worker gateway. The one-time bootstrap and
admin-session handlers are `src/notify.rs:834` and `src/notify.rs:1187`; saving business
configuration uses `src/notify.rs:694`. Later edits can use the protected page through the Worker URL.

Configure at least the Telegram bot token, target chat ID, inbound chat allowlist, Stalwart
JMAP HTTPS session URL and application password, webhook secret, and worker token. Set a
reconcile token if using scheduled reconciliation. Configure LLM settings only when users
have explicitly consented to external processing. Secret values are not returned by the
configuration API; the full field semantics are in the [configuration reference](reference.md#5-environment-variables).

### 2.1 Optional remote diagnostics

Leave `DEBUG_ENABLED` and `DEBUG_TOKEN` unset in production unless remote diagnostics are
deliberately needed. Both must be configured to mount the diagnostic routes, and those routes
are reachable only through the backend origin. The diagnostic surface and response details
are recorded in the [reference](reference.md#3-backend-routes).

## 3. Build and run the Docker image

The root Dockerfile is the container deployment path. It has separate Rust builder and
Debian slim runtime stages; see [§8.2](#82-docker-image-details) for the runtime contents.

## 4.1 Register Telegram and Stalwart callbacks

After saving business configuration, use the **Register Telegram and Stalwart callbacks**
section in the configuration page. Enter the public HTTPS origin that receives callbacks:
the Worker URL when the gateway is enabled, otherwise the backend origin. The page registers
`/webhook/tg` with Telegram and `/push/jmap` with Stalwart. Credentials remain in the backend;
the browser sends only the callback URLs. The protected registration handlers are
`src/notify.rs:1439` for Telegram and `src/notify.rs:1315` for Stalwart.

Telegram's secret token and `allowed_updates: ["message"]` are taken from the saved business
configuration. Stalwart verification and verification-code writeback happen automatically
after registration. Registration can be repeated safely; changing the callback origin updates
Telegram and creates a Stalwart subscription for the new URL.

To remove a subscription, call `POST /api/push/disable` with the same `callback_url`.

## 5. Runtime configuration

Keep boot-time process configuration separate from Redis-resident business configuration.
Only `REDIS_URL` and `CONFIG_ENCRYPTION_KEY` are required at process startup. The full
business field list is in the [configuration reference](reference.md#5-environment-variables).

## 6. Short-request runtime and scheduling

The service handles one request at a time and does not keep JMAP event streams, Telegram
long polling, or a background timer open. Telegram/JMAP push provide the fast path; the
external scheduler invokes the endpoints described below.

### 6.3 Schedule queue draining and reconciliation

The backend has no internal scheduler. An external cron, scheduled job, or platform timer
must call [`scripts/cron-drain.sh`](../scripts/cron-drain.sh); otherwise queued notifications
remain undelivered. The script calls `/reconcile` (`src/notify.rs:268`) when
`MW_RECONCILE_TOKEN` is set, then drains the worker queue through `/worker`
(`src/notify.rs:390`).

Provide `MW_APP_URL` and `MW_WORKER_TOKEN`; set `MW_RECONCILE_TOKEN` when reconciliation is
enabled. Store tokens in the scheduler's secret manager. For a one-shot run:

With the tokens injected by the scheduler's secret manager, run:

```sh
MW_APP_URL='https://<PUBLIC_URL>' bash scripts/cron-drain.sh --once
```

The reconciliation cadence is normally 5–10 minutes. Both successful endpoints return `204`
with an empty body; do not treat that as an error. The script's `--diagnose` mode helps
separate service configuration problems from scheduling problems. Its arguments and
operating modes are listed in the [scheduler reference](reference.md#93-callback-registration-and-scheduled-work).

### 6.4 Reliability behavior

The Worker retries only timeouts and backend `5xx` responses; it returns `4xx` responses
directly. `/reconcile` and `/worker` have longer per-route timeouts and do not fail over to a
second backend, avoiding duplicate work. Backend queue processing is at least once and relies
on Redis-backed idempotency. See the route and state reference for the complete behavior.

### 6.5 Delivery objective

The service permits short notification delays and targets at least 99.9% notification
availability. Redis availability and the external scheduling interval are outside the
application's control; exact delivery guarantees are described in `NFR-NOTIFY-SLA` and
`NFR-RECONCILE-INTERVAL` in the project charter.

## 7. Health checks

Use `/healthz` for liveness (`src/notify.rs:118`) and `/ready` for dependency readiness
(`src/notify.rs:193`). The verification sequence is in [§8.1](#81-verify-the-deployment).

## 8.1 Verify the deployment

Check the backend first:

| Request | Expected result | Meaning |
|---|---|---|
| `GET /healthz` | `200` | The process is alive. |
| `GET /ready` | `200` | Configuration, Redis, JMAP, and Telegram probes are ready; dependency failures return `503`. |
| `GET /api/status` | `ready: true` | Required boot settings are present; `missing` identifies absent names. |

When the Worker gateway is enabled, also request `GET /healthz-worker`. Require
`available >= 1` and check that `version` matches the deployed `LB_VERSION`; HTTP 200 alone
can mean `no-backends`. The `cloudflare-worker/` directory's tests and deploy command are
described in its README.

Send `/help` to the configured Telegram chat and confirm a reply. Then send a new email and
confirm that the external scheduler drains its notification. A healthy `/healthz` only proves
process liveness; it does not prove that Redis or either upstream is usable.

## 8.2 Docker image details

The root Dockerfile's builder uses `rust:1-slim-bookworm`; the runtime is
`debian:bookworm-slim`. The runtime installs CA certificates, `tini`, `curl`, `procps`,
`iproute2`, `jq`, and `netcat-openbsd`, creates the `messageweave` system user, sets
`PORT=8080`, and starts the binary through `tini`. It has no `VOLUME` declaration and does
not start Redis. These details describe the repository Docker image; other platforms may provide their own
builder and runtime image.

## 9. Confirmed deployment choices

These deployment decisions are settled; they are not per-installation prerequisites.

### 9.1 Public ingress and state

The backend uses an external HTTPS ingress and external Redis. Telegram, JMAP Push, and
scheduled requests use the selected public URL. When the Worker gateway is enabled, register
that stable Worker URL with Telegram and Stalwart; do not expose credentials at the gateway.
The stable IDs in this section point to the project charter's registry.

## 10. Optional Cloudflare Worker gateway

The Worker provides one public HTTPS entry point for multiple backend origins. All backends
must share the same Redis and business configuration. The Worker forwards only its fixed
route allowlist; bootstrap and `/debug/*` remain origin-only. Detailed route facts are in
the [gateway route matrix](reference.md#4-gateway-vs-backend-route-matrix).

### 10.1 Topology

Telegram webhooks, Stalwart push callbacks, and external scheduling can use the stable Worker
URL. The Worker forwards to backend HTTPS origins; it does not connect to Redis or JMAP.

### 10.2 Multi-instance prerequisites

Multiple backends can share traffic because request state is stored in Redis, duplicate
deliveries are deduplicated, and reconciliation repairs missed push events. No sticky session
or application code change is required.

### 10.3 Trust model

The Worker passes request headers and bodies through. Backend authentication remains required
because backend origins may be directly reachable. Every instance must use the same shared
business credentials and Redis state.

### 10.4 Routes and failover

HTTP requests to the Worker receive a permanent `308` redirect to the same HTTPS URL before
route validation or forwarding. The Worker rejects unregistered routes with `404` and wrong
methods with `405`. Ordinary requests try at most `LB_MAX_ATTEMPTS` origins (default two), and
only a timeout or `5xx` triggers failover. `/reconcile` and `/worker` are single-attempt routes with longer timeout
overrides. Use the route matrix and tuning values in the [reference](reference.md#4-gateway-vs-backend-route-matrix).

### 10.5 Callback URLs

When using the gateway, set Telegram's webhook and Stalwart's push callback to the Worker
domain. The external scheduler can use the same URL for forwarded paths. Bootstrap and
diagnostic endpoints still require the backend origin.

### 10.6 Gateway health

`GET /healthz-worker` reports the Worker version and backend availability. Use both
`available` and `version` to verify a deployment; `status: no-backends` can be returned with
HTTP 200 when no valid origin is configured.

### 10.7 Boundaries

The Worker is not a Redis proxy and does not make the backend stateless; shared external Redis
remains required. It does not add a database, a long-lived connection, or a second layer of
business authentication.
