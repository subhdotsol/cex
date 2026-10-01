use chrono::{DateTime, Utc};
use serde::Serialize;
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool, Type};
use types::{OrderSide, OrderStatus, OrderType};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Type, Serialize, PartialEq)]
#[sqlx(type_name = "order_side", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum DbOrderSide {
    Buy,
    Sell,
}

impl From<OrderSide> for DbOrderSide {
    fn from(side: OrderSide) -> Self {
        match side {
            OrderSide::Buy => DbOrderSide::Buy,
            OrderSide::Sell => DbOrderSide::Sell,
        }
    }
}

#[derive(Debug, Clone, Copy, Type, Serialize, PartialEq)]
#[sqlx(type_name = "order_type", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum DbOrderType {
    Limit,
    Market,
    Ioc,
    Fok,
}

impl From<OrderType> for DbOrderType {
    fn from(t: OrderType) -> Self {
        match t {
            OrderType::Limit => DbOrderType::Limit,
            OrderType::Market => DbOrderType::Market,
            OrderType::Ioc => DbOrderType::Ioc,
            OrderType::Fok => DbOrderType::Fok,
        }
    }
}

#[derive(Debug, Clone, Copy, Type, Serialize, PartialEq)]
#[sqlx(type_name = "order_status", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum DbOrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Rejected,
    Expired,
}

impl From<OrderStatus> for DbOrderStatus {
    fn from(s: OrderStatus) -> Self {
        match s {
            OrderStatus::Open => DbOrderStatus::Open,
            OrderStatus::Filled => DbOrderStatus::Filled,
            OrderStatus::PartiallyFilled => DbOrderStatus::PartiallyFilled,
            OrderStatus::Cancelled => DbOrderStatus::Cancelled,
            OrderStatus::Rejected => DbOrderStatus::Rejected,
            OrderStatus::Expired => DbOrderStatus::Expired,
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub side: DbOrderSide,
    pub order_type: DbOrderType,
    pub price: Option<String>,
    pub qty: String,
    pub filled_qty: String,
    pub status: DbOrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// get single order
pub async fn get_single_order(pool: &PgPool, order_id: Uuid) -> Result<Option<Order>, sqlx::Error> {
    let order = sqlx::query_as::<_, Order>(
        r#"
        SELECT id, user_id, pair, side, order_type, price, qty, status, filled_qty, created_at, updated_at
        FROM orders
        WHERE id = $1
        "#,
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await?;

    Ok(order)
}

// function to get all the orders of the specific users
pub async fn get_all_orders(
    pool: &PgPool,
    user_id: Uuid,
    limit: usize,
    status: Option<OrderStatus>,
    pair: Option<String>,
    before: Option<Uuid>,
) -> Result<Vec<Order>, sqlx::Error> {
    let mut query_builder = sqlx::QueryBuilder::new(
        "SELECT id, user_id, pair, side, order_type, price, qty, status, filled_qty, created_at, updated_at FROM orders WHERE user_id = ",
    );
    query_builder.push_bind(user_id);

    if let Some(status) = status {
        query_builder.push(" AND status = ");
        query_builder.push_bind(DbOrderStatus::from(status));
    }

    if let Some(pair) = pair {
        query_builder.push(" AND pair = ");
        query_builder.push_bind(pair);
    }

    if let Some(before) = before {
        query_builder.push(" AND id < ");
        query_builder.push_bind(before);
    }

    query_builder.push(" ORDER BY id DESC LIMIT ");
    query_builder.push_bind(limit as i64);

    let orders = query_builder
        .build_query_as::<Order>()
        .fetch_all(pool)
        .await?;

    Ok(orders)
}

// function to update order status
pub async fn update_order_status(
    pool: &PgPool,
    order_id: Uuid,
    status: DbOrderStatus,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    sqlx::query(
        r#"
        UPDATE orders SET status = $1, updated_at = $2 WHERE id = $3
        "#,
    )
    .bind(status)
    .bind(now)
    .bind(order_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Inserts an order together with the amount of funds reserved for it.
/// Meant to run in the same transaction as `balances::lock`.
pub async fn create_order_with_lock(
    conn: &mut PgConnection,
    id: Uuid,
    user_id: Uuid,
    pair: &str,
    side: OrderSide,
    order_type: OrderType,
    price: Option<&str>,
    qty: &str,
    locked: Decimal,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    sqlx::query(
        r#"
        INSERT INTO orders (id, user_id, pair, side, order_type, price, qty, status,
                            locked_remaining, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(pair)
    .bind(DbOrderSide::from(side))
    .bind(DbOrderType::from(order_type))
    .bind(price)
    .bind(qty)
    .bind(DbOrderStatus::Open)
    .bind(locked)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// Fill-relevant state of an order, read with a row lock.
#[derive(Debug, sqlx::FromRow)]
pub struct OrderFillState {
    pub id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub side: DbOrderSide,
    pub qty: String,
    pub filled_qty: String,
    pub status: DbOrderStatus,
    pub locked_remaining: Decimal,
}

impl OrderFillState {
    pub fn qty_dec(&self) -> Decimal {
        self.qty.parse().unwrap_or(Decimal::ZERO)
    }
    pub fn filled_dec(&self) -> Decimal {
        self.filled_qty.parse().unwrap_or(Decimal::ZERO)
    }
}

pub async fn get_fill_state_for_update(
    conn: &mut PgConnection,
    order_id: Uuid,
) -> Result<Option<OrderFillState>, sqlx::Error> {
    sqlx::query_as::<_, OrderFillState>(
        r#"
        SELECT id, user_id, pair, side, qty, filled_qty, status, locked_remaining
        FROM orders WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(order_id)
    .fetch_optional(conn)
    .await
}

pub async fn set_fill_state(
    conn: &mut PgConnection,
    order_id: Uuid,
    filled_qty: Decimal,
    locked_remaining: Decimal,
    status: DbOrderStatus,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE orders
        SET filled_qty = $2, locked_remaining = $3, status = $4, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(order_id)
    .bind(filled_qty.normalize().to_string())
    .bind(locked_remaining)
    .bind(status)
    .execute(conn)
    .await?;
    Ok(())
}

impl From<DbOrderStatus> for OrderStatus {
    fn from(s: DbOrderStatus) -> Self {
        match s {
            DbOrderStatus::Open => OrderStatus::Open,
            DbOrderStatus::Filled => OrderStatus::Filled,
            DbOrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
            DbOrderStatus::Cancelled => OrderStatus::Cancelled,
            DbOrderStatus::Rejected => OrderStatus::Rejected,
            DbOrderStatus::Expired => OrderStatus::Expired,
        }
    }
}

impl From<DbOrderSide> for OrderSide {
    fn from(s: DbOrderSide) -> Self {
        match s {
            DbOrderSide::Buy => OrderSide::Buy,
            DbOrderSide::Sell => OrderSide::Sell,
        }
    }
}
