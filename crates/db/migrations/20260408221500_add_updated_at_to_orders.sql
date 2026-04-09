ALTER TABLE orders ADD COLUMN updated_at TIMESTAMPTZ;
UPDATE orders SET updated_at = created_at WHERE updated_at IS NULL;
ALTER TABLE orders ALTER COLUMN updated_at SET NOT NULL;
