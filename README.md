# MessageWeave

中文文档：[README.zh-CN.md](README.zh-CN.md) · Architecture & design: [docs/design.md](docs/design.md) · Deployment & operations: [docs/deployment.md](docs/deployment.md)

A personal email assistant that connects your **Stalwart mailbox** to **Telegram**: you get notified in Telegram when a new email arrives, and you can read or summarise emails on demand.

> ⚠️ **This is an early-stage project — please complete a small-scale integration test first.**
> Real mailboxes and Telegram still need to be verified by the operator. JMAP Push subscription registration and verification write-back are implemented, but the callback URL is never guessed automatically: an administrator must call the registration API explicitly.

---

## What this project is

- You run your own [Stalwart](https://stalwart.dev) mail server.
- This tool connects your mailbox to a Telegram bot: when a new email arrives, it pushes a notification (sender / subject / time only — **no message body**).
- In Telegram you can ask to view the raw email, or to have it summarised or translated by an AI.

### Privacy model

- **Viewing the raw email never touches any AI.** It is read straight from your mailbox.
- **The body is sent to an AI only when you explicitly ask for it and confirm.**
- Analysis results are never stored.
- The AI is optional and can be left unconfigured or fully disabled.

---

## What you need before deploying

1. **A Stalwart mailbox account**, with an **App Password** generated for it — not your account's master password.
2. **A Telegram bot** (create one via @BotFather), and its **Bot Token**.
3. **A Redis database** that you operate (AOF persistence recommended). All of the bot's state lives there. Use `rediss://` for TLS Redis and `redis://` for plain connections; if your password (Upstash etc.) contains `@`, `:`, `/` or `#`, URL-encode it first.
4. **A public HTTPS address.** The platform points this address at the bot container (HTTPS is provided by the platform).
5. **An external cron / scheduler** that calls `/reconcile` every 5–10 minutes so the bot periodically reconciles against your mailbox. The bot does not run any timers of its own.

### Addresses that must be registered

- Tell Telegram to send its webhook to `your-public-address/webhook/tg`.

> If you use the optional "multiple instances + Cloudflare Worker load balancer" setup (see `docs/deployment.md` §10), point the Telegram webhook and the reconcile URL at the **Worker's stable domain** (`https://<your-worker>.workers.dev/webhook/tg` etc.) instead of the per-instance platform addresses.

---

## Creating the container in your Docker manager

Create a new container, fill in the usual image / port / memory fields, and inject only the Redis credentials:

| Environment variable | Required? | Value |
|---|---|---|
| `REDIS_URL` | ✔ | Redis connection string (including password), e.g. `redis://:password@redis.example.com:6379/0` |
| `CONFIG_ENCRYPTION_KEY` | ✔ | A 32-byte hex key; generate with `openssl rand -hex 32` |

After the first start, submit the Telegram / JMAP / LLM and business-auth configuration through the protected SPA page. Configuration secrets are encrypted into Redis and are never returned by the API; after a successful admin `PUT` the service validates the new configuration and hot-rebuilds the clients atomically, so subsequent requests use it immediately, and on failure the previous instances are kept.

AI summarisation is off by default. A Telegram user must first send one of the recognised consent phrases, then `/summary <email_id>`: `/ai on` or `/ai yes` (1 hour); any message containing `临时` or `一次`, e.g. `临时一次` (one-off, 1 hour); `今天` (24 hours); `7天` (7 days); or `直到我撤销` / `长期` (until revoked, capped at 365 days). These phrases are matched literally, so type them in Chinese exactly as shown — English aliases are not recognised. To revoke: `/ai off` (or `撤销授权`). Authorisation never auto-renews once it expires. Without a valid authorisation only metadata is returned, and if the LLM is unset or fails the bot falls back to a locally truncated summary.

> If `REDIS_URL` or `CONFIG_ENCRYPTION_KEY` is missing, the service enters *configuration-setup* mode: the SPA, `/api/status` and the probes still work. The SPA hides the admin-session section and shows only which variables are missing, together with the note that keys are never stored or echoed. Once the configuration is restored, the authorisation UI appears only after `/api/status` reports `ready=true`.

### HostStack native deployment

HostStack builds and runs the Rust service using the repository-root `hoststack.yaml`: it runs `cargo build --release --locked`, executes `./target/release/message-weave`, and uses `/healthz` as a health check every 30 seconds with a 5-second timeout. Provide `REDIS_URL` and `CONFIG_ENCRYPTION_KEY` as HostStack Secrets; the keys are never written into the YAML, the image, or any repository file.

The "enable business processing" switch in the admin session is persisted in Redis at `config:enabled`; a missing value or a read failure is treated as disabled. While disabled, the webhook, reconcile and worker routes return HTTP 503 and neither ack nor drop upstream events; the SPA, status and configuration APIs stay available.

---

## How to check that it is healthy after startup

- Open **`your-public-address/healthz`** in a browser or with `curl`: a normal response means the bot process is alive.
- `/ready` returns a JSON readiness report and checks configuration completeness and Redis reachability: 200 means the basic dependencies are ready, 503 means it should not receive traffic. It never calls JMAP or Telegram and never triggers a business side effect.
- For monitoring, Uptime Kuma is recommended: an HTTP liveness check on `/healthz` and an HTTP readiness check on `/ready` (expect 200, 30-second interval, 5-second timeout). The project ships no Prometheus exporter.
- `/reconcile` performs authentication, takes a Redis single-flight lock, and runs a bounded JMAP `Email/changes` reconciliation. Events are written to Redis Streams idempotently, and the Redis `sinceState` cursor is only advanced after all of them are queued; a temporary dependency failure returns `503` so cron can retry. Push events still go through the authenticated, de-duplicated enqueue path, so this must not be described as complete real-time synchronisation.
- The cold-start baseline also has page, message and time budgets. When a budget is exhausted the checkpoint is saved and the next `/reconcile` continues from it, so a large mailbox is never re-scanned from the beginning.

## Configuration management

Open the root address **`your-public-address/`** and enter the password of the Redis ACL user from `REDIS_URL` to create an admin session valid for 15 minutes. The session and the keys typed into the form live only in the current page's memory; after a refresh or closing the page you must authorise again. The page never stores sessions, keys or configuration in browser storage or cookies.

The page keeps two kinds of configuration separate: runtime parameters (JMAP, Telegram, LLM timeouts and retry limits) can be read and saved through the API; business configuration (JMAP, Telegram, LLM, chat allowlist and the various auth tokens) is fully replaced through `PUT /api/business-config`. To avoid echoing secrets, the service exposes no read endpoint for business configuration, so every full replacement requires re-entering the mandatory fields. After a 204 response the page reports that the new configuration has been hot-loaded and clears the key inputs.

While Redis is not connected or initialised, the admin-session and configuration APIs return `503`; the page explains the unavailable state and keeps runtime-parameter saving disabled until the read succeeds. When reached through a Cloudflare Worker, use the Worker's root URL; the admin API paths `/api/config`, `/api/business-config`, `/api/admin/session` and `/api/admin/session/revoke` are on the restricted route allowlist (`POST /api/push/register` is not — see `docs/deployment.md` §10.5).

## End-to-end first integration

This is a repeatable path from an empty deployment to a first message in Telegram. Every `<...>` below is a placeholder; do not copy the angle brackets, and never paste real secrets into chats, tickets or the repository.

### 1. Verify the deployment and Redis

```bash
curl -fsS https://<message-weave-domain>/api/status
curl -fsS https://<message-weave-domain>/healthz
```

With correct `REDIS_URL` and `CONFIG_ENCRYPTION_KEY`, `/api/status` should contain:

```json
{"ready":true,"mode":"configured","missing":[]}
```

If a variable is missing, the container does not exit; it enters `configuration-setup`, and the SPA shows only the missing variables without the admin-session section. The Redis URL must use the right scheme: `rediss://` for TLS, `redis://` for plain connections. A TLS Redis must not be verified only with `redis-cli --tls` — the application's own `REDIS_URL` must use `rediss://` too.

### 2. Create a Telegram bot and a test group

Open `@BotFather` in Telegram, send `/newbot`, follow the prompts and save the bot token. Then create a **group** (not a channel), e.g. `MessageWeave`, add the bot and make it an administrator.

Send `/start` in the group, then read the Chat ID from the Bot API's `getUpdates` endpoint:

```bash
curl "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/getUpdates"
```

In the response:

```json
"chat":{"id":-5260770881,"title":"MessageWeave","type":"group"}
```

`-5260770881` is the Chat ID. Group IDs are usually negative — do not drop the minus sign. `getUpdates` is not a group-list endpoint; the group only appears once the bot has actually received a message in it. If it returns an empty array, send `/start` in the group first. If a webhook is already set, delete it first, otherwise messages are consumed by the webhook:

```bash
curl -X POST "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/deleteWebhook"
```

If the bot cannot see ordinary messages, run `/setprivacy` in `@BotFather`, choose the bot and select `Disable`; or make sure the bot is an administrator. Switch the webhook back on once debugging is done.

### 3. Generate the application-side auth secrets

Generate these three values separately. Do not reuse the Redis password, the bot token, or the SPA admin session:

```bash
openssl rand -hex 32   # Telegram Webhook Secret
openssl rand -hex 32   # Reconcile Token
openssl rand -hex 32   # Worker Token
```

- **Telegram Webhook Secret**: you generate it, submit it through the SPA, and pass it as `secret_token` to `setWebhook`. MessageWeave validates the `X-Telegram-Bot-Api-Secret-Token` request header.
- **Reconcile Token**: you generate it; the external cron uses it for `/reconcile`.
- **Worker Token**: you generate it; it protects `/worker` and the compatibility admin API.

### 4. Create the SPA admin session

Open `https://<message-weave-domain>/`, enter the password of the Redis ACL user from `REDIS_URL`, and click to create the admin session. It is valid for 900 seconds, lives only in the current page's memory, and must be created again after a refresh.

### 5. Fill in the Telegram business configuration

Example:

```text
Target Chat ID:          -5260770881
Chat allowlist:          -5260770881
Telegram Webhook Secret: <first openssl random value>
Reconcile Token:         <second openssl random value>
Worker Token:            <third openssl random value>
```

`Target Chat ID` and `Chat allowlist` are **not** the same field: the former is the default target, the latter is the whitelist of chats allowed to drive the bot. For a single test group they can hold the same ID. The allowlist accepts one entry per line or a comma-separated list; duplicates are rejected.

### 6. Fill in the JMAP business configuration

- The `JMAP Session URL` may be either `https://mail.example.com` or `https://mail.example.com/.well-known/jmap`; both are normalised.
- HTTPS only. Do not put a username, password, query string or fragment into the URL.
- `JMAP Username` is the mailbox account.
- `JMAP Password` is the Stalwart **App Password**.
- `Account ID` may be left empty for the first integration test; the session's primary account is used.
- You do not need to create a PushSubscription by hand in the Stalwart WebUI. The built-in `user` role normally already includes reading, creating, modifying and deleting PushSubscriptions; only if registration actually returns `forbidden` should an administrator check the role under `/admin → Management → Directory → Accounts/Roles`. Stalwart's Push global settings under `Settings → Network → JMAP → Push` are not a per-subscription configuration.
- Keep the LLM disabled for the first integration test, so you do not mix mailbox problems with AI network configuration.

### 7. Register the JMAP PushSubscription

MessageWeave does not require you to create a PushSubscription by hand in the Stalwart WebUI. After saving the JMAP business configuration, call the registration API with the current SPA admin session or `WORKER_TOKEN`, supplying a public HTTPS callback URL explicitly:

```bash
curl -X POST "https://<messageweave-domain>/api/push/register" \
  -H "Authorization: Bearer <admin-session-or-worker-token>" \
  -H "Content-Type: application/json" \
  -d '{"callback_url":"https://<messageweave-domain>/push/jmap"}'
```

On success the response contains `push_subscription_id`. The URL must be HTTPS and must not contain a username, password or any other embedded credential. Re-registering the same callback URL reuses the existing subscription instead of creating a duplicate. Stalwart then sends its generated `PushVerification` to `/push/jmap`; MessageWeave writes the verification code back through JMAP `PushSubscription/set`, and only accepts real Push events once verification has succeeded. The registration API never guesses the platform domain and never accepts a user-entered verification code.

The regular `user` role normally already has read/create/modify/delete permissions on PushSubscriptions, so no manual configuration is needed; only check `/admin → Management → Directory → Accounts/Roles` if you actually get `forbidden`. Stalwart's `Settings → Network → JMAP → Push` page holds global Push parameters, not per-subscription creation.

References: [Stalwart Push notifications](https://stalw.art/docs/http/jmap/push/), [Stalwart Permissions](https://stalw.art/docs/auth/authorization/permissions/), [Stalwart Roles](https://stalw.art/docs/auth/authorization/roles/), [RFC 8620 §7.2 PushSubscription](https://www.rfc-editor.org/rfc/rfc8620.html).

When the business configuration is saved the service really instantiates the JMAP client; if that fails it returns `503`, stores no broken configuration, and you can fix and re-save. There is no GET endpoint for business configuration, so after a successful save the keys are not echoed back; every full replacement requires re-entering the mandatory fields.

### 8. Set the Telegram webhook and enable business processing

After the business configuration is saved:

```bash
curl -X POST "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/setWebhook" \
  --data-urlencode "url=https://<message-weave-domain>/webhook/tg" \
  --data-urlencode "secret_token=<TELEGRAM_WEBHOOK_SECRET>"

curl "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/getWebhookInfo"
```

Make sure the returned `url` is correct and that `last_error_message` does not keep growing. Then go back to the SPA and switch on "enable business processing". While the global switch is off, the webhook, reconcile and worker routes return `503 service disabled` and neither enqueue nor ack upstream messages.

Finally send a test message in the allowlisted group. When it arrives, watch Telegram `getWebhookInfo`, the platform logs and the bot's reply. If there is no reply, check the Chat ID's minus sign, the allowlist, the Webhook Secret, the global switch, and the bot's admin rights first.

### 9. Test reconcile authentication

```bash
curl -i -X POST "https://<message-weave-domain>/reconcile" \
  -H "Authorization: Bearer <RECONCILE_TOKEN>"
```

A wrong token must return `401`. The external cron is recommended to call once every 5–10 minutes, as a safety net for email reconciliation when Push events are lost.

## How email synchronisation works today

Do not treat PushSubscription as the only reliable source in the current release. Push callbacks still have to be authenticated and enqueued, so run the external cron calling `/reconcile` at the same time. Reconciliation uses a persistent Redis `sinceState` and returns `503` on enqueue failure instead of advancing the cursor early.

If your platform lets the service sleep, the cron's HTTP request will usually wake the container, but the first request pays the cold-start cost and may exceed the cron's or the platform's request timeout. Therefore:

- set the cron timeout long enough and allow retries on failure;
- do not call more often than the platform's wake-up / request limits allow;
- do not use `/healthz` as email synchronisation — it only checks that the process is alive;
- if the service cannot be woken while sleeping, reconciliation cannot run either; in that case disable sleeping or use an always-on instance.

The reconciliation lock is renewed by its holder while the task runs; if the renewal fails the cursor is not advanced. Push registration and reconciliation both use Redis owner-token single-flight locks, so concurrent requests cannot create duplicate subscriptions or advance the same JMAP cursor twice.

## Troubleshooting

### Redis: `can't connect with TLS, the feature is not enabled`

Usually the Redis TLS feature was not enabled when the image was built, or `redis://` / `rediss://` is wrong. Use the latest image with TLS support and make sure `REDIS_URL=rediss://...`. Special characters in an Upstash password must be URL-encoded, e.g. `@` becomes `%40`.

### Redis: `Multiplexed connection driver unexpectedly terminated`

First validate the network and credentials with the same URL from outside. Do not validate only `redis-cli --tls` while the application still uses `redis://`. Check that HostStack / the platform allows outbound 6379/TLS, and check the ACL user, password and port in the URL.

### rustls provider panic

This is the old image loading more than one TLS provider. You must use the latest image, which installs the explicit rustls `ring` provider; do not keep using cached old image tags.

### `getUpdates` returns an empty array

The bot has received no new messages, or the message was already consumed by the webhook. Delete the webhook and send `/start` in the group again; confirm the bot is in the group, is an administrator, and — if needed — that privacy mode is disabled through BotFather.

### Telegram says it is not allowed to read messages

Confirm that you created a **group** and not a channel; make the bot an administrator; send `/start` again. With privacy mode enabled, the bot normally only sees commands, replies and mentions.

### SPA returns `503` when saving business configuration

This usually means the JMAP URL is unreachable from the deployment platform, the certificate/DNS is broken, the app password is wrong, or the LLM configuration fails HTTPS validation. A failed save keeps the old worker and the old configuration, and does not lock the initialisation slot; fix and retry.

### SPA shows *configuration-setup*

Check that the HostStack Secrets or container environment contain both:

```text
REDIS_URL
CONFIG_ENCRYPTION_KEY
```

`CONFIG_ENCRYPTION_KEY` must be a 32-byte hex value, e.g.:

```bash
openssl rand -hex 32
```

It must never be written into the public `hoststack.yaml`, the Dockerfile, the README or the image.

## Privacy and operational red lines (transparent to users)

- **No database, no local files**: all of the bot's state lives in your Redis. The container writes no local files and mounts no local volumes; after a restart it recovers from Redis plus mailbox reconciliation.
- **Logs go to stdout only**: the container prints logs to stdout/stderr, and your platform or collector is responsible for collection and storage. The bot **does not write log files**.
- **Sensitive content never lands in logs**: mailbox bodies, keys, AI requests/responses and attachment contents are **never** written to logs or Redis. Only structured events, counters, timestamps and redacted summaries are recorded.
- See the checklists in `docs/deployment.md` §0 and §8.2.

---

## What it can do today / what is still missing

**Already in place**

- The entry-point framework, three layers of auth protection, and configuration loading.
- A configuration SPA backed by short-lived Redis admin sessions: the full business configuration can be edited and hot-loaded, and `/api/config` runtime parameters can be read and saved.
- Basic code for reading your Stalwart mailbox (list folders, list emails, read raw messages).
- Telegram webhook, Chat ID allowlist, Reconcile / Worker bearer tokens, and a Redis-persisted global enable switch.
- HostStack native Rust deployment configuration and a Docker build.
- **Optional**: a free Cloudflare Worker front-load-balancer for multi-instance HA — see `docs/deployment.md` §10 and the `cloudflare-worker/` directory.

**Still missing / not yet verified**

- ⚠️ **Real mailbox integration needs to be verified by the operator**: the JMAP client code and configuration validation are provided, but different Stalwart networks, permissions and app passwords still differ.
- **JMAP Push registration is implemented**: after an administrator submits a public HTTPS callback URL through `POST /api/push/register`, the backend calls `PushSubscription/set create` and `/push/jmap` writes the verification code back automatically. A real Stalwart environment still needs to be verified, and the external cron calling `/reconcile` remains the reliable compensation channel.
- Production-grade multi-instance coordination and sending email are not finished yet; AI summarisation is optional and needs separate configuration and verification.

Both a Docker image and the HostStack native Rust deployment are provided, but we still recommend running the integration steps above on a small scale before using it in production.

## Quality gates for contributors

Everything must be green before a change is released (run inside the Debian `rust:1-slim-bookworm` container — see `AGENTS.md` §5 and `docs/deployment.md` §8):

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- Real-device verification tests are marked `#[ignore]` and driven by environment variables (`cargo test -- --ignored jmap::`); CI does not run them by default. Do not claim "verified on real hardware" before actually running them.
- Any change that touches a security boundary must be reflected consistently in all three of `docs/design.md`, `docs/deployment.md` and `AGENTS.md`.

---

## For developers

Architecture, design and deployment details:

- [Architecture & design](docs/design.md)
- [Deployment & operations](docs/deployment.md)
- [Rules for AI coding assistants](AGENTS.md)
- [Open items and blockers](docs/todo.md)

## License

(TBD)
