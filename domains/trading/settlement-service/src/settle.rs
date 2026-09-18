// Applies engine events to Postgres. Each event runs in a single transaction.

use crate::accounting::{FillEffect, OrderSnapshot, apply_fill, close_status, split_pair};
use anyhow::{Context, anyhow};
use db::balances::{self, Balance, kind};
use db::orders::{self, DbOrderSide, DbOrderStatus, OrderFillState};
use db::trades::{self, Trade};
use kafka::trade::{OrderClosed, TradeExecuted};
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Order state after an event, for client notifications.
#[derive(Debug, Clone)]
pub struct OrderChange {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub status: DbOrderStatus,
    pub filled_qty: Decimal,
}

/// Everything that changed while applying one event.
#[derive(Debug, Default)]
pub struct Changes {
    pub balances: Vec<Balance>,
    pub orders: Vec<OrderChange>,
    pub trade: Option<TradeExecuted>,
}

fn snapshot(o: &OrderFillState) -> OrderSnapshot {
    OrderSnapshot {
        qty: o.qty_dec(),
        filled: o.filled_dec(),
        locked_remaining: o.locked_remaining,
    }
}

/// Moves the paid side of a fill out of the payer's balance:
/// reserved funds first, `available` for any shortfall, then releases the leftover reservation.
async fn pay(
    conn: &mut PgConnection,
    user_id: Uuid,
    asset: &str,
    effect: &FillEffect,
    trade_id: Uuid,
    order_id: Uuid,
    out: &mut Changes,
) -> anyhow::Result<()> {
    let b = balances::adjust(
        conn,
        user_id,
        asset,
        -effect.shortfall,
        -effect.lock_used,
        kind::TRADE,
        Some(trade_id),
    )
    .await?;
    out.balances.push(b);

    if effect.refund > Decimal::ZERO {
        let b = balances::adjust(
            conn,
            user_id,
            asset,
            effect.refund,
            -effect.refund,
            kind::UNLOCK,
            Some(order_id),
        )
        .await?;
        out.balances.push(b);
    }
    Ok(())
}

pub async fn settle_trade(pool: &PgPool, t: &TradeExecuted) -> anyhow::Result<Changes> {
    let price: Decimal = t.price.parse().context("bad trade price")?;
    let qty: Decimal = t.qty.parse().context("bad trade qty")?;
    let (base, quote) = split_pair(&t.pair).ok_or_else(|| anyhow!("bad pair {}", t.pair))?;
    let taker_side = match t.taker_side.as_str() {
        "BUY" => DbOrderSide::Buy,
        "SELL" => DbOrderSide::Sell,
        other => return Err(anyhow!("bad taker side {other}")),
    };
    let cost = price * qty;

    let mut tx = pool.begin().await?;

    let inserted = trades::insert_trade(
        &mut tx,
        &Trade {
            id: t.trade_id,
            pair: t.pair.clone(),
            price,
            qty,
            maker_order_id: t.maker_order_id,
            taker_order_id: t.taker_order_id,
            maker_user_id: t.maker_user_id,
            taker_user_id: t.taker_user_id,
            taker_side,
            executed_at: t.executed_at,
        },
    )
    .await?;

    let mut out = Changes::default();
    if !inserted {
        // duplicate delivery, already settled
        tx.commit().await?;
        return Ok(out);
    }

    let (buy_order_id, sell_order_id) = match taker_side {
        DbOrderSide::Buy => (t.taker_order_id, t.maker_order_id),
        DbOrderSide::Sell => (t.maker_order_id, t.taker_order_id),
    };

    // Lock rows in a stable order so concurrent settlements can't deadlock.
    let (first, second) = if buy_order_id < sell_order_id {
        (buy_order_id, sell_order_id)
    } else {
        (sell_order_id, buy_order_id)
    };
    let a = orders::get_fill_state_for_update(&mut tx, first)
        .await?
        .ok_or_else(|| anyhow!("order {first} not found"))?;
    let b = orders::get_fill_state_for_update(&mut tx, second)
        .await?
        .ok_or_else(|| anyhow!("order {second} not found"))?;
    let (buy, sell) = if a.id == buy_order_id { (a, b) } else { (b, a) };

    // buyer pays quote, receives base
    let buy_fx = apply_fill(snapshot(&buy), qty, cost);
    orders::set_fill_state(&mut tx, buy.id, buy_fx.new_filled, buy_fx.new_locked_remaining, buy_fx.status).await?;
    pay(&mut tx, buy.user_id, quote, &buy_fx, t.trade_id, buy.id, &mut out).await?;
    out.balances.push(
        balances::adjust(&mut tx, buy.user_id, base, qty, Decimal::ZERO, kind::TRADE, Some(t.trade_id)).await?,
    );

    // seller pays base, receives quote
    let sell_fx = apply_fill(snapshot(&sell), qty, qty);
    orders::set_fill_state(&mut tx, sell.id, sell_fx.new_filled, sell_fx.new_locked_remaining, sell_fx.status).await?;
    pay(&mut tx, sell.user_id, base, &sell_fx, t.trade_id, sell.id, &mut out).await?;
    out.balances.push(
        balances::adjust(&mut tx, sell.user_id, quote, cost, Decimal::ZERO, kind::TRADE, Some(t.trade_id)).await?,
    );

    if buy_fx.shortfall > Decimal::ZERO || sell_fx.shortfall > Decimal::ZERO {
        eprintln!(
            "trade {} settled with shortfall (buy {}, sell {})",
            t.trade_id, buy_fx.shortfall, sell_fx.shortfall
        );
    }

    tx.commit().await?;

    out.orders.push(OrderChange {
        order_id: buy.id,
        user_id: buy.user_id,
        pair: t.pair.clone(),
        status: buy_fx.status,
        filled_qty: buy_fx.new_filled,
    });
    out.orders.push(OrderChange {
        order_id: sell.id,
        user_id: sell.user_id,
        pair: t.pair.clone(),
        status: sell_fx.status,
        filled_qty: sell_fx.new_filled,
    });
    out.trade = Some(t.clone());
    Ok(out)
}

pub async fn settle_close(pool: &PgPool, c: &OrderClosed) -> anyhow::Result<Changes> {
    let mut tx = pool.begin().await?;
    let mut out = Changes::default();

    let Some(order) = orders::get_fill_state_for_update(&mut tx, c.order_id).await? else {
        return Err(anyhow!("order {} not found", c.order_id));
    };

    let (base, quote) = split_pair(&order.pair).ok_or_else(|| anyhow!("bad pair {}", order.pair))?;
    let locked_asset = match order.side {
        DbOrderSide::Buy => quote,
        DbOrderSide::Sell => base,
    };

    let refund = order.locked_remaining;
    let status = close_status(c.reason);

    // idempotent: second delivery finds locked_remaining = 0 and the same status
    orders::set_fill_state(&mut tx, order.id, order.filled_dec(), Decimal::ZERO, status).await?;
    if refund > Decimal::ZERO {
        out.balances.push(
            balances::adjust(&mut tx, order.user_id, locked_asset, refund, -refund, kind::UNLOCK, Some(order.id)).await?,
        );
    }
    tx.commit().await?;

    out.orders.push(OrderChange {
        order_id: order.id,
        user_id: order.user_id,
        pair: order.pair.clone(),
        status,
        filled_qty: order.filled_dec(),
    });
    Ok(out)
}
