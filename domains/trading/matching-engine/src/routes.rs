use crate::state::SharedState;
use actix_web::{HttpResponse, Responder, get, web};
use chrono::Utc;
use serde::Deserialize;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(depth)
        .service(recent_trades)
        .service(ticker)
        .service(health);
}

#[derive(Deserialize)]
struct PairQuery {
    pair: String,
    limit: Option<usize>,
}

#[get("/depth")]
async fn depth(state: web::Data<SharedState>, query: web::Query<PairQuery>) -> impl Responder {
    let st = state.read().await;
    let book = match st.books.get(&query.pair) {
        Some(b) => b,
        None => return HttpResponse::BadRequest().body("unknown pair"),
    };

    let limit = query.limit.unwrap_or(20).min(100);
    let (bids, asks) = book.depth_snapshot(limit);

    HttpResponse::Ok().json(serde_json::json!({
        "pair": query.pair,
        "bids": bids.iter().map(|(p, q)| serde_json::json!({ "price": format!("{:.2}", p), "qty": format!("{:.8}", q) })).collect::<Vec<_>>(),
        "asks": asks.iter().map(|(p, q)| serde_json::json!({ "price": format!("{:.2}", p), "qty": format!("{:.8}", q) })).collect::<Vec<_>>(),
        "timestamp": Utc::now(),
    }))
}

#[get("/trades/recent")]
async fn recent_trades(
    state: web::Data<SharedState>,
    query: web::Query<PairQuery>,
) -> impl Responder {
    let st = state.read().await;
    let ring = match st.recent_trades.get(&query.pair) {
        Some(r) => r,
        None => return HttpResponse::BadRequest().body("unknown pair"),
    };

    let limit = query.limit.unwrap_or(50);
    let trades: Vec<_> = ring.iter().rev().take(limit).collect();

    HttpResponse::Ok().json(serde_json::json!({
        "pair": query.pair,
        "trades": trades,
    }))
}

#[get("/ticker")]
async fn ticker(state: web::Data<SharedState>, query: web::Query<PairQuery>) -> impl Responder {
    let st = state.read().await;
    let t = match st.ticker.get(&query.pair) {
        Some(t) => t,
        None => return HttpResponse::BadRequest().body("unknown pair"),
    };

    let change_pct = if t.open_price > 0.0 {
        ((t.last_price - t.open_price) / t.open_price) * 100.0
    } else {
        0.0
    };

    HttpResponse::Ok().json(serde_json::json!({
        "pair": query.pair,
        "last_price": format!("{:.2}", t.last_price),
        "change_pct": format!("{:.2}", change_pct),
        "high_24h": format!("{:.2}", t.high_24h),
        "low_24h": format!("{:.2}", t.low_24h),
        "volume_24h": format!("{:.8}", t.volume_24h),
        "updated_at": t.updated_at,
    }))
}

#[get("/health")]
async fn health(state: web::Data<SharedState>) -> impl Responder {
    let st = state.read().await;
    let orders_in_book = st.orders_in_book();
    let pairs: Vec<&String> = st.books.keys().collect();

    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
        "pairs": pairs,
        "kafka_lag": st.kafka_lag,
        "orders_in_book": orders_in_book,
    }))
}
