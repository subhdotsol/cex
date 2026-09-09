use crate::state::{BookOrder, OrderBook, Side};
use chrono::{DateTime, Utc};
use ordered_float::NotNan;
use std::cmp::Reverse;
use std::collections::VecDeque;
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

/// What happened to the incoming order after matching.
#[derive(Debug)]
pub struct MatchOutcome {
    pub trades: Vec<TradeResult>,
    /// quantity of the incoming order that was neither filled nor left resting
    pub dropped_qty: f64,
    /// FOK order killed because the book could not fill it completely
    pub rejected: bool,
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
) -> MatchOutcome {
    // FOK: cancel entirely if full quantity can't be filled immediately
    if order_type == "FOK" {
        let available = available_liquidity(book, &side, price);
        if available < qty - 1e-10 {
            return MatchOutcome {
                trades: vec![],
                dropped_qty: qty,
                rejected: true,
            };
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

    let dropped_qty = if rest_in_book || remaining <= 1e-10 {
        0.0
    } else {
        remaining
    };

    MatchOutcome {
        trades,
        dropped_qty,
        rejected: false,
    }
}

/// Removes a resting order from the book.
/// Returns the (user_id, remaining_qty) of the removed order, or None if it was not resting
/// (already filled, already cancelled, or never existed).
pub fn cancel_order(book: &mut OrderBook, order_id: Uuid) -> Option<(Uuid, f64)> {
    fn take(level: &mut VecDeque<BookOrder>, order_id: Uuid) -> Option<(Uuid, f64)> {
        let pos = level.iter().position(|o| o.order_id == order_id)?;
        level.remove(pos).map(|o| (o.user_id, o.remaining_qty))
    }

    match book.index.remove(&order_id)? {
        (Side::Buy, price) => {
            let key = Reverse(NotNan::new(price).unwrap());
            let level = book.bids.get_mut(&key)?;
            let removed = take(level, order_id);
            if level.is_empty() {
                book.bids.remove(&key);
            }
            removed
        }
        (Side::Sell, price) => {
            let key = NotNan::new(price).unwrap();
            let level = book.asks.get_mut(&key)?;
            let removed = take(level, order_id);
            if level.is_empty() {
                book.asks.remove(&key);
            }
            removed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> OrderBook {
        OrderBook::default()
    }

    fn place(b: &mut OrderBook, side: Side, ty: &str, price: Option<f64>, qty: f64) -> (Uuid, MatchOutcome) {
        let id = Uuid::new_v4();
        let out = match_order(b, id, Uuid::new_v4(), "BTC-USDT", side, ty, price, qty, Utc::now());
        (id, out)
    }

    #[test]
    fn limit_rests_when_no_liquidity() {
        let mut b = book();
        let (_, out) = place(&mut b, Side::Buy, "LIMIT", Some(100.0), 1.0);
        assert!(out.trades.is_empty());
        assert_eq!(out.dropped_qty, 0.0);
        assert_eq!(b.index.len(), 1);
    }

    #[test]
    fn crossing_limit_fills_at_maker_price() {
        let mut b = book();
        place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        let (_, out) = place(&mut b, Side::Buy, "LIMIT", Some(105.0), 0.4);
        assert_eq!(out.trades.len(), 1);
        assert_eq!(out.trades[0].price, 100.0);
        assert!((out.trades[0].qty - 0.4).abs() < 1e-9);
        assert!(!out.trades[0].maker_fully_filled);
    }

    #[test]
    fn price_time_priority() {
        let mut b = book();
        let (first, _) = place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        place(&mut b, Side::Sell, "LIMIT", Some(99.0), 1.0);
        let (_, out) = place(&mut b, Side::Buy, "MARKET", None, 1.5);
        assert_eq!(out.trades[0].price, 99.0);
        assert_eq!(out.trades[1].maker_order_id, first);
    }

    #[test]
    fn ioc_drops_remainder() {
        let mut b = book();
        place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        let (_, out) = place(&mut b, Side::Buy, "IOC", Some(100.0), 3.0);
        assert_eq!(out.trades.len(), 1);
        assert!((out.dropped_qty - 2.0).abs() < 1e-9);
        assert!(b.index.is_empty());
    }

    #[test]
    fn fok_rejected_without_full_liquidity() {
        let mut b = book();
        place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        let (_, out) = place(&mut b, Side::Buy, "FOK", Some(100.0), 2.0);
        assert!(out.rejected);
        assert!(out.trades.is_empty());
        assert_eq!(b.index.len(), 1);
    }

    #[test]
    fn cancel_returns_remaining() {
        let mut b = book();
        let (id, _) = place(&mut b, Side::Sell, "LIMIT", Some(100.0), 1.0);
        place(&mut b, Side::Buy, "LIMIT", Some(100.0), 0.25);
        let (_, rem) = cancel_order(&mut b, id).unwrap();
        assert!((rem - 0.75).abs() < 1e-9);
        assert!(cancel_order(&mut b, id).is_none());
        assert!(b.asks.is_empty());
    }
}
