
CREATE TYPE order_side AS ENUM ('BUY', 'SELL');
CREATE TYPE order_type AS ENUM ('LIMIT', 'MARKET', 'IOC', 'FOK');
CREATE TYPE order_status AS ENUM ('OPEN', 'FILLED', 'PARTIALLY_FILLED', 'CANCELLED', 'REJECTED');

CREATE TABLE orders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    pair TEXT NOT NULL,
    side order_side NOT NULL,
    order_type order_type NOT NULL,
    price TEXT, -- Storing as text to preserve precision, or could use NUMERIC
    qty TEXT NOT NULL,
    filled_qty TEXT NOT NULL DEFAULT '0',
    status order_status NOT NULL DEFAULT 'OPEN',
    created_at TIMESTAMPTZ NOT NULL
);