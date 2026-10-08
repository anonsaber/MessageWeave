# MessageWeave

**[中文文档 / Chinese documentation → README.zh-CN.md](README.zh-CN.md)**

> One Telegram bot per mailbox. New mail arrives as metadata; the original is always
> fetched read-only from your own JMAP server.

## 1. What it is

MessageWeave sits between a JMAP mail server (for example Stalwart) and a Telegram bot.

- **Inbound.** A JMAP push subscription tells the service that a message changed. Nothing
  is polled at rest.
- **Outbound.** Each event becomes a short, **metadata-only** Telegram notification:
  sender, subject, timestamp. Mail bodies are never sent to Telegram.
- **On demand.** When you ask, the service reads the original through a fresh JMAP read
  call. Nothing is cached in between.
- **Optionally summarised.** If you explicitly opt in, an OpenAI-compatible LLM returns a
  short summary. Requests, responses and attachment bytes are never persisted.

State is an architectural boundary here, not a preference.

- **Redis is the only state store.** No database, no embedded store, no second store.
- **The process writes nothing to disk.** No log files, no data files, no local volume.
- **No long-lived connections.** No SSE, no WebSocket, no long polling.
- **Logs go to stdout only.** The platform's collector owns log storage and retention.
- **What it is not.** It is not a mail client, not a proxy, and not a relay. It forwards
  metadata to one bot; it does not read arbitrary URLs, open tunnels, or broker traffic.

## 2. Two ways to run it

One image, one Redis, one SPA. The only difference is what sits in front and which URL you hand
to Telegram and Stalwart.

**The Cloudflare Worker load balancer is optional.** Nothing in the backend requires it, and the
backend runs identically with or without it. It exists for one reason: Cloudflare's Load
Balancer product is not available on the free plan, so a Worker is the way a free-plan account
puts load sharing and failover in front of several backends.

**It is also not a security layer, and it is not meant to be one.** There is no intent to hide
the backends and no intent to harden them; the origins stay directly reachable whether the
load balancer is present or not, and no access control is added at the edge. The fixed route safelist
is what makes it a load balancer for a bounded set of paths, not a firewall.

| | Form A: backend origin only | Form B: behind the Cloudflare Worker load balancer |
|---|---|---|
| What answers requests | one backend host | one Worker URL, N backend origins |
| Failover | none — a down origin stays down | bounded: 2 attempts by default, only timeout and `5xx` retry |
| Callback URL you register | `https://a.example` | `https://lb.example` |
| Backend access | direct from the network | equally direct — the load balancer adds no access control |
| Load balancer route set | n/a — there is no load balancer | fixed at 19 paths; unknown paths answer `404` |
| Health to watch | `GET /ready` on the origin | `GET /ready` proxied, plus `GET /healthz-worker` for the load balancer aggregate |
| Extra moving parts | none | one Worker deploy, one origin list |
| Docs | `docs/deployment.md §2` | `docs/deployment.md §10`, `cloudflare-worker/README.md` |

Pick Form A for a single origin with no HA requirement. Pick Form B for two or more origins, or
whenever you want one stable callback URL while you roll or rebuild an instance.

Switching is a re-registration, not a migration. Registration is idempotent and registering a
new address retires the previous subscription, so moving from Form A to Form B, or back, is one
field in the SPA. Do not register both origins at once: there is a single registration slot, and
the platform entrance must stay internal (`C-LB-SINGLE-REG-URL`).

No secret moves to the load balancer in Form B. It holds no business credential and checks no auth
header. Instead every backend must carry the same `SAF-AUTH-*` secrets and the same encrypted
business config, because a callback can land on any instance and no instance can prove it was
the one asked for (`C-LB-SHARED-SECRETS`). Mis-matched secrets show up as a random 401, not as
a routing error.

`POST /api/bootstrap` and `/debug/*` are not in the load balancer's route set, so requests addressed
to the Worker URL never reach them. That is the safelist working as designed for the paths that
need to be load balanced; it is not hiding anything, because the same origins are reachable at
their own addresses in both forms.

## 3. Setup, from zero

This section is a hand-holding walkthrough: create a Telegram bot, deploy one backend, fill
the configuration page, register the callbacks, get the first notification. Field semantics,
defaults, TTLs and error codes live in `docs/reference.md`; production, scaling and load
balancer operations live in `docs/deployment.md`.

### 3.0 What you need

- A mailbox on a JMAP server you administer (this guide assumes Stalwart), HTTPS only.
- A Telegram account.
- A Redis endpoint with persistence (Upstash's free tier works; it is TLS-only, so the URL
  starts with `rediss://`).
- A machine or platform that can run one container and expose one HTTP port.
- One public HTTPS origin that can receive webhooks — your own domain, a tunnel, or a
  Cloudflare route. Telegram and Stalwart both require HTTPS.
- Optional: an OpenAI-compatible LLM key, a Cloudflare account if you want the load balancer.

Nothing else is required: no Postgres, no message broker, no Kubernetes.

### 3.1 Create the Telegram bot

1. In Telegram open a chat with `@BotFather` and send `/newbot`.
2. Pick a display name, then a username that ends in `bot` or `_bot`.
3. BotFather replies with a token that looks like `1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw1`.
   That string is the bot token. Treat it like a password: anyone holding it can read your
   webhook traffic and send messages as your bot. It is never echoed back by the API once saved.
4. Send the new bot any message (for example `/start`) from the account or group that should
   receive notifications. Until you do, Telegram has no chat to deliver to.

### 3.2 Find the chat id

1. Open `https://api.telegram.org/bot<TOKEN>/getUpdates` in a browser, replacing `<TOKEN>` with
   the token from step 1.
2. In the JSON, find `result[].message.chat.id`. Private chats give a positive number such as
   `123456789`; groups and supergroups give a negative one such as `-1001234567890`.
3. For a group, add the bot to the group first, then send a message in the group before you call
   `getUpdates`. The bot only sees messages in groups after you disable privacy mode under
   `@BotFather` → `/setprivacy` → `Disable`, otherwise group messages never reach it.

You will use this id twice in step 6: as the notification target and as a member of the inbound
allowlist. The allowlist must contain at least one id — an empty allowlist is rejected as invalid
configuration rather than treated as "allow everyone".

### 3.3 Get the JMAP credentials

1. In your Stalwart admin, create an application password for the mailbox — an app password, not
   the account login password. The backend authenticates with HTTP Basic over JMAP.
2. The session URL is your mail origin: `https://mail.example.com`. You may also paste
   `https://mail.example.com/.well-known/jmap`; the backend collapses both to the origin, and
   rejects any URL with embedded credentials, a query string or a fragment.
3. The JMAP account id is optional. Leave it empty: after the first successful save the backend
   reads it back from the server and the page shows it.

### 3.4 Run the backend

The container needs exactly two environment variables:

```sh
# .env — never commit this file
REDIS_URL=rediss://default:<password>@<host>:6379
CONFIG_ENCRYPTION_KEY=<64 hex characters, from: openssl rand -hex 32>
```

- `REDIS_URL` is your only state store. Back it up like a database.
- `CONFIG_ENCRYPTION_KEY` doubles as the SPA admin credential and the encryption key for stored
  business secrets. If you lose it you can no longer read the configuration page or decrypt the
  stored secrets; if you rotate it, previously stored secrets become unreadable.

Then build and start:

```sh
docker build -t messageweave .
docker run --rm --env-file .env -p 8080:8080 messageweave
curl localhost:8080/healthz   # → ok
```

For local builds without Docker: `cargo build --locked` produces the `message-weave` binary.

`GET /healthz` is a static liveness reply and never touches Redis. The readiness report is
`GET /ready`: it answers 503 until the configuration exists and every configured upstream is
reachable, and 200 with a JSON component report once they are. Point your HTTPS reverse proxy or
tunnel at this port now — steps 6 and 7 need a public HTTPS origin.

If the service answers requests but does nothing useful, check the logs for a startup warning:
a missing required variable does **not** crash the process. It serves a read-only setup router
over in-memory state instead. See §4.

### 3.5 Sign in to the configuration page

1. Open the page in a browser. If the backend started with the required variables the first card
   is **Create admin session**.
2. Paste `CONFIG_ENCRYPTION_KEY` — not the Redis password. This mints an admin session that lives
   only in the page's memory for 30 minutes; reloading the page logs you out.

### 3.6 Fill in the business configuration

The form is a full replacement, not a patch: whatever you submit becomes the whole stored
configuration. The sections:

- **01 JMAP mailbox** — session URL and username, plus the app password from step 3.3.
- **02 Telegram and access auth** — bot token, notification chat id, allowlist, and three secrets
  you make up yourself: webhook secret, reconcile token and worker token. Generate them with
  `openssl rand -hex 32` each. The webhook secret must match on the Telegram side later; the
  reconcile and worker tokens protect the scheduler endpoints.
- **03 LLM service** — optional. Leave disabled unless you want AI digests; if you enable it, the
  HTTPS base URL, model name and API key all become required.
- **04 Notification display** — the time zone used to render received times in notifications.

Then, in order:

1. Click **Test connection**. It preflights the credentials without saving anything.
2. Click **Save business config and hot-reload**. The first save must contain every required
   field or the server answers 422 and stores nothing.
3. Read the result line under the button: `runtime_applied: true` means the new runtime was
   installed on this backend. `false` means it saved but the runtime was not installed, and the
   `warnings` array says which component failed — most often an unreachable `jmap_session_url`.
   Fix the field and save again; `/ready` stays 503 until it goes green.
4. If the save answers **409 conflict**, someone else (another tab, a second operator) saved in
   between. Reload the page to pick up their values, then re-apply your edit.

Secrets are write-only: the page never shows a stored secret again, and submitting an empty secret
field keeps the previously stored value — clear a secret deliberately in a separate edit.

### 3.7 Register the callbacks

Still on the same page, under **External callbacks**:

1. Paste the public HTTPS origin that reaches this backend, scheme and host only, no path —
   for example `https://mw.example.com`. If you put the Cloudflare load balancer in front, use its
   origin, not the backend's: callbacks must survive a backend restart, and the load balancer is
   what stays up.
2. Click **Register callbacks**. One click registers both directions: the Telegram webhook at
   `/webhook/tg` and the Stalwart push subscription at `/push/jmap`. Verification is handled
   automatically; the page reports the result.

Registration is explicit and repeatable. Registering a new origin cancels the previous subscription,
so the old origin stops receiving pushes. A failed registration never removes the old one — if it
can't reach Telegram, the previous webhook stays in place. If it did reach Telegram but not
Stalwart, the page reports a partial failure and tells you to click again.

Verify from a terminal:

```sh
curl -s "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
# expect "url": "https://mw.example.com/webhook/tg" and "pending_update_count": 0
```

### 3.8 Turn on processing

Flip **Enable business processing** at the top of the page. This is the master switch: with it off,
webhooks are accepted but `/worker` and `/reconcile` refuse to drain, so nothing reaches Telegram.
With it on, incoming mail and messages are enqueued and the worker drains them.

### 3.9 Verify end to end

1. Send yourself an email. Within a few seconds the bot should post a notification with the
   sender, subject and received time — never the body. That path alone proves JMAP ingest,
   Redis, the queue and the Telegram send all work.
2. In Telegram try `/search 发票`. Note: the search triggers are the Chinese words `搜索`, `查找`
   and `检索`, plus the `/search` command — English keywords like `search invoice` are not
   recognised as triggers.
3. Try `/summary <email_id>` from the notification's email id. The first call asks for
   confirmation; the summary only goes out after you confirm, and only if the LLM section is
   enabled.

### 3.10 Schedule the drain

Push delivery can be delayed or missed; an external scheduler is the safety net. On any machine
that can reach your origin, every 5–10 minutes:

```sh
MW_APP_URL=https://mw.example.com \
MW_WORKER_TOKEN=<worker token> \
MW_RECONCILE_TOKEN=<reconcile token> \
scripts/cron-drain.sh
```

The script drains the queue (`/worker`) and rescans for missed pushes (`/reconcile`); both return
204 with an empty body on success. `MW_RECONCILE_TOKEN` is optional — if omitted, the reconcile
step is skipped and only the queue is drained. If you put the load balancer in front, `/worker`
must reach the backend origin directly — see `docs/deployment.md` §10 for why.

### 3.11 When something is wrong

- **Nothing arrives after saving.** Check the enable toggle (step 3.8), then
  `getWebhookInfo` (step 3.7). A `last_error_message` about a 401 means the webhook secret does not
  match what Telegram sends — re-save section 02 and re-register.
- **`/ready` answers 503.** That body is just an error code — it does not name the failing
  component. When `/ready` returns 200, its JSON has per-field booleans (`jmap`, `telegram`,
  `redis`). Common causes are an unreachable `jmap_session_url`, a wrong app password, or
  `runtime_applied: false` from the last save.
- **The form says conflict.** Two sessions saved at once; reload and redo (step 3.6).
- **The bot ignores you in a group.** Privacy mode is on; disable it under `@BotFather` (step 3.2).
- **`/search` answers with a help hint.** Your query started with a Latin word; the prefix triggers
  are `搜索` / `查找` / `检索`.

## 4. Configuration entry points

Three layers. They are not interchangeable.

**Boot-time environment — 2 required, 1 optional.**

| Variable | Required | Default |
|---|---|---|
| `REDIS_URL` | yes | — |
| `CONFIG_ENCRYPTION_KEY` | yes | — |
| `PORT` | no | `8080` |

There is no `RUN_MODE`: the legacy environment parser was removed, and webhook plus
reconcile traffic share one router (`POST /reconcile` is a standalone endpoint).
`.env.example` ships only the two required
variables; the rest are documented defaults.

**Business configuration — Redis, written by the browser.**
`POST /api/bootstrap` is the one-shot trust bootstrap; `PUT /api/business-config` hot-
reloads individual settings within about one second. The process never reads business
secrets from the environment in normal operation.

**Health.**
`GET /healthz` returns 200 unconditionally — it is liveness, not readiness. `GET /ready`
returns 503 until configuration, Redis, and both upstreams (a JMAP session `GET` and
Telegram `getMe`, 3s each) are reachable. Probe `/ready`, not `/healthz`, when deciding
whether to route traffic — but `/ready` is the heavier probe (~3s worst case), so a monitor
that must stay cheap should watch the load balancer aggregate `GET /healthz-worker` when the load balancer
is enabled.

**Remote debug (opt-in, off by default).**
`/debug/*` is an optional remote-debug surface: live JMAP and Telegram probes, the current
business config, and a single Telegram send. It sits behind a two-factor gate — the process
must be started with `--debug` **and** `DEBUG_TOKEN` must be set; miss either and the routes
do not exist at all (requests fall through to a generic 404). Nothing under `/debug/*` is on
the load balancer allowlist, so it is reachable only on the backend origin itself. The only rule for
running it is to keep it off: leave `--debug` out of the start command and leave `DEBUG_TOKEN`
unset. If you do turn it on for a one-off diagnosis, configure the chat allowlist first — with
an empty allowlist the test send is not restricted to any chat. See
`docs/deployment.md` §2.1.

## 5. Security boundary, in one sentence

> All state lives in Redis; the process reads only two environment variables and writes
> nothing to disk.

One corollary matters for setup: the SPA admin credential — and therefore the bootstrap
trust root — is `CONFIG_ENCRYPTION_KEY` itself, the single 32-byte hex value you supply at
startup. It is only compared in constant time, and is never echoed, logged, or stored. The
Redis ACL password, if you have one, authenticates the Redis connection alone and is not
the credential for any HTTP endpoint.

The optional Cloudflare Worker load balancer in front of the backends is not part of this boundary.
It holds no credential and checks no auth header, and the backends behind it are directly
reachable either way, so nothing about it changes what stands between an attacker and your
mail. See §2.

## 6. Where to read more

| Document | What it answers |
|---|---|
| [`docs/design.md`](docs/design.md) | Why the system is shaped this way: data flow, JMAP semantics, Redis streams, AI consent rules |
| [`docs/deployment.md`](docs/deployment.md) | How to deploy: Dockerfile, secrets, Redis hosting, webhook/push/reconcile routing, multi-instance load balancing |
| [`docs/reference.md`](docs/reference.md) | **Single source of truth for verifiable facts** — Redis keys and TTLs, error codes, routes, environment layers, budgets |
| [`docs/opengaps.md`](docs/opengaps.md) | Gaps, blockers and the next phase |
| [`docs/retired.md`](docs/retired.md) | What was tried and dropped — abandoned routes, unreleased designs, and names that never existed |
| [`docs/charter.md`](docs/charter.md) | Project charter: goals, locked technology choices, security invariants, prohibitions, and the stable-ID registry |
| [`cloudflare-worker/README.md`](cloudflare-worker/README.md) | The Cloudflare Worker load balancer: route safelist, timeout and retry budget, bounded failover, health aggregate, deploy |
| [`AGENTS.md`](AGENTS.md) | Language-agnostic engineering norms: code style, config and secrets, build environment, testing gates, document governance |

Facts that matter are traceable. If two documents disagree, `docs/reference.md` wins.
