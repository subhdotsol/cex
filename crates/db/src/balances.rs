// Balance + ledger helpers. All mutations take a connection so callers can
// run them inside their own transaction together with other writes.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Balance {
    pub user_id: Uuid,
    pub asset: String,
    pub available: Decimal,
    pub locked: Decimal,
    pub updated_at: DateTime<Utc>,
}

pub mod kind {
    pub const DEPOSIT: &str = "DEPOSIT";
    pub const WITHDRAW: &str = "WITHDRAW";
    pub const LOCK: &str = "LOCK";
    pub const UNLOCK: &str = "UNLOCK";
    pub const TRADE: &str = "TRADE";
}

async fn ledger(
    conn: &mut PgConnection,
    user_id: Uuid,
    asset: &str,
    available_delta: Decimal,
    locked_delta: Decimal,
    kind: &str,
    ref_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO ledger_entries (user_id, asset, available_delta, locked_delta, kind, ref_id)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(user_id)
    .bind(asset)
    .bind(available_delta)
    .bind(locked_delta)
    .bind(kind)
    .bind(ref_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Unconditionally applies deltas (creating the row if needed) and records a ledger entry.
pub async fn adjust(
    conn: &mut PgConnection,
    user_id: Uuid,
    asset: &str,
    available_delta: Decimal,
    locked_delta: Decimal,
    kind: &str,
    ref_id: Option<Uuid>,
) -> Result<Balance, sqlx::Error> {
    let balance = sqlx::query_as::<_, Balance>(
        r#"
        INSERT INTO balances (user_id, asset, available, locked, updated_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (user_id, asset) DO UPDATE
        SET available = balances.available + EXCLUDED.available,
            locked = balances.locked + EXCLUDED.locked,
            updated_at = NOW()
        RETURNING user_id, asset, available, locked, updated_at
        "#,
    )
    .bind(user_id)
    .bind(asset)
    .bind(available_delta)
    .bind(locked_delta)
    .fetch_one(&mut *conn)
    .await?;

    ledger(conn, user_id, asset, available_delta, locked_delta, kind, ref_id).await?;
    Ok(balance)
}

/// Moves `amount` from available to locked only if enough is available.
/// Returns None when the balance is insufficient.
pub async fn lock(
    conn: &mut PgConnection,
    user_id: Uuid,
    asset: &str,
    amount: Decimal,
    order_id: Uuid,
) -> Result<Option<Balance>, sqlx::Error> {
    let balance = sqlx::query_as::<_, Balance>(
        r#"
        UPDATE balances
        SET available = available - $3, locked = locked + $3, updated_at = NOW()
        WHERE user_id = $1 AND asset = $2 AND available >= $3
        RETURNING user_id, asset, available, locked, updated_at
        "#,
    )
    .bind(user_id)
    .bind(asset)
    .bind(amount)
    .fetch_optional(&mut *conn)
    .await?;

    if balance.is_some() {
        ledger(conn, user_id, asset, -amount, amount, kind::LOCK, Some(order_id)).await?;
    }
    Ok(balance)
}

/// Debits available only if enough is available. Returns None when insufficient.
pub async fn withdraw(
    conn: &mut PgConnection,
    user_id: Uuid,
    asset: &str,
    amount: Decimal,
) -> Result<Option<Balance>, sqlx::Error> {
    let balance = sqlx::query_as::<_, Balance>(
        r#"
        UPDATE balances
        SET available = available - $3, updated_at = NOW()
        WHERE user_id = $1 AND asset = $2 AND available >= $3
        RETURNING user_id, asset, available, locked, updated_at
        "#,
    )
    .bind(user_id)
    .bind(asset)
    .bind(amount)
    .fetch_optional(&mut *conn)
    .await?;

    if balance.is_some() {
        ledger(conn, user_id, asset, -amount, Decimal::ZERO, kind::WITHDRAW, None).await?;
    }
    Ok(balance)
}

pub async fn get_balances(pool: &PgPool, user_id: Uuid) -> Result<Vec<Balance>, sqlx::Error> {
    sqlx::query_as::<_, Balance>(
        r#"
        SELECT user_id, asset, available, locked, updated_at
        FROM balances WHERE user_id = $1 ORDER BY asset
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}
