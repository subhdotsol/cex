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
