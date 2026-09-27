// Risk engine
//
// Pre-trade checks run by the order service before an order is accepted:
//   - pair is listed, price / qty respect tick + lot size and min / max limits
//   - order notional is within limits
//   - user does not exceed the max number of open orders
//   - computes the funds that must be reserved (quote for buys, base for sells);
//     MARKET buys are priced by walking the current ask side of the book + a buffer
//
// The actual reservation happens in Postgres (`db::balances::lock`) in the same
// transaction as the order insert, so a check-then-lock race cannot overspend.

use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::fmt;
use types::{OrderRequest, OrderSide, OrderType};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PairRules {
    pub base: String,
    pub quote: String,
    /// max decimal places of price (matching engine prints prices with 2)
    pub price_scale: u32,
    /// max decimal places of qty
    pub qty_scale: u32,
    pub min_qty: Decimal,
    pub max_qty: Decimal,
    /// min / max price * qty in quote
    pub min_notional: Decimal,
    pub max_notional: Decimal,
}

#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub pairs: HashMap<String, PairRules>,
    pub max_open_orders_per_user: i64,
    /// MARKET buy reservation = estimated cost * (1 + buffer)
    pub market_buy_buffer: Decimal,
}

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid decimal literal")
}

impl Default for RiskConfig {
    fn default() -> Self {
        let mut pairs = HashMap::new();
        pairs.insert(
            "BTC-USDT".to_string(),
            PairRules {
                base: "BTC".into(),
                quote: "USDT".into(),
                price_scale: 2,
                qty_scale: 6,
                min_qty: dec("0.00001"),
                max_qty: dec("100"),
                min_notional: dec("1"),
                max_notional: dec("5000000"),
            },
        );
        pairs.insert(
            "ETH-USDT".to_string(),
            PairRules {
                base: "ETH".into(),
                quote: "USDT".into(),
                price_scale: 2,
                qty_scale: 5,
                min_qty: dec("0.0001"),
                max_qty: dec("1000"),
                min_notional: dec("1"),
                max_notional: dec("5000000"),
            },
        );
        Self {
            pairs,
            max_open_orders_per_user: 200,
            market_buy_buffer: dec("0.05"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum RiskError {
    UnknownPair,
    InvalidQty(String),
    InvalidPrice(String),
    Notional(String),
    TooManyOpenOrders,
    NoLiquidity,
    Internal(String),
}

impl fmt::Display for RiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskError::UnknownPair => write!(f, "unknown pair"),
            RiskError::InvalidQty(m) => write!(f, "invalid qty: {m}"),
            RiskError::InvalidPrice(m) => write!(f, "invalid price: {m}"),
            RiskError::Notional(m) => write!(f, "notional {m}"),
            RiskError::TooManyOpenOrders => write!(f, "too many open orders"),
            RiskError::NoLiquidity => write!(f, "not enough liquidity for market order"),
            RiskError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl RiskError {
    /// true when the client sent something we reject (HTTP 400)
    pub fn is_client_error(&self) -> bool {
        !matches!(self, RiskError::Internal(_))
    }
}

/// Parsed and validated order.
#[derive(Debug, Clone)]
pub struct ValidatedOrder {
    pub base: String,
    pub quote: String,
    pub qty: Decimal,
    pub price: Option<Decimal>,
}

/// Outcome of a successful assessment: what to reserve before the order may enter the book.
#[derive(Debug, Clone)]
pub struct Assessment {
    pub order: ValidatedOrder,
    pub lock_asset: String,
    pub lock_amount: Decimal,
}

/// Stateless checks on the request itself.
pub fn validate(cfg: &RiskConfig, req: &OrderRequest) -> Result<ValidatedOrder, RiskError> {
    let rules = cfg.pairs.get(&req.pair).ok_or(RiskError::UnknownPair)?;

    let qty: Decimal = req
        .qty
        .trim()
        .parse()
        .map_err(|_| RiskError::InvalidQty("not a number".into()))?;
    if qty <= Decimal::ZERO {
        return Err(RiskError::InvalidQty("must be greater than 0".into()));
    }
    if qty.normalize().scale() > rules.qty_scale {
        return Err(RiskError::InvalidQty(format!("max {} decimals", rules.qty_scale)));
    }
    if qty < rules.min_qty || qty > rules.max_qty {
        return Err(RiskError::InvalidQty(format!(
            "must be between {} and {}",
            rules.min_qty, rules.max_qty
        )));
    }

    let price = match (req.order_type, &req.price) {
        (OrderType::Market, _) => None,
        (_, None) => {
            return Err(RiskError::InvalidPrice(
                "required for LIMIT/IOC/FOK orders".into(),
            ));
        }
        (_, Some(p)) => {
            let p: Decimal = p
                .trim()
                .parse()
                .map_err(|_| RiskError::InvalidPrice("not a number".into()))?;
            if p <= Decimal::ZERO {
                return Err(RiskError::InvalidPrice("must be greater than 0".into()));
            }
            if p.normalize().scale() > rules.price_scale {
                return Err(RiskError::InvalidPrice(format!(
                    "max {} decimals",
                    rules.price_scale
                )));
            }
            Some(p)
        }
    };

    if let Some(p) = price {
        let notional = p * qty;
        if notional < rules.min_notional {
            return Err(RiskError::Notional(format!("below minimum {}", rules.min_notional)));
        }
        if notional > rules.max_notional {
            return Err(RiskError::Notional(format!("above maximum {}", rules.max_notional)));
        }
    }

    Ok(ValidatedOrder {
        base: rules.base.clone(),
        quote: rules.quote.clone(),
        qty,
        price,
    })
}

/// Cost of buying `qty` by walking `asks` (best first). None if the book is too thin.
pub fn estimate_market_buy(asks: &[(Decimal, Decimal)], qty: Decimal) -> Option<Decimal> {
    let mut remaining = qty;
    let mut cost = Decimal::ZERO;
    for &(price, size) in asks {
        if remaining <= Decimal::ZERO {
            break;
        }
        let take = remaining.min(size);
        cost += take * price;
        remaining -= take;
    }
    (remaining <= Decimal::ZERO).then_some(cost)
}

/// Funds to reserve for an order.
pub fn lock_requirement(
    cfg: &RiskConfig,
    order: &ValidatedOrder,
    side: OrderSide,
    market_buy_cost: Option<Decimal>,
) -> Result<(String, Decimal), RiskError> {
    match side {
        OrderSide::Sell => Ok((order.base.clone(), order.qty)),
        OrderSide::Buy => {
            let amount = match order.price {
                Some(p) => p * order.qty,
                None => {
                    let cost = market_buy_cost.ok_or(RiskError::NoLiquidity)?;
                    (cost * (Decimal::ONE + cfg.market_buy_buffer)).round_dp(8)
                }
            };
            Ok((order.quote.clone(), amount))
        }
    }
}

/// Reads the top of book published by the matching engine (`depth:{pair}` in Redis).
async fn load_asks(redis: &RedisPool, pair: &str) -> Result<Vec<(Decimal, Decimal)>, RiskError> {
    let mut conn = redis
        .get()
        .await
        .map_err(|e| RiskError::Internal(e.to_string()))?;
    let raw: Option<String> = conn
        .get(format!("depth:{pair}"))
        .await
        .map_err(|e| RiskError::Internal(e.to_string()))?;
    let Some(raw) = raw else {
        return Ok(vec![]);
    };
    let snapshot: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| RiskError::Internal(e.to_string()))?;

    let asks = snapshot["asks"]
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(|l| {
                    let p = l["price"].as_str()?.parse().ok()?;
                    let q = l["qty"].as_str()?.parse().ok()?;
                    Some((p, q))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(asks)
}

async fn open_order_count(pg: &PgPool, user_id: Uuid) -> Result<i64, RiskError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM orders WHERE user_id = $1 AND status IN ('OPEN', 'PARTIALLY_FILLED')",
    )
    .bind(user_id)
    .fetch_one(pg)
    .await
    .map_err(|e| RiskError::Internal(e.to_string()))
}

/// Full pre-trade assessment.
pub async fn assess(
    cfg: &RiskConfig,
    pg: &PgPool,
    redis: &RedisPool,
    user_id: Uuid,
    req: &OrderRequest,
) -> Result<Assessment, RiskError> {
    let order = validate(cfg, req)?;

    if open_order_count(pg, user_id).await? >= cfg.max_open_orders_per_user {
        return Err(RiskError::TooManyOpenOrders);
    }

    let market_buy_cost = if req.side == OrderSide::Buy && order.price.is_none() {
        let asks = load_asks(redis, &req.pair).await?;
        estimate_market_buy(&asks, order.qty)
    } else {
        None
    };

    let (lock_asset, lock_amount) = lock_requirement(cfg, &order, req.side, market_buy_cost)?;
    Ok(Assessment {
        order,
        lock_asset,
        lock_amount,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(side: OrderSide, ty: OrderType, price: Option<&str>, qty: &str) -> OrderRequest {
        OrderRequest {
            pair: "BTC-USDT".into(),
            side,
            order_type: ty,
            price: price.map(String::from),
            qty: qty.into(),
        }
    }

    #[test]
    fn rejects_unknown_pair() {
        let mut r = req(OrderSide::Buy, OrderType::Limit, Some("100"), "1");
        r.pair = "DOGE-USDT".into();
        assert_eq!(validate(&RiskConfig::default(), &r).unwrap_err(), RiskError::UnknownPair);
    }

    #[test]
    fn rejects_bad_tick_and_lot() {
        let cfg = RiskConfig::default();
        let r = req(OrderSide::Buy, OrderType::Limit, Some("100.123"), "1");
        assert!(matches!(validate(&cfg, &r), Err(RiskError::InvalidPrice(_))));
        let r = req(OrderSide::Buy, OrderType::Limit, Some("100"), "0.1234567");
        assert!(matches!(validate(&cfg, &r), Err(RiskError::InvalidQty(_))));
    }

    #[test]
    fn limit_requires_price() {
        let r = req(OrderSide::Buy, OrderType::Ioc, None, "1");
        assert!(matches!(
            validate(&RiskConfig::default(), &r),
            Err(RiskError::InvalidPrice(_))
        ));
    }

    #[test]
    fn notional_limits() {
        let r = req(OrderSide::Buy, OrderType::Limit, Some("0.01"), "1");
        assert!(matches!(
            validate(&RiskConfig::default(), &r),
            Err(RiskError::Notional(_))
        ));
    }

    #[test]
    fn buy_limit_locks_quote_sell_locks_base() {
        let cfg = RiskConfig::default();
        let r = req(OrderSide::Buy, OrderType::Limit, Some("100.50"), "2");
        let v = validate(&cfg, &r).unwrap();
        assert_eq!(lock_requirement(&cfg, &v, OrderSide::Buy, None).unwrap(), ("USDT".into(), dec("201")));
        assert_eq!(lock_requirement(&cfg, &v, OrderSide::Sell, None).unwrap(), ("BTC".into(), dec("2")));
    }

    #[test]
    fn market_buy_walks_book_with_buffer() {
        let asks = vec![(dec("100"), dec("1")), (dec("110"), dec("1"))];
        assert_eq!(estimate_market_buy(&asks, dec("1.5")), Some(dec("155")));
        assert_eq!(estimate_market_buy(&asks, dec("3")), None);

        let cfg = RiskConfig::default();
        let v = validate(&cfg, &req(OrderSide::Buy, OrderType::Market, None, "1.5")).unwrap();
        let (_, amt) = lock_requirement(&cfg, &v, OrderSide::Buy, Some(dec("155"))).unwrap();
        assert_eq!(amt, dec("162.75"));
        assert_eq!(
            lock_requirement(&cfg, &v, OrderSide::Buy, None).unwrap_err(),
            RiskError::NoLiquidity
        );
    }
}
