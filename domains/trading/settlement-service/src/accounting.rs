// Pure accounting rules, no IO. Kept separate so they are easy to unit test.

use db::orders::DbOrderStatus;
use kafka::trade::CloseReason;
use rust_decimal::Decimal;

/// What an order looks like before a fill is applied.
#[derive(Debug, Clone, Copy)]
pub struct OrderSnapshot {
    pub qty: Decimal,
    pub filled: Decimal,
    pub locked_remaining: Decimal,
}

/// Result of applying one fill to one side of a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillEffect {
    pub new_filled: Decimal,
    pub new_locked_remaining: Decimal,
    pub status: DbOrderStatus,
    /// paid out of the order's reserved funds
    pub lock_used: Decimal,
    /// paid out of `available` because the reservation was too small
    /// (only possible for MARKET buys where the lock is an estimate)
    pub shortfall: Decimal,
    /// reservation left over once the order is done (price improvement, estimate buffer)
    pub refund: Decimal,
}

/// `consume` is what this side pays: quote (price * qty) for buyers, base (qty) for sellers.
pub fn apply_fill(order: OrderSnapshot, fill_qty: Decimal, consume: Decimal) -> FillEffect {
    let lock_used = order.locked_remaining.min(consume);
    let shortfall = consume - lock_used;
    let after_lock = order.locked_remaining - lock_used;
    let new_filled = order.filled + fill_qty;

    if new_filled >= order.qty {
        FillEffect {
            new_filled,
            new_locked_remaining: Decimal::ZERO,
            status: DbOrderStatus::Filled,
            lock_used,
            shortfall,
            refund: after_lock,
        }
    } else {
        FillEffect {
            new_filled,
            new_locked_remaining: after_lock,
            status: DbOrderStatus::PartiallyFilled,
            lock_used,
            shortfall,
            refund: Decimal::ZERO,
        }
    }
}

pub fn close_status(reason: CloseReason) -> DbOrderStatus {
    match reason {
        CloseReason::Cancelled => DbOrderStatus::Cancelled,
        CloseReason::Expired => DbOrderStatus::Expired,
        CloseReason::Rejected => DbOrderStatus::Rejected,
    }
}

/// "BTC-USDT" -> ("BTC", "USDT")
pub fn split_pair(pair: &str) -> Option<(&str, &str)> {
    let (base, quote) = pair.split_once('-')?;
    if base.is_empty() || quote.is_empty() {
        return None;
    }
    Some((base, quote))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn partial_fill_keeps_remaining_lock() {
        // limit buy 2 @ 100 -> locked 200, fills 0.5 @ 100
        let o = OrderSnapshot { qty: d("2"), filled: d("0"), locked_remaining: d("200") };
        let e = apply_fill(o, d("0.5"), d("50"));
        assert_eq!(e.status, DbOrderStatus::PartiallyFilled);
        assert_eq!(e.new_locked_remaining, d("150"));
        assert_eq!(e.lock_used, d("50"));
        assert_eq!(e.refund, d("0"));
        assert_eq!(e.shortfall, d("0"));
    }

    #[test]
    fn full_fill_refunds_price_improvement() {
        // limit buy 1 @ 105 -> locked 105, fills at 100
        let o = OrderSnapshot { qty: d("1"), filled: d("0"), locked_remaining: d("105") };
        let e = apply_fill(o, d("1"), d("100"));
        assert_eq!(e.status, DbOrderStatus::Filled);
        assert_eq!(e.refund, d("5"));
        assert_eq!(e.new_locked_remaining, d("0"));
    }

    #[test]
    fn market_buy_underestimate_takes_shortfall() {
        let o = OrderSnapshot { qty: d("1"), filled: d("0"), locked_remaining: d("90") };
        let e = apply_fill(o, d("1"), d("100"));
        assert_eq!(e.lock_used, d("90"));
        assert_eq!(e.shortfall, d("10"));
        assert_eq!(e.refund, d("0"));
    }

    #[test]
    fn sell_side_consumes_base() {
        let o = OrderSnapshot { qty: d("3"), filled: d("1"), locked_remaining: d("2") };
        let e = apply_fill(o, d("2"), d("2"));
        assert_eq!(e.status, DbOrderStatus::Filled);
        assert_eq!(e.new_filled, d("3"));
        assert_eq!(e.refund, d("0"));
    }

    #[test]
    fn pair_split() {
        assert_eq!(split_pair("BTC-USDT"), Some(("BTC", "USDT")));
        assert_eq!(split_pair("BTCUSDT"), None);
    }
}
