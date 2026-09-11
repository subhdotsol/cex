-- Balances: source of truth for user funds. Redis only caches `available`.
CREATE TABLE balances (
    user_id UUID NOT NULL REFERENCES users(id),
    asset TEXT NOT NULL,
    available NUMERIC(38, 18) NOT NULL DEFAULT 0,
    locked NUMERIC(38, 18) NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, asset)
);

-- Every executed fill. Primary key on trade id makes settlement idempotent
-- (kafka is at-least-once, the same trade may be delivered twice).
CREATE TABLE trades (
    id UUID PRIMARY KEY,
    pair TEXT NOT NULL,
    price NUMERIC(38, 18) NOT NULL,
    qty NUMERIC(38, 18) NOT NULL,
    maker_order_id UUID NOT NULL REFERENCES orders(id),
    taker_order_id UUID NOT NULL REFERENCES orders(id),
    maker_user_id UUID NOT NULL REFERENCES users(id),
    taker_user_id UUID NOT NULL REFERENCES users(id),
    taker_side order_side NOT NULL,
    executed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX trades_pair_executed_at_idx ON trades (pair, executed_at DESC);
CREATE INDEX trades_maker_user_idx ON trades (maker_user_id, executed_at DESC);
CREATE INDEX trades_taker_user_idx ON trades (taker_user_id, executed_at DESC);

-- Append-only audit log of every balance movement.
CREATE TABLE ledger_entries (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    asset TEXT NOT NULL,
    -- signed change of `available`
    available_delta NUMERIC(38, 18) NOT NULL,
    -- signed change of `locked`
    locked_delta NUMERIC(38, 18) NOT NULL,
    kind TEXT NOT NULL, -- DEPOSIT, WITHDRAW, LOCK, UNLOCK, TRADE
    ref_id UUID, -- order id / trade id that caused it
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX ledger_entries_user_idx ON ledger_entries (user_id, created_at DESC);

-- Funds still reserved for an order (quote for buys, base for sells).
-- Consumed by fills, released when the order is filled / cancelled / expired.
ALTER TABLE orders ADD COLUMN locked_remaining NUMERIC(38, 18) NOT NULL DEFAULT 0;

CREATE INDEX orders_user_id_idx ON orders (user_id, id DESC);
