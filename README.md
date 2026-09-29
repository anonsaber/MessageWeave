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

## 2. 5-minute setup

Requires a reachable Redis 7 instance and a JMAP session plus a Telegram bot token.

**This is the local/dev path.** The repo's `Dockerfile` is for local `docker run` only --
production does not execute it. Live deploys run from `hoststack.yaml` with
`runtime: rust`, which HostStack builds in its own `rust:slim-trixie` image and runs in its
own `debian:trixie-slim` runner, so the Dockerfile's diagnostic tools, `ENTRYPOINT` and
`EXPOSE` do not apply there. See `docs/deployment.md` §3 and §5.1 for the boundary.

```sh
cp .env.example .env           # fill REDIS_URL and CONFIG_ENCRYPTION_KEY
docker build -t messageweave:latest .
docker run --env-file .env -p 8080:8080 messageweave:latest
```

Then open `http://localhost:8080` in a browser and enter your `CONFIG_ENCRYPTION_KEY`
value. The SPA exchanges it with `POST /api/admin/session` for a 900-second admin session,
then writes the business secrets to Redis with `PUT /api/business-config`. No environment
variable is read or written again for business configuration.

For local builds without Docker: `cargo build --locked` produces the `message-weave` binary.

If the service answers requests but does nothing useful, check the logs for a startup
warning: a missing required variable does **not** crash the process. It serves a
read-only setup router over in-memory state instead. See §3.

## 3. Configuration entry points

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
that must stay cheap should watch the gateway's aggregated `/healthz`.

**Remote debug (opt-in, off by default).**
`/debug/*` is an optional remote-debug surface: live JMAP and Telegram probes, the current
business config, and a single Telegram send. It sits behind a two-factor gate — the process
must be started with `--debug` **and** `DEBUG_TOKEN` must be set; miss either and the routes
do not exist at all (requests fall through to a generic 404). Nothing under `/debug/*` is on
the gateway allowlist, so it is reachable only on the backend origin itself. The only rule for
running it is to keep it off: leave `--debug` out of the start command and leave `DEBUG_TOKEN`
unset. If you do turn it on for a one-off diagnosis, configure the chat allowlist first — with
an empty allowlist the test send is not restricted to any chat. See
`docs/deployment.md` §2.1.

## 4. Security boundary, in one sentence

> All state lives in Redis; the process reads only two environment variables and writes
> nothing to disk.

One corollary matters for setup: the SPA admin credential — and therefore the bootstrap
trust root — is `CONFIG_ENCRYPTION_KEY` itself, the single 32-byte hex value you supply at
startup. It is only compared in constant time, and is never echoed, logged, or stored. The
Redis ACL password, if you have one, authenticates the Redis connection alone and is not
the credential for any HTTP endpoint.

## 5. Where to read more

| Document | What it answers |
|---|---|
| [`docs/design.md`](docs/design.md) | Why the system is shaped this way: data flow, JMAP semantics, Redis streams, AI consent rules |
| [`docs/deployment.md`](docs/deployment.md) | How to deploy: Dockerfile, secrets, Redis hosting, webhook/push/reconcile routing, multi-instance load balancing |
| [`docs/reference.md`](docs/reference.md) | **Single source of truth for verifiable facts** — Redis keys and TTLs, error codes, routes, environment layers, budgets |
| [`docs/roadmap.md`](docs/roadmap.md) | Gaps, blockers and the next phase |
| [`docs/retired.md`](docs/retired.md) | What was tried and dropped — abandoned routes, unreleased designs, and names that never existed |
| [`docs/charter.md`](docs/charter.md) | Project charter: goals, locked technology choices, security invariants, prohibitions, and the stable-ID registry |
| [`AGENTS.md`](AGENTS.md) | Language-agnostic engineering norms: code style, config and secrets, build environment, testing gates, document governance |

Facts that matter are traceable. If two documents disagree, `docs/reference.md` wins.
