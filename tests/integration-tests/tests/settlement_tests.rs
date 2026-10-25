// Settlement against a real Postgres (DATABASE_URL, migrations applied).
// Exercises the money paths: lock -> fill -> refund, cancel release, duplicate delivery.

use chrono::Utc;
use db::balances;
use db::orders::{self, DbOrderStatus};
use kafka::trade::{CloseReason, OrderClosed, TradeExecuted};
use rust_decimal::Decimal;
use settlement_service::settle;
use sqlx::PgPool;
use std::env;
use types::{OrderSide, OrderType};
use uuid::Uuid;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

async fn pool() -> PgPool {
    dotenvy::dotenv().ok();
    let url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    db::connection_pool(&url).await.expect("postgres")
}

async fn user_with(pool: &PgPool, asset: &str, amount: &str) -> Uuid {
    let email = format!("settle-{}@example.com", Uuid::new_v4());
    db::users::create_user(pool, &email, "x").await.unwrap();
    let id = db::users::get_user_by_email(pool, &email).await.unwrap().unwrap().id;
    let mut conn = pool.acquire().await.unwrap();
    balances::adjust(&mut conn, id, asset, d(amount), Decimal::ZERO, balances::kind::DEPOSIT, None)
        .await
        .unwrap();
    id
}

/// Same path as the order service: lock funds + insert order in one transaction.
async fn place(
    pool: &PgPool,
    user: Uuid,
    side: OrderSide,
    price: &str,
    qty: &str,
    lock_asset: &str,
    lock: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    balances::lock(&mut tx, user, lock_asset, d(lock), id).await.unwrap().expect("enough balance");
    orders::create_order_with_lock(&mut tx, id, user, "BTC-USDT", side, OrderType::Limit, Some(price), qty, d(lock))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn balance(pool: &PgPool, user: Uuid, asset: &str) -> (Decimal, Decimal) {
    balances::get_balances(pool, user)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.asset == asset)
        .map(|b| (b.available, b.locked))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO))
}

async fn status(pool: &PgPool, order: Uuid) -> (DbOrderStatus, String) {
    let o = orders::get_single_order(pool, order).await.unwrap().unwrap();
    (o.status, o.filled_qty)
}

fn trade(maker: (Uuid, Uuid), taker: (Uuid, Uuid), taker_side: &str, price: &str, qty: &str) -> TradeExecuted {
    TradeExecuted {
        trade_id: Uuid::new_v4(),
        pair: "BTC-USDT".into(),
        price: price.into(),
        qty: qty.into(),
        maker_order_id: maker.0,
        taker_order_id: taker.0,
        maker_user_id: maker.1,
        taker_user_id: taker.1,
        taker_side: taker_side.into(),
        executed_at: Utc::now(),
    }
}

#[tokio::test]
async fn fill_moves_funds_and_refunds_price_improvement() {
    let pool = pool().await;
    let seller = user_with(&pool, "BTC", "1").await;
    let buyer = user_with(&pool, "USDT", "1000").await;

    let ask = place(&pool, seller, OrderSide::Sell, "100", "1", "BTC", "1").await;
    // buyer bids 105 for 1 -> 105 locked, fills at the maker's 100
    let bid = place(&pool, buyer, OrderSide::Buy, "105", "1", "USDT", "105").await;

    let t = trade((ask, seller), (bid, buyer), "BUY", "100", "1");
    settle::settle_trade(&pool, &t).await.unwrap();

    assert_eq!(balance(&pool, buyer, "USDT").await, (d("900"), d("0")), "paid 100, 5 refunded");
    assert_eq!(balance(&pool, buyer, "BTC").await, (d("1"), d("0")));
    assert_eq!(balance(&pool, seller, "BTC").await, (d("0"), d("0")));
    assert_eq!(balance(&pool, seller, "USDT").await, (d("100"), d("0")));
    assert_eq!(status(&pool, ask).await.0, DbOrderStatus::Filled);
    assert_eq!(status(&pool, bid).await.0, DbOrderStatus::Filled);

    // duplicate delivery of the same trade changes nothing
    settle::settle_trade(&pool, &t).await.unwrap();
    assert_eq!(balance(&pool, buyer, "USDT").await, (d("900"), d("0")));
    assert_eq!(balance(&pool, seller, "USDT").await, (d("100"), d("0")));
}

#[tokio::test]
async fn partial_fill_then_cancel_releases_the_rest() {
    let pool = pool().await;
    let seller = user_with(&pool, "BTC", "2").await;
    let buyer = user_with(&pool, "USDT", "1000").await;

    let ask = place(&pool, seller, OrderSide::Sell, "200", "2", "BTC", "2").await;
    let bid = place(&pool, buyer, OrderSide::Buy, "200", "0.5", "USDT", "100").await;

    // buyer is the taker and fully fills, seller is left with 1.5 resting
    settle::settle_trade(&pool, &trade((ask, seller), (bid, buyer), "BUY", "200", "0.5"))
        .await
        .unwrap();
    assert_eq!(status(&pool, ask).await, (DbOrderStatus::PartiallyFilled, "0.5".into()));
    assert_eq!(balance(&pool, seller, "BTC").await, (d("0"), d("1.5")));

    let close = OrderClosed {
        order_id: ask,
        user_id: seller,
        pair: "BTC-USDT".into(),
        reason: CloseReason::Cancelled,
        remaining_qty: "1.5".into(),
        closed_at: Utc::now(),
    };
    settle::settle_close(&pool, &close).await.unwrap();
    settle::settle_close(&pool, &close).await.unwrap(); // idempotent

    assert_eq!(status(&pool, ask).await, (DbOrderStatus::Cancelled, "0.5".into()));
    assert_eq!(balance(&pool, seller, "BTC").await, (d("1.5"), d("0")));
    assert_eq!(balance(&pool, seller, "USDT").await, (d("100"), d("0")));
}

#[tokio::test]
async fn lock_rejects_overspend() {
    let pool = pool().await;
    let user = user_with(&pool, "USDT", "50").await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(balances::lock(&mut conn, user, "USDT", d("50.01"), Uuid::new_v4()).await.unwrap().is_none());
    assert!(balances::lock(&mut conn, user, "USDT", d("50"), Uuid::new_v4()).await.unwrap().is_some());
    assert_eq!(balance(&pool, user, "USDT").await, (d("0"), d("50")));
}
