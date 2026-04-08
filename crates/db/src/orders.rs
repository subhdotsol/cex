use chrono::{DateTime, Utc};
use sqlx::{PgPool, Type};
use uuid::Uuid;
use types::{OrderSide, OrderType, OrderStatus};

#[derive(Debug, Type)]
#[sqlx(type_name = "order_side", rename_all = "UPPERCASE")]
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

#[derive(Debug, Type)]
#[sqlx(type_name = "order_type", rename_all = "UPPERCASE")]
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

#[derive(Debug, Type)]
#[sqlx(type_name = "order_status", rename_all = "UPPERCASE")]
pub enum DbOrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Rejected,
}

impl From<OrderStatus> for DbOrderStatus {
    fn from(s: OrderStatus) -> Self {
        match s {
            OrderStatus::Open => DbOrderStatus::Open,
            OrderStatus::Filled => DbOrderStatus::Filled,
            OrderStatus::PartiallyFilled => DbOrderStatus::PartiallyFilled,
            OrderStatus::Cancelled => DbOrderStatus::Cancelled,
            OrderStatus::Rejected => DbOrderStatus::Rejected,
        }
    }
}

pub async fn create_order(
    pool: &PgPool,
    id: Uuid,
    user_id: Uuid,
    pair: &str,
    side: OrderSide,
    order_type: OrderType,
    price: Option<&str>,
    qty: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO orders (id, user_id, pair, side, order_type, price, qty, status, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
    .bind(Utc::now())
    .execute(pool)
    .await?;

    Ok(())
}
