# Architecture & Production Readiness

Notes on what this system is, what's built, and what's left before it could run with real money.
Last updated: 2026-11-13.

---

## 1. What kind of architecture is this?

**Event-driven microservices**, which also makes it a **distributed system**. These are not competing labels:

- **Distributed system**: the broad category. Several processes on a network work together and
  coordinate through messages (Kafka, Redis, Postgres). Anything split across processes is distributed.
- **Microservices**: a specific style of distributed system. Separately deployable services, each with one job
  (auth, orders, wallet, matching, settlement, websocket gateway).
- **Event-driven**: how they talk. The core flow goes through Kafka events, not direct calls between services.

One-line description:

> An event-driven microservices exchange with a shared transactional database and a single-writer,
> in-memory matching engine.

Real exchanges (Coinbase, Binance, LMAX) have broadly this shape.

### Patterns used

| Pattern | Where |
|---|---|
| Event sourcing (partial) | Matching engine state is rebuilt by replaying the `orders.commands` log |
| CQRS-ish split | Writes go through Kafka to settlement, reads come from Postgres / Redis |
| Single-writer | One matching engine owns each order book, so matching needs no locks |
| Idempotent consumers | Settlement deduplicates trades by deterministic trade ID, so at-least-once delivery is safe |
| Pub/sub fan-out | Redis pub/sub feeds the stateless websocket gateways |

### Where it isn't "pure" microservices

- **Shared database.** Order, wallet and settlement all read and write the same Postgres tables.
  Strict microservices give each service its own database. For an exchange this is a deliberate trade-off,
  because locking funds and inserting an order must happen in one transaction. It is still coupling.
- **Risk engine is a library, not a service.** It runs inside the order service. Faster, but it can't
  scale or deploy on its own.
- **One Cargo workspace.** Common and fine.

### Services

| Service | Port | Responsibility |
|---|---|---|
| auth-service | 8080 | register / login / refresh / logout, API key management |
| order-service | 8081 | place / get / list / cancel orders, trade history, candles |
| wallet-service | 8082 | mock deposit / withdraw, balances |
| matching-engine | 8083 | in-memory order books, depth / recent trades / ticker |
| settlement-service | - | applies fills to Postgres, releases locked funds |
| ws-gateway | 8085 | realtime depth, trades, balances and order updates |
| risk-engine | (library) | pre-trade checks used by order-service |

### Order lifecycle

1. `POST /orders`: the risk engine validates the order and computes the funds to reserve.
2. Funds are locked and the order inserted in **one Postgres transaction**.
3. The order goes to Kafka `orders.commands`, keyed by pair. Cancels use the same topic, so they can't overtake their order.
4. The matching engine matches it and emits `TRADE` / `ORDER_CLOSED` events to `engine.events`.
5. The settlement service records trades, updates orders, moves funds, refunds unused reservations and writes the ledger.
6. Redis is refreshed and pub/sub feeds the websocket gateway.

---

## 2. Is it production-ready?

**No.** The architecture is production-shaped and the core money logic is careful, but a lot of operational
work is missing. It's a strong portfolio / learning project, not something to run with real money.

### Already solid

- Exact decimal balances (`NUMERIC`). Locking funds and inserting the order happen in one transaction, so a balance can't be overspent.
- Ledger table (`ledger_entries`) records every balance movement.
- Idempotent settlement. Kafka offsets are committed only after the database commit.
- Cancels can't overtake their order (one topic, keyed by pair).
- Order book rebuilt by replay after an engine restart, with deterministic trade IDs.
- HMAC API keys: replay protection (30s timestamp window), constant-time comparison, AES-256-GCM encrypted secrets.
- Rate limiting (Redis, per API key / user / IP).
- Integration tests on the money paths; CI on GitHub Actions (clippy + tests) passing.

### Blocking production

| Area | Gap |
|---|---|
| Matching engine availability | One instance, no hot standby. A crash stops trading until replay finishes, and replay starts from offset 0 because there are no snapshots. |
| Exactly-once output | A crash between producing events and committing the offset can emit duplicates. Settlement deduplicates trades, but the window isn't closed with Kafka transactions. |
| Numeric precision | The engine matches with `f64`, which can drift on some values. It should use integer ticks or decimals. Settlement already uses exact decimals. |
| Wallet | Deposits / withdrawals are fake. Needs chain integration, confirmations, withdrawal approval, hot/cold wallets. |
| Reconciliation | No job checks that balances equal the sum of ledger entries, or that the engine's book matches Postgres. |
| Security | No 2FA, no TLS termination, no withdrawal allowlists, no secret manager, no admin audit. `.expect()` panics can crash a service. |
| Observability | `println!` logs only. No metrics, alerts, tracing or settlement-lag monitoring. |
| Infrastructure | Single-node Kafka / Postgres / Redis: no replication, backups or failover. No Kubernetes setup. |
| Testing | No load tests, chaos tests or full end-to-end suite in CI. Throughput and latency not measured. |
| Compliance | No KYC / AML flow, which is legally required to run an exchange in most countries. |

---

## 3. What's left

### Not built yet

| Item | Size | Notes |
|---|---|---|
| Notification service (email / push) | Medium | Needs an email or push provider. The websocket gateway already covers in-app updates. |
| Web UI (Next.js) | Large | No frontend exists. |
| Kubernetes / Helm / Argo CD | Medium | Only Docker Compose for now. |
| Observability (Prometheus, Grafana, Loki, Jaeger) | Medium | No metrics or tracing. |
| Trading fees | Small | Settlement charges no maker/taker fees. |
| Real deposits and withdrawals | Large | The wallet just credits and debits; nothing is connected to a blockchain. |

### Gaps / known bugs in what's built

- [ ] **Matching-engine snapshots.** Replay starts from offset 0, which gets slow over time and breaks once Kafka deletes old messages.
- [ ] **`f64` inside the matching engine.** Switch to integer ticks / lots.
- [ ] **Order list pagination** sorts by random UUIDs (`ORDER BY id DESC`), so pages aren't in time order. Use `created_at` + id as the cursor.
- [ ] **`POST /auth/register` returns the wrong `user_id`.** The handler generates its own UUID instead of returning the one `db::users::create_user` saved.
- [ ] Old clippy warnings (`too_many_arguments`, complex type in `state.rs`, unused `maker_fully_filled` / `timestamp` fields, unused `redis` dep in auth-service).

---

## 4. Suggested order of work toward production

1. **Correctness:** trading fees, the register `user_id` bug, the pagination cursor, integer prices in the engine.
2. **Resilience:** matching-engine snapshots plus a standby engine; Kafka transactions for engine output.
3. **Visibility:** reconciliation jobs (ledger vs balances, engine book vs DB), Prometheus metrics, settlement-lag alerts.
4. **Infrastructure:** replicated Kafka / Postgres / Redis with backups, deployed on Kubernetes with Helm.
5. **Product / security:** real wallet integration, withdrawal security, 2FA, notification service, web UI.
6. **Launch-only:** load testing, security audit, KYC / AML compliance.

Steps 1–3 are the most useful engineering work. Steps 5–6 matter only if you plan to launch.

---

## 5. Running locally (quick reference)

```bash
cp .env.example .env    # set TOKEN_SECRET and API_KEY_ENCRYPTION_KEY (openssl rand -hex 32)
make docker-up          # postgres, redis, kafka
make migrate            # sqlx migrate run
make topics             # create kafka topics. Run BEFORE starting services, or the engine misses them
make cex                # start every service
cargo test --workspace  # unit + integration tests (needs postgres + redis up)
```
