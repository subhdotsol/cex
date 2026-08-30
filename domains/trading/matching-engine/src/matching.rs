use crate::state::{BookOrder, OrderBook, Side};
use chrono::{DateTime, Utc};
use ordered_float::NotNan;
use std::cmp::Reverse;
use uuid::Uuid;

#[derive(Debug)]
pub struct TradeResult {
    pub trade_id: Uuid,
    pub maker_order_id: Uuid,
    pub taker_order_id: Uuid,
    pub maker_user_id: Uuid,
    pub taker_user_id: Uuid,
    pub pair: String,
    pub price: f64,
    pub qty: f64,
    pub maker_fully_filled: bool,
    pub taker_side: Side,
    pub executed_at: DateTime<Utc>,
}

fn available_liquidity(book: &OrderBook, side: &Side, limit: Option<f64>) -> f64 {
    match side {
        Side::Buy => {
            let cap = limit.unwrap_or(f64::MAX);
            book.asks
                .iter()
                .take_while(|(p, _)| (*p).into_inner() <= cap)
                .flat_map(|(_, level)| level.iter())
                .map(|o| o.remaining_qty)
                .sum()
        }
        Side::Sell => {
            let floor = limit.unwrap_or(0.0);
            book.bids
                .iter()
                .take_while(|(Reverse(p), _)| (*p).into_inner() >= floor)
                .flat_map(|(_, level)| level.iter())
                .map(|o| o.remaining_qty)
                .sum()
        }
    }
}

pub fn match_order(
    book: &mut OrderBook,
    order_id: Uuid,
    user_id: Uuid,
    pair: &str,
    side: Side,
    order_type: &str,
    price: Option<f64>,
    qty: f64,
    timestamp: DateTime<Utc>,
) -> Vec<TradeResult> {
    // FOK: cancel entirely if full quantity can't be filled immediately
    if order_type == "FOK" {
        let available = available_liquidity(book, &side, price);
        if available < qty - 1e-10 {
            return vec![];
        }
    }

    let mut trades = Vec::new();
    let mut remaining = qty;
    let rest_in_book = order_type == "LIMIT";

    match side {
        Side::Buy => {
            let limit_price = price.unwrap_or(f64::MAX);
            loop {
                if remaining <= 1e-10 {
                    break;
                }
                let best_ask_key = match book.asks.keys().next().copied() {
                    Some(k) => k,
                    None => break,
                };
                if best_ask_key.into_inner() > limit_price {
                    break;
                }
                let level = book.asks.get_mut(&best_ask_key).unwrap();
                loop {
                    if remaining <= 1e-10 {
                        break;
                    }
                    let maker = match level.front_mut() {
                        Some(m) => m,
                        None => break,
                    };
                    let fill_qty = remaining.min(maker.remaining_qty);
                    maker.remaining_qty -= fill_qty;
                    remaining -= fill_qty;
                    let maker_fully_filled = maker.remaining_qty <= 1e-10;
                    trades.push(TradeResult {
                        trade_id: Uuid::new_v4(),
                        maker_order_id: maker.order_id,
                        taker_order_id: order_id,
                        maker_user_id: maker.user_id,
                        taker_user_id: user_id,
                        pair: pair.to_string(),
                        price: best_ask_key.into_inner(),
                        qty: fill_qty,
                        maker_fully_filled,
                        taker_side: Side::Buy,
                        executed_at: Utc::now(),
                    });
                    if maker_fully_filled {
                        let mid = maker.order_id;
                        level.pop_front();
                        book.index.remove(&mid);
                    } else {
                        break;
                    }
                }
                if level.is_empty() {
                    book.asks.remove(&best_ask_key);
                }
            }
            if rest_in_book && remaining > 1e-10 {
                let p = price.unwrap();
                let key = Reverse(NotNan::new(p).unwrap());
                book.bids
                    .entry(key)
                    .or_default()
                    .push_back(BookOrder { order_id, user_id, remaining_qty: remaining, timestamp });
                book.index.insert(order_id, (Side::Buy, p));
            }
        }
        Side::Sell => {
            let limit_price = price.unwrap_or(0.0);
            loop {
                if remaining <= 1e-10 {
                    break;
                }
                let best_bid_key = match book.bids.keys().next().copied() {
                    Some(k) => k,
                    None => break,
                };
                let Reverse(best_bid_price_nn) = best_bid_key;
                if best_bid_price_nn.into_inner() < limit_price {
                    break;
                }
                let level = book.bids.get_mut(&best_bid_key).unwrap();
                loop {
                    if remaining <= 1e-10 {
                        break;
                    }
                    let maker = match level.front_mut() {
                        Some(m) => m,
                        None => break,
                    };
                    let fill_qty = remaining.min(maker.remaining_qty);
                    maker.remaining_qty -= fill_qty;
                    remaining -= fill_qty;
                    let maker_fully_filled = maker.remaining_qty <= 1e-10;
                    trades.push(TradeResult {
                        trade_id: Uuid::new_v4(),
                        maker_order_id: maker.order_id,
                        taker_order_id: order_id,
                        maker_user_id: maker.user_id,
                        taker_user_id: user_id,
                        pair: pair.to_string(),
                        price: best_bid_price_nn.into_inner(),
                        qty: fill_qty,
                        maker_fully_filled,
                        taker_side: Side::Sell,
                        executed_at: Utc::now(),
                    });
                    if maker_fully_filled {
                        let mid = maker.order_id;
                        level.pop_front();
                        book.index.remove(&mid);
                    } else {
                        break;
                    }
                }
                if level.is_empty() {
                    book.bids.remove(&best_bid_key);
                }
            }
            if rest_in_book && remaining > 1e-10 {
                let p = price.unwrap();
                let key = NotNan::new(p).unwrap();
                book.asks
                    .entry(key)
                    .or_default()
                    .push_back(BookOrder { order_id, user_id, remaining_qty: remaining, timestamp });
                book.index.insert(order_id, (Side::Sell, p));
            }
        }
    }

    trades
}

pub fn cancel_order(book: &mut OrderBook, order_id: Uuid) -> bool {
    match book.index.remove(&order_id) {
        None => false,
        Some((Side::Buy, price)) => {
            let key = Reverse(NotNan::new(price).unwrap());
            if let Some(level) = book.bids.get_mut(&key) {
                level.retain(|o| o.order_id != order_id);
                if level.is_empty() {
                    book.bids.remove(&key);
                }
            }
            true
        }
        Some((Side::Sell, price)) => {
            let key = NotNan::new(price).unwrap();
            if let Some(level) = book.asks.get_mut(&key) {
                level.retain(|o| o.order_id != order_id);
                if level.is_empty() {
                    book.asks.remove(&key);
                }
            }
            true
        }
    }
}
