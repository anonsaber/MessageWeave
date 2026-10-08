# 1. Quickstart

Chinese: [quickstart.zh-CN.md](quickstart.zh-CN.md)

This page is a from-zero walkthrough for a human operator: create a Telegram bot, deploy
one backend, fill the configuration page, register the callbacks, and get the first
notification. It only contains steps and decisions. Field semantics, defaults, TTLs and
error codes live in [reference.md](reference.md); production, scaling and gateway
operations live in [deployment.md](deployment.md).

## 1.1 What you need

- A mailbox on a JMAP server you administer (this guide assumes Stalwart), HTTPS only.
- A Telegram account.
- A Redis endpoint with persistence (Upstash's free tier works; it is TLS-only, so the URL
  starts with `rediss://`).
- A machine or platform that can run one container and expose one HTTP port.
- One public HTTPS origin that can receive webhooks — your own domain, a tunnel, or a
  Cloudflare route. Telegram and Stalwart both require HTTPS.
- Optional: an OpenAI-compatible LLM key, a Cloudflare account if you want the gateway.

Nothing else is required: no Postgres, no message broker, no Kubernetes.

## 1.2 Create the Telegram bot

1. In Telegram open a chat with `@BotFather` and send `/newbot`.
2. Pick a display name, then a username that ends in `bot` or `_bot`.
3. BotFather replies with a token that looks like `1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw1`.
   That string is the bot token. Treat it like a password: anyone holding it can read your
   webhook traffic and send messages as your bot. It is never echoed back by the API once saved.
4. Send the new bot any message (for example `/start`) from the account or group that should
   receive notifications. Until you do, Telegram has no chat to deliver to.

## 1.3 Find the chat id

1. Open `https://api.telegram.org/bot<TOKEN>/getUpdates` in a browser, replacing `<TOKEN>` with
   the token from step 3.
2. In the JSON, find `result[].message.chat.id`. Private chats give a positive number such as
   `123456789`; groups and supergroups give a negative one such as `-1001234567890`.
3. For a group, add the bot to the group first, then send a message in the group before you call
   `getUpdates`. The bot only sees messages in groups after you disable privacy mode under
   `@BotFather` → `/setprivacy` → `Disable`, otherwise group messages never reach it.

You will use this id twice in step 7: as the notification target and as a member of the inbound
allowlist. The allowlist must contain at least one id — an empty allowlist is rejected as invalid
configuration rather than treated as "allow everyone".

## 1.4 Get the JMAP credentials

1. In your Stalwart admin, create an application password for the mailbox — an app password, not
   the account login password. The backend authenticates with HTTP Basic over JMAP.
2. The session URL is your mail origin: `https://mail.example.com`. You may also paste
   `https://mail.example.com/.well-known/jmap`; the backend collapses both to the origin, and
   rejects any URL with embedded credentials, a query string or a fragment.
3. The JMAP account id is optional. Leave it empty: after the first successful save the backend
   reads it back from the server and the page shows it.

## 1.5 Run the backend

The container needs exactly two environment variables:

```bash
# .env — never commit this file
REDIS_URL=rediss://default:<password>@<host>:6379
CONFIG_ENCRYPTION_KEY=<64 hex characters, from: openssl rand -hex 32>
```

- `REDIS_URL` is your only state store. Back it up like a database.
- `CONFIG_ENCRYPTION_KEY` doubles as the SPA admin credential and the encryption key for stored
  business secrets. If you lose it you can no longer read the configuration page or decrypt the
  stored secrets; if you rotate it, previously stored secrets become unreadable.

Then build and start:

```bash
docker build -t messageweave .
docker run --rm --env-file .env -p 8080:8080 messageweave
curl localhost:8080/healthz   # → ok
```

`GET /healthz` is a static liveness reply and never touches Redis. The readiness report is
`GET /ready`: it answers 503 until the configuration exists and every configured upstream is
reachable, and 200 with a JSON component report once they are. Point your HTTPS reverse proxy or
tunnel at this port now — steps 8 and 9 need a public HTTPS origin.

## 1.6 Sign in to the configuration page

1. Open the page in a browser. If the backend started with the required variables the first card
   is **Create admin session**.
2. Paste `CONFIG_ENCRYPTION_KEY` — not the Redis password. This mints an admin session that lives
   only in the page's memory for 30 minutes; reloading the page logs you out.

## 1.7 Fill in the business configuration

The form is a full replacement, not a patch: whatever you submit becomes the whole stored
configuration. The sections:

- **01 JMAP mailbox** — session URL and username, plus the app password from step 4.
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

## 1.8 Register the callbacks

Still on the same page, under **External callbacks**:

1. Paste the public HTTPS origin that reaches this backend, scheme and host only, no path —
   for example `https://mw.example.com`. If you put the Cloudflare gateway in front, use the
   gateway's origin, not the backend's: callbacks must survive a backend restart, and the
   gateway is what stays up.
2. Click **Register callbacks**. One click registers both directions: the Telegram webhook at
   `/webhook/tg` and the Stalwart push subscription at `/push/jmap`. Verification is handled
   automatically; the page reports the result.

Registration is explicit and repeatable. Registering a new origin cancels the previous subscription,
so the old origin stops receiving pushes. A failed registration never removes the old one — if it
can't reach Telegram, the previous webhook stays in place. If it did reach Telegram but not
Stalwart, the page reports a partial failure and tells you to click again.

Verify from a terminal:

```bash
curl -s "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
# expect "url": "https://mw.example.com/webhook/tg" and "pending_update_count": 0
```

## 1.9 Turn on processing

Flip **Enable business processing** at the top of the page. This is the master switch: with it off,
webhooks are accepted but `/worker` and `/reconcile` refuse to drain, so nothing reaches Telegram.
With it on, incoming mail and messages are enqueued and the worker drains them.

## 1.10 Verify end to end

1. Send yourself an email. Within a few seconds the bot should post a notification with the
   sender, subject and received time — never the body. That path alone proves JMAP ingest,
   Redis, the queue and the Telegram send all work.
2. In Telegram try `/search 发票`. Note: the search triggers are the Chinese words `搜索`, `查找`
   and `检索`, plus the `/search` command — English keywords like `search invoice` are not
   recognised as triggers.
3. Try `/summary <email_id>` from the notification's email id. The first call asks for
   confirmation; the summary only goes out after you confirm, and only if the LLM section is
   enabled.

## 1.11 Schedule the drain

Push delivery can be delayed or missed; an external scheduler is the safety net. On any machine
that can reach your origin, every 5–10 minutes:

```bash
MW_APP_URL=https://mw.example.com \
MW_WORKER_TOKEN=<worker token> \
MW_RECONCILE_TOKEN=<reconcile token> \
scripts/cron-drain.sh
```

The script drains the queue (`/worker`) and rescans for missed pushes (`/reconcile`); both return
204 with an empty body on success. `MW_RECONCILE_TOKEN` is optional — if omitted, the reconcile
step is skipped and only the queue is drained. If you put the gateway in front, `/worker` must
reach the backend origin directly — see deployment.md for why.

## 1.12 Optional: the Cloudflare gateway

Skip this unless you want a load balancer across two backends. The Worker is optional, gives
free-tier Cloudflare users a load balancer, and is not a security layer: the backend origins stay
directly reachable either way. Setup lives in `cloudflare-worker/README.md`; when you use it,
register the callback origin from step 8 against the gateway.

## 1.13 When something is wrong

- **Nothing arrives after saving.** Check the enable toggle (step 9), then
  `getWebhookInfo` (step 8). A `last_error_message` about a 401 means the webhook secret does not
  match what Telegram sends — re-save section 02 and re-register.
- **`/ready` answers 503.** That body is just an error code — it does not name the failing
  component. When `/ready` returns 200, its JSON has per-field booleans (`jmap`, `telegram`,
  `redis`). Common causes are an unreachable `jmap_session_url`, a wrong app password, or
  `runtime_applied: false` from the last save.
- **The form says conflict.** Two sessions saved at once; reload and redo (step 7.3).
- **The bot ignores you in a group.** Privacy mode is on; disable it under `@BotFather` (step 3).
- **`/search` answers with a help hint.** Your query started with a Latin word; the prefix triggers
  are `搜索` / `查找` / `检索`.

Field-level errors, TTLs, Redis keys and route tables: [reference.md](reference.md).
Scaling, secrets rotation, gateways and cron on the wire: [deployment.md](deployment.md).
