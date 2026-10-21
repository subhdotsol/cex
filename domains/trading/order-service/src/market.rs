// Read-only trade data backed by the `trades` table written by the settlement service.

use actix_web::{HttpResponse, Responder, get, web};
use serde::Deserialize;
use sqlx::PgPool;
use utils::auth::AuthUser;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(my_trades).service(candles);
}

#[derive(Deserialize)]
struct TradesQuery {
    pair: Option<String>,
    limit: Option<i64>,
}

/// Fills of the authenticated user (as maker or taker), newest first.
#[get("/trades")]
async fn my_trades(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    query: web::Query<TradesQuery>,
) -> impl Responder {
    let limit = query.limit.unwrap_or(50);
    if !(1..=500).contains(&limit) {
        return HttpResponse::BadRequest().body("limit must be between 1 and 500");
    }
    match db::trades::get_user_trades(&pg_pool, auth.0, query.pair.as_deref(), limit).await {
        Ok(trades) => {
            let rows: Vec<_> = trades
                .into_iter()
                .map(|t| {
                    let is_maker = t.maker_user_id == auth.0;
                    let taker_buy = t.taker_side == db::orders::DbOrderSide::Buy;
                    serde_json::json!({
                        "trade_id": t.id,
                        "pair": t.pair,
                        "price": t.price.normalize().to_string(),
                        "qty": t.qty.normalize().to_string(),
                        "side": if is_maker != taker_buy { "BUY" } else { "SELL" },
                        "liquidity": if is_maker { "MAKER" } else { "TAKER" },
                        "order_id": if is_maker { t.maker_order_id } else { t.taker_order_id },
                        "executed_at": t.executed_at,
                    })
                })
                .collect();
            HttpResponse::Ok().json(rows)
        }
        Err(e) => {
            eprintln!("Failed to load trades: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[derive(Deserialize)]
struct CandlesQuery {
    pair: String,
    interval: Option<String>,
    limit: Option<i64>,
}

/// Public OHLCV candles: /market/candles?pair=BTC-USDT&interval=1m&limit=100
#[get("/market/candles")]
async fn candles(pg_pool: web::Data<PgPool>, query: web::Query<CandlesQuery>) -> impl Responder {
    let interval = query.interval.as_deref().unwrap_or("1m");
    let Some(pg_interval) = db::trades::candle_interval(interval) else {
        return HttpResponse::BadRequest().body("interval must be one of 1m, 5m, 15m, 1h, 4h, 1d");
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return HttpResponse::BadRequest().body("limit must be between 1 and 1000");
    }
    match db::trades::get_candles(&pg_pool, &query.pair, pg_interval, limit).await {
        Ok(c) => HttpResponse::Ok().json(serde_json::json!({
            "pair": query.pair,
            "interval": interval,
            "candles": c,
        })),
        Err(e) => {
            eprintln!("Failed to load candles: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}
