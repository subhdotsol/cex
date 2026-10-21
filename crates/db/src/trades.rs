use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::orders::DbOrderSide;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Trade {
    pub id: Uuid,
    pub pair: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub maker_order_id: Uuid,
    pub taker_order_id: Uuid,
    pub maker_user_id: Uuid,
    pub taker_user_id: Uuid,
    pub taker_side: DbOrderSide,
    pub executed_at: DateTime<Utc>,
}

/// Inserts a trade. Returns false if it was already recorded (duplicate delivery).
pub async fn insert_trade(conn: &mut PgConnection, t: &Trade) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        r#"
        INSERT INTO trades (id, pair, price, qty, maker_order_id, taker_order_id,
                            maker_user_id, taker_user_id, taker_side, executed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(t.id)
    .bind(&t.pair)
    .bind(t.price)
    .bind(t.qty)
    .bind(t.maker_order_id)
    .bind(t.taker_order_id)
    .bind(t.maker_user_id)
    .bind(t.taker_user_id)
    .bind(&t.taker_side)
    .bind(t.executed_at)
    .execute(conn)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Trades where the user was maker or taker, newest first.
pub async fn get_user_trades(
    pool: &PgPool,
    user_id: Uuid,
    pair: Option<&str>,
    limit: i64,
) -> Result<Vec<Trade>, sqlx::Error> {
    sqlx::query_as::<_, Trade>(
        r#"
        SELECT id, pair, price, qty, maker_order_id, taker_order_id,
               maker_user_id, taker_user_id, taker_side, executed_at
        FROM trades
        WHERE (maker_user_id = $1 OR taker_user_id = $1)
          AND ($2::text IS NULL OR pair = $2)
        ORDER BY executed_at DESC
        LIMIT $3
        "#,
    )
    .bind(user_id)
    .bind(pair)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Candle {
    pub open_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub trades: i64,
}

/// Supported candle intervals: api name -> postgres interval.
pub fn candle_interval(name: &str) -> Option<&'static str> {
    Some(match name {
        "1m" => "1 minute",
        "5m" => "5 minutes",
        "15m" => "15 minutes",
        "1h" => "1 hour",
        "4h" => "4 hours",
        "1d" => "1 day",
        _ => return None,
    })
}

/// OHLCV buckets computed from executed trades, newest first. Buckets without trades are omitted.
pub async fn get_candles(
    pool: &PgPool,
    pair: &str,
    pg_interval: &str,
    limit: i64,
) -> Result<Vec<Candle>, sqlx::Error> {
    sqlx::query_as::<_, Candle>(
        r#"
        SELECT date_bin($2::interval, executed_at, TIMESTAMPTZ '2000-01-01 00:00:00+00') AS open_time,
               (array_agg(price ORDER BY executed_at ASC, id ASC))[1]   AS open,
               MAX(price)                                               AS high,
               MIN(price)                                               AS low,
               (array_agg(price ORDER BY executed_at DESC, id DESC))[1] AS close,
               SUM(qty)                                                 AS volume,
               COUNT(*)                                                 AS trades
        FROM trades
        WHERE pair = $1
          AND executed_at >= date_bin($2::interval, NOW(), TIMESTAMPTZ '2000-01-01 00:00:00+00')
                             - ($2::interval * ($3 - 1))
        GROUP BY open_time
        ORDER BY open_time DESC
        LIMIT $3
        "#,
    )
    .bind(pair)
    .bind(pg_interval)
    .bind(limit)
    .fetch_all(pool)
    .await
}
