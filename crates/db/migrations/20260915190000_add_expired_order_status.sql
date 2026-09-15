-- IOC / MARKET orders whose unfilled remainder was dropped by the matching engine
ALTER TYPE order_status ADD VALUE IF NOT EXISTS 'EXPIRED';
