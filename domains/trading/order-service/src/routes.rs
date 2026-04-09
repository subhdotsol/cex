use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use kafka::FutureProducer;
use kafka::order::{OrderNew, produce_order_new};
use sqlx::PgPool;
use std::env;
use types::{OrderRequest, OrderResponse, OrderSide, OrderStatus, OrderType};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/orders")
            .service(place_order)
            .service(get_order),
    );
}

#[post("")]
async fn place_order(
    req: HttpRequest,
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    kafka_producer: web::Data<FutureProducer>,
    body: web::Json<OrderRequest>,
) -> impl Responder {
    // 1. JWT Authentication
    let auth_header = match req.headers().get("Authorization") {
        Some(h) => h.to_str().unwrap_or(""),
        None => return HttpResponse::Unauthorized().body("Missing Authorization header"),
    };

    if !auth_header.starts_with("Bearer ") {
        return HttpResponse::Unauthorized().body("Invalid Authorization header format");
    }

    let token = &auth_header[7..];
    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    let claims = match utils::verify_token(token, &token_secret) {
        Ok(c) => c,
        Err(_) => return HttpResponse::Unauthorized().body("Invalid or expired token"),
    };

    let user_id = claims.sub;

    // 2. Validation
    let qty: f64 = match body.qty.parse() {
        Ok(q) if q > 0.0 => q,
        _ => return HttpResponse::BadRequest().body("qty must be greater than 0"),
    };

    if body.order_type != OrderType::Market && body.price.is_none() {
        return HttpResponse::BadRequest().body("price is required for LIMIT/IOC/FOK orders");
    }

    let price: f64 = if let Some(p) = &body.price {
        match p.parse() {
            Ok(val) if val > 0.0 => val,
            _ => return HttpResponse::BadRequest().body("invalid price"),
        }
    } else {
        0.0 // Market order might not have price initially
    };

    // Unknown pair check (simplified)
    let supported_pairs = vec!["BTC-USDT", "ETH-USDT"];
    if !supported_pairs.contains(&body.pair.as_str()) {
        return HttpResponse::BadRequest().body("unknown pair");
    }

    // 3. Balance pre-check and lock via Redis
    let parts: Vec<&str> = body.pair.split('-').collect();
    if parts.len() != 2 {
        return HttpResponse::BadRequest().body("invalid pair format");
    }
    let base_asset = parts[0];
    let quote_asset = parts[1];

    let (asset_to_lock, amount_to_lock) = match body.side {
        OrderSide::Buy => (quote_asset, price * qty),
        OrderSide::Sell => (base_asset, qty),
    };

    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    let balance_key = format!("balance:{}:{}", user_id, asset_to_lock);

    // Check balance first (simplified atomic check using script or just GET/SET if not using MULTI)
    let current_balance: f64 = match redis_conn.get::<_, Option<String>>(&balance_key).await {
        Ok(Some(b)) => b.parse().unwrap_or(0.0),
        _ => 0.0,
    };

    if current_balance < amount_to_lock {
        return HttpResponse::BadRequest().body("insufficient balance");
    }

    // Lock funds (simplified DECRBY)
    let _: Result<(), _> = redis_conn
        .set(&balance_key, (current_balance - amount_to_lock).to_string())
        .await;

    // 4. INSERT into Postgres
    let order_id = Uuid::new_v4();
    if let Err(e) = db::orders::create_order(
        &pg_pool,
        order_id,
        user_id,
        &body.pair,
        body.side,
        body.order_type,
        body.price.as_deref(),
        &body.qty,
    )
    .await
    {
        eprintln!("Failed to save order: {:?}", e);
        // ROLLBACK Redis (ideally should be in a transaction)
        let _: Result<(), _> = redis_conn
            .set(&balance_key, current_balance.to_string())
            .await;
        return HttpResponse::InternalServerError().body("Failed to save order");
    }

    // 5. Produce to Kafka
    let kafka_event = OrderNew {
        order_id,
        user_id,
        pair: body.pair.clone(),
        side: format!("{:?}", body.side).to_uppercase(),
        order_type: format!("{:?}", body.order_type).to_uppercase(),
        price: body.price.clone(),
        qty: body.qty.clone(),
        timestamp: Utc::now(),
    };

    if let Err(e) = produce_order_new(&kafka_producer, &kafka_event).await {
        eprintln!("Failed to produce kafka message: {:?}", e);
        // Note: In a real system, we'd need a way to handle this (e.g., outbox pattern)
    }

    // 6. Response
    let response = OrderResponse {
        order_id,
        status: OrderStatus::Open,
        pair: body.pair.clone(),
        side: body.side,
        price: body.price.clone(),
        qty: body.qty.clone(),
        filled_qty: "0".to_string(),
        created_at: Utc::now(),
    };

    HttpResponse::Ok().json(response)
}

#[get("/{order_id}")]
async fn get_order(
    req: HttpRequest,
    pg_pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> impl Responder {
    // 1. JWT Authentication
    let auth_header = match req.headers().get("Authorization") {
        Some(h) => h.to_str().unwrap_or(""),
        None => return HttpResponse::Unauthorized().body("Missing Authorization header"),
    };

    if !auth_header.starts_with("Bearer ") {
        return HttpResponse::Unauthorized().body("Invalid Authorization header format");
    }

    let token = &auth_header[7..];
    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    let claims = match utils::verify_token(token, &token_secret) {
        Ok(c) => c,
        Err(_) => return HttpResponse::Unauthorized().body("Invalid or expired token"),
    };

    let user_id = claims.sub;
    let order_id = path.into_inner();

    // 2. Fetch from DB
    let order = match db::orders::get_single_order(&pg_pool, order_id).await {
        Ok(Some(o)) => o,
        Ok(None) => return HttpResponse::NotFound().body("Order not found"),
        Err(e) => {
            eprintln!("Failed to fetch order: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    // 3. Ownership Check
    if order.user_id != user_id {
        return HttpResponse::Forbidden().body("Order belongs to different user");
    }

    // 4. Return Order
    HttpResponse::Ok().json(order)
}
