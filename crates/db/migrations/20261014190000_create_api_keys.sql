-- API keys for programmatic (bot) access, authenticated with HMAC-SHA256 request signatures.
-- The secret must be recoverable to recompute signatures, so it is stored encrypted
-- (AES-256-GCM, key from API_KEY_ENCRYPTION_KEY), never in plain text and never returned again.
CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    key TEXT UNIQUE NOT NULL,
    secret_ciphertext BYTEA NOT NULL,
    secret_nonce BYTEA NOT NULL,
    label TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX api_keys_user_idx ON api_keys (user_id);
