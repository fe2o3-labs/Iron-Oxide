# Billing: plans, feature gating and Stripe

Iron Oxide has two plans, **free** and **pro**, stored in `users.plan` (the `user_plan` enum,
`free` by default). This page covers what each plan includes, how the code gates features, and how
Stripe will flip `users.plan`. Today only the gating is live. The Stripe side is a documented stub
(#21).

| File | What |
|---|---|
| `crates/iron-oxide-domain/src/entitlements.rs` | **The policy**: `Plan`, `Feature`, `Quota`, `allows`, `limit`, `can_add`, `Entitlements` |
| `crates/iron-oxide-app/src/server/entitlements.rs` | `require` (feature, 403 on refusal), `reserve_quota` (takes a quota slot under the user's row lock, 403 at the cap), `check` / `check_quota` (pure) |
| `crates/iron-oxide-app/src/server/db/users.rs` | `plan(executor, user)`, `lock_plan(tx, user)` (`FOR UPDATE`), `unarchived_programs(executor, user)` |
| `crates/iron-oxide-app/src/api/billing.rs` | `my_entitlements()` server function for the UI (`POST /api/billing/entitlements`) |
| `crates/iron-oxide-app/src/server/billing.rs` | `POST /webhooks/stripe` stub (answers `501`) |

## Policy

`iron_oxide_domain::entitlements` is the **only** code that decides what a plan may do. Nothing
else compares a plan. Both matches in it are exhaustive, so a new plan, feature or quota does not
compile until its policy is written. The tests `feature_matrix` and `quota_matrix` pin the tables
below; change them together.

### Features (on/off)

| Feature | What it gates | Free | Pro |
|---|---|---|---|
| `UploadPrograms` | Uploading your own `program.json` (#19, #35) | yes | yes |
| `ExerciseCharts` | Per-exercise progress charts in the history (#33) | yes | yes |

### Quotas

| Quota | What is counted | Free | Pro |
|---|---|---|---|
| `CustomPrograms` | Programs the user owns and has not archived: uploaded ones **and copies of built-ins** | 10 | unlimited |

Programs are never deleted, only archived, so archiving one frees a slot and unarchiving one takes
a slot. "Unlimited" still has the abuse limits of rate limiting (#23).

For now almost everything is free: the custom program cap is the only difference, and it is high
enough that a lifter rarely meets it. To gate a feature, change its arm in `allows` to
`matches!(plan, Plan::Pro)` and update this table. No other code changes.

### Never gated

These must stay available on every plan. Do not add a `Feature` for them:

- signing in, and managing passkeys and Google;
- browsing the built-in programs and choosing the active program (copying a built-in is not behind
  a feature, but the copy counts toward `CustomPrograms`, see below);
- logging workouts, the rest timer, the plate calculator, and settings;
- the session history list and session details;
- **GDPR rights (#22)**: exporting, importing and deleting your data and your account.

**Downgrading never deletes or hides data.** A user who goes from pro to free keeps every program,
including those over the free cap, and can still use, rename, edit and archive them. While over the
cap they cannot add a program (create, upload, copy a built-in) **or unarchive one**, until archiving
brings them under the cap.

## Gating in code

The plan is read from `users.plan` **on every call**, never cached in the session or anywhere
else. Flipping `users.plan` (billing, or an operator in SQL) therefore changes what the user may do
on their next request, with no other code change. `flipping_the_plan_in_the_database_changes_the_entitlements`
and `the_entitlements_endpoint_follows_the_database_plan` test exactly that.

In a server function:

```rust
#[cfg(feature = "server")]
use crate::server::{AppState, auth::AuthUser, entitlements};

#[post("/api/programs/upload", state: Extension<AppState>, user: AuthUser)]
pub async fn upload_program(/* ... */) -> Result<(), ServerFnError> {
    entitlements::require(&state.db, user, Feature::UploadPrograms).await?;
    // ...
}
```

- `require(db, user, feature)` returns the plan, or a `403` with a short message ("This feature is
  part of Iron Oxide Pro."). A user deleted meanwhile gets `401`, and a database failure `500`
  (`503` when transient). Details are only logged.
- **Quotas: `reserve_quota(&mut tx, user, quota)`, in the transaction that writes, before the
  write.** It locks the user's row (`SELECT … FROM users WHERE id = $1 FOR UPDATE`, held until the
  transaction ends), reads the plan, counts the quota (`unarchived_programs`) and checks it, all
  under that lock, and answers `403` at the cap ("Your plan keeps up to 10 programs. Archive one, or
  upgrade to Pro."). Two concurrent requests of one user are thereby serialised: the second counts
  the first one's row. Counting on the pool outside such a transaction is always racy, so there is
  no pool variant. `two_concurrent_reservations_cannot_both_take_the_last_slot` is the reference
  test of this recipe.
- **Every write that raises the `CustomPrograms` count must call `reserve_quota`** (#19 implements
  them):

  | Program write (#19) | Takes a slot? |
  |---|---|
  | Create a program / upload a `program.json` as a new program | yes |
  | Copy a built-in | yes |
  | **Unarchive** a program | yes |
  | Idempotent replay (same creation id: answered from the existing row) | no: check for the replay **before** reserving, so a retry is never refused |
  | Archive, rename, upload a new version of an existing program, set the active program | no |

  The repository functions that do these writes must therefore run in the caller's transaction
  (or take it), so the reservation and the insert or update commit together.
- The error mapping is local to `server/entitlements.rs` for now. It moves to `ApiError::Forbidden`
  once #68 lands (TODO in the code).
- The UI calls `my_entitlements()` (a `POST`, `401` when signed out) to show locks, remaining slots
  and upgrade prompts. That copy is for display only: the server checks every gate itself.

## Stripe

**Not implemented yet.** This section is the design. It is written to be implemented as-is.

### Stripe objects

- **Customer**: one per user, created by us before the first Checkout, so the id is known before
  any event arrives. `metadata.user_id` holds our `users.id`.
- **Price**: the Pro price or prices (monthly, maybe yearly). Their ids go in configuration, never
  in code.
- **Checkout Session** (`mode=subscription`): created by a server function for the signed-in user,
  with `customer` set to their customer and `client_reference_id` set to their user id. It
  redirects to Stripe's hosted page. We never see card data.
- **Subscription**: normally one live subscription per customer, but **nothing guarantees it**: a
  Checkout submitted from two tabs, or a resubscription while an old subscription is still being
  cancelled, can leave several. The plan is therefore always derived from **all** of the
  customer's subscriptions (see **Sync**), never from one stored id. Checkout is only offered to a
  user without a live subscription (the account screen sends pro users to the Customer Portal).
- **Customer Portal**: Stripe's hosted page for cancelling, changing the card, and invoices.

### Tables (a future migration)

The customer id links a Stripe customer to a user. The tables are designed here and ship with the
billing implementation, not in #21.

```sql
-- One row per user who has ever started a checkout.
CREATE TABLE billing_customers (
    user_id              uuid        PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    stripe_customer_id   text        NOT NULL UNIQUE,  -- cus_…
    stripe_subscription_id text,                       -- sub_…, the one shown (see Sync); NULL if none
    subscription_status  text,                         -- that subscription's status, as last fetched
    current_period_end   timestamptz,
    cancel_at_period_end boolean     NOT NULL DEFAULT false,
    updated_at           timestamptz NOT NULL DEFAULT now()
);

-- Every event id we have processed, for idempotency.
CREATE TABLE stripe_events (
    event_id     text        PRIMARY KEY,              -- evt_…
    event_type   text        NOT NULL,
    processed_at timestamptz NOT NULL DEFAULT now()
);
```

| Column | Why |
|---|---|
| `billing_customers.user_id` | The owner. Cascades with account deletion (#22). |
| `stripe_customer_id` | How a webhook event finds the user: every subscription event names its customer. Unique, so a customer can never map to two users. |
| `stripe_subscription_id`, `subscription_status`, `current_period_end`, `cancel_at_period_end` | The **shown** subscription (the one that grants pro, else the most recent), for support and the account screen ("renews on", "ends on"). Display only: the plan is never derived from this one id. The row is also the per-customer **lock** of the sync. |
| `stripe_events.event_id` | Deduplication. Stripe may deliver an event more than once. Rows older than 35 days can be pruned: Stripe retries for up to 3 days, and a manual resend works for up to 30. |

`users.plan` stays the one thing gating reads. Billing only *writes* it, derived from the
subscription status.

### Events to plan

Subscribe the endpoint to these event types only.

| Event | Action |
|---|---|
| `checkout.session.completed` (`mode=subscription`) | Find the user by `client_reference_id`, check that it matches the customer's `metadata.user_id` and our `billing_customers` row, then **sync** the customer. If `payment_status` is `unpaid` (delayed payment methods), the sync leaves the plan free until a subscription is `active`. |
| `customer.subscription.created` / `.updated` / `.deleted` | **Sync** the event's customer (`data.object.customer`). The event's own subscription is not trusted on its own. |
| `invoice.payment_failed` | No plan change. The subscription becomes `past_due` and `customer.subscription.updated` follows. A later version may email the user. |
| Anything else | Answer `200` and ignore it. |

**Sync a customer** (every event above, inside the event's transaction, step 6 below):

1. **Lock the customer first:** `SELECT user_id FROM billing_customers WHERE stripe_customer_id = $1
   FOR UPDATE`. Every sync of the same customer now runs one after the other, and the lock is held
   until commit.
2. **Then**, under the lock, list **all** the customer's subscriptions from the Stripe API
   (`GET /v1/subscriptions?customer=cus_…&status=all`, paginated).
3. Map each status with the table below. The plan is **pro if any subscription maps to pro**, else
   free (`incomplete` alone leaves the plan unchanged).
4. Store the shown subscription (the pro-granting one with the latest `current_period_end`, else the
   most recent) in `billing_customers`, set `users.plan`, and commit, releasing the lock.
5. If **more than one** subscription is live (`active`, `trialing` or `past_due`), log a warning
   with the ids and cancel the newer duplicates through the API (with proration): the user must not
   be billed twice. Their `customer.subscription.deleted` events then sync again and change nothing.

| `status` | Plan | Why |
|---|---|---|
| `active`, `trialing` | pro | Paid, or in a trial |
| `past_due` | **pro** | The grace period (below) |
| `incomplete` | unchanged (free for a new subscriber) | The first payment is not done yet |
| `incomplete_expired`, `unpaid`, `canceled`, `paused` | free | Not paid |

Why both the lock and the full list:

- **Concurrent deliveries.** Stripe delivers events concurrently and out of order. Without the lock,
  a handler for `updated` could fetch `active`, then a handler for `deleted` fetch `canceled`, set
  free and commit, and the first handler commit `pro` last: a cancelled user left on pro, with no
  further event to fix it. With the row lock taken **before** the fetch, the handler that commits
  last is the one that fetched last, so the final plan always reflects Stripe's latest state. The
  lock is held during an API call (a second or so); per customer that is fine.
- **Order.** Because each sync reads the current state from Stripe, a late `updated` can never
  overwrite a newer `deleted`: it re-reads `canceled`. `created` has one-second resolution and is
  never used for ordering.
- **Several subscriptions.** A late `deleted` of an old subscription after a resubscription, or
  cancelling one of two live subscriptions, leaves the user pro, because another subscription still
  maps to pro.
- A cancellation "at period end" keeps the status `active` (with `cancel_at_period_end`) until the
  period ends, so the user stays pro until then.

### Grace period

A failed renewal makes the subscription `past_due`, and Stripe's Smart Retries retry the payment.
The user **stays pro while `past_due`**, so one declined card does not lock anyone out mid-program.
The grace period is Stripe's retry schedule: in the Stripe dashboard (Billing → Revenue recovery),
retry for up to 2 weeks, then **cancel the subscription**. That sends
`customer.subscription.deleted`, which makes the user free. The app has no timer of its own, so the
plan always follows Stripe's state. The account screen can show "payment failed, update your card"
while `past_due`.

### The webhook endpoint

`POST /webhooks/stripe` is mounted **outside the session and CSRF layers** (it is merged after
`auth::install`, see `server::router`). Stripe's deliveries have no `Origin`, no `Sec-Fetch-Site`
and no cookie, so the CSRF check would refuse them. Instead, every request is authenticated by its
signature. The body is read as raw bytes, at most **256 KiB** (`413` above that). Today the handler
stops there and answers **`501`**.

To implement (the TODO in `server/billing.rs`):

1. If `STRIPE_WEBHOOK_SECRET` is unset, answer `503` and log an error: never accept events without
   verifying them.
2. **Verify the signature.** Stripe sends
   `Stripe-Signature: t=1492774577,v1=5257a8…,v0=6ffbb5…` on one line.
   - Split on `,`, then each element on the first `=`. Take `t` (Unix seconds) and **every** `v1`.
     Ignore every other scheme, `v0` included, so no downgrade is possible. While a secret is being
     rolled (Stripe allows up to 24 hours), there is one `v1` per active secret.
   - Compute `HMAC-SHA256(key = the endpoint secret, message = "{t}.{raw body}")` and hex-encode it.
   - Compare it with each `v1` in **constant time** (`subtle`, already a dependency). One match is
     enough.
   - Use the **raw body bytes exactly as received**. Never parse or re-serialise the JSON before
     verifying.
3. **Replay window.** Refuse the event if `|now - t| > 300 s` (Stripe's default tolerance, 5
   minutes). `t` is covered by the signature, so it cannot be changed. Each retry by Stripe carries a
   fresh `t` and signature. The server clock must be NTP-synced (it is on Fly).
4. Answer **`400`** for a missing or malformed header, no matching signature, or a stale timestamp.
   Log the reason, never the body, the header or the secret.
5. Parse the event JSON. Only now is it trusted.
6. **Idempotency by event id.** In one transaction:
   `INSERT INTO stripe_events (event_id, event_type) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING event_id`.
   If no row comes back, the event was already processed: commit and answer `200` with no other
   effect. Otherwise apply the event (the table above: lock the customer's `billing_customers` row,
   then fetch and sync under that lock), then commit. A failure rolls back the event row too, so
   Stripe's retry processes it again.
7. Answer **`200`** quickly. A database failure gets `500`/`503`, and Stripe retries it (for up to 3
   days in live mode). An event for an unknown customer or a deleted user gets `200` and a warning,
   since a retry cannot fix it.

Test-mode and live-mode endpoints have **different secrets**, and so does `stripe listen`. The
secret is `STRIPE_WEBHOOK_SECRET`, optional today (the stub does not read it). The API key and
price ids come with the implementation as more optional variables. None of these values ever goes
in the repository: set them with `fly secrets import` in production and in `.env` locally.

**Do not register the endpoint in Stripe while it is a stub.** Stripe counts the `501`s as failed
deliveries, retries them for days, and eventually disables an endpoint that keeps failing.

Stripe also publishes its webhook source IP addresses. An IP allowlist is optional defence in depth
(behind Fly's proxy, read `Fly-Client-IP`). The signature is the real check.

### Account deletion (#22)

Deleting an account must first cancel its Stripe subscription through the API (immediately, no
refund logic in v1). Otherwise the user keeps being charged. `billing_customers` then goes with the
`users` cascade. Whether to also delete the Stripe customer (it holds the email and billing details
Stripe needs for accounting) is a maintainer decision to make with #22.

## Tests

### Today (#21)

| Test | Proves |
|---|---|
| `entitlements::tests::feature_matrix`, `quota_matrix` (domain) | The policy tables above, cell by cell |
| `pro_never_has_less_than_free`, `free_custom_programs_stop_at_the_limit`, `limit_boundaries` | Upgrading never removes anything; the cap boundaries (9 ok, 10 refused) |
| `wire_format_is_stable` | The JSON the UI receives |
| `server::entitlements::tests::*` (unit) | Refusals map to `403`; other failures to `401`/`500`/`503` without details |
| `flipping_the_plan_in_the_database_changes_the_entitlements` (Postgres) | The acceptance: `UPDATE users SET plan` flips `reserve_quota` and the entitlements, both ways |
| `two_concurrent_reservations_cannot_both_take_the_last_slot` (Postgres) | The quota recipe: the second concurrent reservation waits for the first one's row lock and is refused at the cap (10, never 11) |
| `only_unarchived_programs_count_and_a_downgrade_keeps_everything` (Postgres) | Archived programs do not count, unarchiving takes a slot, and a downgraded user over the cap keeps everything but can add or unarchive nothing |
| `the_entitlements_endpoint_follows_the_database_plan` (Postgres) | The same through the real router and a signed-in session |
| `two_users_each_get_their_own_plan`, `the_entitlements_endpoint_needs_a_session_and_a_same_origin_post` (Postgres) | Isolation, `401` signed out, CSRF still applies to the server function |
| `server::db::users::tests::*` (Postgres) | Every `user_plan` enum value parses; unknown user gives `None`; reads work inside a transaction; `lock_plan`; `unarchived_programs` counts only the user's own unarchived programs |
| `server::billing::tests::*` | The stub answers `501`; a Stripe-like cross-site `POST` is not blocked by CSRF and gets no session; the exemption is only that exact path; `413` above 256 KiB |
| `config::tests::stripe_webhook_secret_*` | The variable is optional and redacted from `Debug` |

### When Stripe is implemented

- **Signature (unit, no network)**, with a test secret generated in the test and a fixed clock: a
  valid signature is accepted; tampered body, tampered `t`, or the wrong secret is refused; a
  missing header, no `t`, a non-numeric `t`, or no `v1` is refused; a `v0`-only header is refused;
  two `v1` with one valid are accepted (secret rotation); `t` at exactly 300 s is accepted and 301 s
  is refused, in the past and in the future.
- **Handler (Postgres)**, with events signed by the test: `checkout.session.completed` then an
  `active` subscription makes the user pro; `past_due` keeps pro; `unpaid`, `canceled` and
  `customer.subscription.deleted` make them free; the same event id twice has one effect and both
  answers are `200`; an older `updated` arriving after `deleted` leaves the user free (the sync
  reads the current state); an unknown customer gets `200` and no change; a database failure gets
  a non-2xx and a retry succeeds; a customer id can never be linked to a second user.
- **Concurrency (Postgres, the fake Stripe API blocking on demand):** handler X (`updated`) fetches
  `active` and is held inside the fetch; the subscription is cancelled; handler Y (`deleted`) must
  wait on X's customer lock, then fetch `canceled`; release X. The final plan is free. (Without the
  lock taken before the fetch, this interleaving ends on pro: the test must fail if the lock is
  moved after the fetch.)
- **Several subscriptions:** an old subscription `canceled` plus a new one `active` gives pro,
  whatever order their events arrive in; two `active` gives pro, cancels the newer one and logs a
  warning; cancelling one of two live subscriptions keeps pro.
- The Stripe API is behind a trait, with a fake in tests. No test calls Stripe.
- **Manually, in test mode**: `stripe listen --forward-to localhost:8080/webhooks/stripe` (it prints
  a test secret for `.env`), then `stripe trigger checkout.session.completed`, `stripe trigger
  customer.subscription.updated` and `stripe trigger customer.subscription.deleted`, and check the
  plan with `my_entitlements()`.
