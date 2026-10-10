# Unfinished items (Open Gaps)

> [中文版本 / Chinese version → opengaps.zh-CN.md](opengaps.zh-CN.md)

There are currently **no unfinished items** in this document: all four stage goals have been closed, and the only pending product decision (multiple accounts) has been determined not to be made by the user on 2026-09-28 (§3). Keep closing records to facilitate tracing why and how each gap was closed.

The implemented interface and architecture design can be found in `docs/reference.md`, `docs/design.md`, and the retired capabilities can be found in `docs/retired.md`. The verification baseline is based on the "P0 Access Control" section of `docs/design.md`.

## What you have to do

**none. **

The TTL green light for real machines has already been passed using the Upstash URL you gave (see §1), and there is no need to submit anything else.

## Stage goal

| # | item | status |
| --- | --- | --- |
| 1 | `newState` semantics for `Email/changes` | Closed (commit `269c8f6`) |
| 2 | TTL actual measurement | Closed (code closing `0890eb1` + real machine `ttl` two assertions run through) |
| 3 | Telegram inbound / Stalwart `PushSubscription` joint debugging | Closed (real traffic has been driven, please see `README.md` §3.7 for operation steps) |
| 4 | `/worker` is not in the deployment document | Closed (`README.md` §3.10 has been added) |

Access control: `cargo fmt --check` / `cargo check --locked` / `cargo clippy --locked --all-targets -- -D warnings` all green; `cargo test --locked` **94 passed / 0 failed / 4 ignored** (2 TTL assertions out of 4 `#[ignore]` have been run with real Upstash: `REDIS_TEST_URL=… cargo test --locked -- --ignored ttl` → **2 passed / 0 failed**; the remaining 2 require real JMAP servers, this capability has been verified in staging joint debugging); documentation gate is 6/6 PASS, including 72 line number anchor non-null verifications.

Item 5 is not within the stage goal: multi-account product decision-making, which has been determined not to be done by the user on 2026-09-28. The decision boundary and future changes will be discussed in §3.

## blocking

**none. ** All four stage goals have been closed, both the code side and the real machine side have been connected, and the only pending product decision has been finalized (§3).

## Closed

### 1. TTL actual measurement (closed: constant closing + real machine assertion run-through)

Original entry: Full code base **12 TTL write points, 17 TTL values**, but only three atomic commands that actually fall to Redis (`SET...NX EX` of `set_nx_ex`, `EXPIRE` of `retry_or_dlq` in Lua, `SET... EX` of `set_ai_consent`) are actually asserted on real Redis PTTL; the rest are all bare literals. The test double `MemoryState::claim_dedup` directly ignores the TTL parameter, so the single test covers the calling path, not the expiration length - changing a 24-hour deduplication window to `60_480` can still compile and pass, and the single test is all green.

Three steps closed the issue:

1. **True Redis Assertion** (commit `0890eb1`). `real_redis_ttl_tests` of `src/state.rs`, two `#[ignore]`-gated use cases, follow the pattern of `src/domain/jmap/client.rs:522`: skipped and returned when `REDIS_TEST_URL` is missing, the URL will not be logged. Assert actual PTTL for three atomic write points. Run: `REDIS_TEST_URL=… cargo test --locked -- --ignored ttl`.
2. **Kill naked literals** (one round after commit `0890eb1`). All 17 values     are included in `pub(crate) mod ttl` of `src/state.rs`, the call point changes the constant reference, and a new `ttl_contract_is_pinned` is added to assert the constant table one by one, with two sequential assertions (the heartbeat must be shorter than the lock, and the consent gear must be strictly increasing). Class B Lua embedded values   that were out of reach in the original entry have also been connected - `retry_or_dlq` now uses `ttl::RETRY_COUNTER_SECONDS` interpolation to generate script strings, and there is no longer a second literal in the script. Changing the value now = changing the constant + assertion failure, both of which must be reviewed.
3. **Real machine green light** (this round, run with Upstash instance). The two assertions were actually connected to the real Redis to read back the PTTL and passed. The Class B Lua embedded value was verified on a real machine for the first time. The process is shown below in "Testing the four TTL test fixes".

Contract baseline (see `docs/reference.md` §1 for the complete semantics of keys):

| constant | value (seconds) | key |
| --- | --- | --- |
| `ttl::RECONCILE_LOCK_SECONDS` | 300 | `lock:reconcile` |
| `ttl::RECONCILE_HEARTBEAT_SECONDS` | 90 | `lock:reconcile` (heartbeat renewal) |
| `ttl::PUSH_REGISTER_LOCK_SECONDS` | 360 | `lock:push-register:{sha256(callback_url)}` |
| `ttl::PUSH_VERIFY_LIMIT_SECONDS` | 30 | `ratelimit:push-verify:{subscription_id}` |
| `ttl::DEDUP_TG_SECONDS` | 86_400 | `dedup:tg:{update_id}` |
| `ttl::DEDUP_JMAP_SECONDS` | 86_400 | `dedup:jmap:{account_id}:{email_id}` (shared with callback enqueue and reconciliation incremental enqueue) |
| `ttl::DELIVERY_INFLIGHT_SECONDS` | 60 | `delivery:inflight:{stream}:{message.id}` |
| `ttl::DELIVERY_COMMITTED_SECONDS` | 604_800 | `delivery:committed:{stream}:{message.id}` |
| `ttl::PUSH_DISABLED_SECONDS` | 86_400 | `push:subscription:{id}:status` (`disabled`) |
| `ttl::PUSH_STATUS_PENDING_SECONDS` | 900 | `push:subscription:{id}:status` (`pending`) |
| `ttl::PUSH_STATUS_VERIFIED_SECONDS` | 300 | `push:subscription:{id}:status` (`verified`) |
| `ttl::PUSH_SUBSCRIPTION_SECONDS` | 300 | `push:subscription:{id}` / `push:subscription-code:{code}` |
| `ttl::PUSH_REGISTRATION_SECONDS` | 604_800 | `push:registration:{sha256(callback_url)}` |
| `ttl::PUSH_ORPHAN_SECONDS` | 604_800 | `push:orphan:{subscription_id}` |
| `ttl::ADMIN_SESSION_SECONDS` | 1,800 | `admin-session:{token}` (also the `expires_in` returned by `/admin/session`) |
| `ttl::RETRY_COUNTER_SECONDS` | 86_400 | `retry:{stream}:{message.id}` |
| `ttl::CONSENT_TEMPORARY_SECONDS` | 3_600 | `consent:ai:{chat_id}` ("temporary/once" shared with explicit `/ai on`) |
| `ttl::CONSENT_TODAY_SECONDS` | 86_400 | `consent:ai:{chat_id}` ("Today") |
| `ttl::CONSENT_WEEK_SECONDS` | 604_800 | `consent:ai:{chat_id}` ("7 days") |
| `ttl::CONSENT_MAXIMUM_SECONDS` | 31_536_000 | `consent:ai:{chat_id}` ("until revoked / long term") |
| `ttl::CONSENT_REVOKED_SECONDS` | 0 | `consent:ai:{chat_id}` (revoked; `max(1)` = 1s, semantics is "expired") |

Naked TTL literals no longer exist in production code; `worker.rs`'s agreed-upon test assertions still retain numeric literals, which is **deliberate** - use expected values   independent of constants to test production code, otherwise if both sides are changed at the same time, the single test will always be green.

This update fixed four bugs in the test itself.

- The single test binary will not run `main()`, rustls does not have the crypto provider installed, and the `rediss://` handshake will directly panic; now the test module adjusts `install_rustls_provider()` by itself.
- The key construction of `retry:{stream}:{message.id}` uses message id instead of run id. The assertion reads a key that has never been written. PTTL returns `-2` and the first sentence of the test fails.
- `worker.rs` reconciles the TTL literal where the increment was added to the queue, which was originally missing and is still `86_400`.

The fourth is that the real machine asserts that it is unstable. The first three can be discovered at compile time:

- The zero value case originally asserted `assert_within(pttl, 1, 500)` - requiring the 1 second floor to be read back within ±500 ms. But the readback occurs after the cross-network TLS round trip, and that second is usually consumed. Running three times in a row got `-2` (key has expired), `269`, `118` ms. There is nothing wrong with the product code itself: `SET … NX EX 1` is indeed implemented, and `-2` just proves that EX takes effect and the keys expire as expected. Change the criterion to "`pttl != -1`" - `-1` is the real regression to prevent (EX is silently discarded, the key never expires), plus "`pttl == -2 || pttl <= 1000 + 2000`" is a judgment that only sets an upper bound, because a value that is still alive will only count down from 1s. After the change, I ran 6 consecutive rounds with all green.

**Two TTL assertions have been run on the real Upstash instance (`rediss://`, TLS-only; plaintext `redis://` will receive a null reply after `AUTH` and disconnect, so TLS must be used)**: `SET … NX EX` (`claim_dedup`), `SET … EX 3600` (`set_ai_consent`), Lua embedded `EXPIRE 86400` (`retry_or_dlq`) The actual PTTL of the three atomic write points all fall within the 2s tolerance. Class B Lua embedded values   that were out of reach in the original article are now also verified on real machines, not just constant references.

### 2. `newState` semantics of `Email/changes` (commit `269c8f6`)

The original article worried that "the server returns `newState` when it cannot replay old increments, requiring the client to re-baseline". But jmap-client 0.4.2's `ChangesResponse` only has `accountId` / `oldState` / `newState` / `hasMoreChanges` / `created` / `updated` / `destroyed` plus expanded call parameters - **no reset / re-baseline signal field**. Therefore, the failure of `/changes` is just an ordinary `Err` in the code, and it is impossible to distinguish between "temporary failure" and "the server will no longer play back this state". This entry therefore cannot be closed by any observation means: what can be observed in the shutdown window is still the same `Err`.

The actual behavior of the code is worse than the entry describes: `Err` is thrown up to `Err(())` as is, `reconcile:state` is left as is - the next cron tries again with the same dead `sinceState`, fails again, and gets another 503. The cursor is permanently frozen and `/reconcile` always returns `503 reconcile_retry` without any advancement.

**Modification**: `/changes` will be re-baselined when it fails - take a fresh `session_state()`, write back `baseline:{fresh_state}:0` and return `Ok`. Position walk does not rely on the server-side changelog retention policy at all, so it can be recovered by re-walking from position 0 in the next round; the 24h deduplication key (`enqueue_reconcile_event`, `ttl::DEDUP_JMAP_SECONDS`) ensures that the replay will be repeated once at worst without leakage. Added single test `reconcile_rebaselines_when_changes_replay_is_stale` to lock this behavior.

> The position walk path (`list_emails_page(None, position, BASELINE_PAGE_SIZE)`) itself is not affected - there is no server-side state parameter in its signature (`None` is `folder_id`), so it will not encounter the above type of token semantics.

### 3. Multiple accounts (decided: not to do, 2026-09-28 user decision)

Single accounts are fixed by requirement (`REQ-SINGLE-ACCOUNT`) and never enter the stage target. The user made it clear on 2026-09-28 that "no multiple accounts will be made for the time being" and this gap was closed. `docs/design.md` §11.3, the `REQ-SINGLE-ACCOUNT` line of `docs/charter.md` and the environment variable table/requirements mapping table of `docs/reference.md` were originally expressed in this way, and these three places have not been changed in this round.

**This article determines the scope of coverage**

- **No multi-account single instance** - One bot instance serves multiple JMAP accounts at the same time. Therefore there is no need for chat→account routing, per-domain/per-folder routing, or `JmapService` pooling.
- **Existing path when second mailbox is required**: Deploy a second bot instance, each with independent `BOT_TOKEN` / `ACCOUNT_ID` / `JMAP_SESSION_URL` and Redis prefix. This is an existing capability that is documented and requires no new code (`README.md` §6.1).
- **Trigger condition for re-opening**: When there is a routing requirement within a single instance such as "the same TG session needs to be routed according to the source mailbox", then dismantle `REQ-SINGLE-ACCOUNT`.

**Minimum changes if necessary in the future** (The current situation has been verified in this round and will be used directly when disassembly is needed)

- `state:jmap:since` is a **single global cursor**, no account dimension (read `src/state.rs:792`, write `src/state.rs:800`) - both accounts will overwrite each other's cursors, which is the first place to move.
- `lock:reconcile` is a **single global solo lock** (Redis implementation `src/state.rs:1130`) - the reconciliation of two accounts will be serialized by the same lock.
- `dedup:jmap:{account_id}:{email_id}` **Already has the account segment** (`docs/reference.md` §1), the key shape does not need to be changed.
- The `ACCOUNT_ID` environment variable already exists and has been verified (leave it blank to take the session main account, and an error will be reported if the out-of-bounds value is `src/domain/jmap/client.rs:485`). The business configuration layer can also override `account_id` (`unwrap_or_else` of `src/config.rs:465` falls back). The configuration interface is ready, all that is missing is the sub-dimension status within the instance.

This item is not skipped, but explicitly rejected: the deployment unit saved by single instance and multiple accounts cannot be exchanged for the routing and cursor complexity it introduces.

## Deployment document defects discovered and fixed in this round

`README.md` §3.10 Originally, only the scheduling example of `/reconcile` was given, and `/worker` - the only queue consumption entry in the entire code base - was described as "manual triggering of operation and maintenance" and was deliberately not included in the Worker whitelist. **Copying the deployment according to the document = emails continue to be queued and notifications are never sent out**. This is the reason why the joint debugging environment is backlogged for several days before being emptied manually. `README.md` §3.10 The description of `/worker`’s scheduling steps, sequence requirements, batch limit and "204 cannot be used as a success signal" has been added.

At the same time, the anchor points of `/worker` in `docs/reference.md` and `docs/retired.md` are corrected: the original value `notify.rs:340` falls in the `reconcile` function body, and the anchor point audit will not report an error (the line exists and is not empty), but the semantics are wrong; the correct value is `notify.rs:390`.
