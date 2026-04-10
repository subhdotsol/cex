use actix_web::{HttpRequest, HttpResponse, Responder, delete, get, post, web};
use chrono::Utc;
use db::orders::{self, DbOrderStatus};
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use kafka::FutureProducer;
use kafka::order::{OrderCancel, OrderNew, produce_order_cancel, produce_order_new};
use sqlx::PgPool;
use std::env;
use types::{
    BalanceUpdate, GetAllOrdersRequest, OrderCancelResponse, OrderHealthCheckResponse,
    OrderRequest, OrderResponse, OrderSide, OrderStatus, OrderType,
};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/orders")
            .service(place_order)
            .service(get_all_orders)
            .service(health_check)
            .service(get_order)
            .service(cancel_order),
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
    let new_balance = current_balance - amount_to_lock;
    let _: Result<(), _> = redis_conn
        .set(&balance_key, new_balance.to_string())
        .await;

    // Notify Gateway about LOCK via Pub/Sub
    let update = BalanceUpdate {
        user_id,
        asset: asset_to_lock.to_string(),
        balance: new_balance.to_string(),
        change_type: "LOCK".to_string(),
        timestamp: Utc::now(),
    };
    let payload = serde_json::to_string(&update).unwrap_or_default();
    let _: Result<(), _> = redis_conn
        .publish(format!("balance:{}", user_id), payload)
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
        // ROLLBACK Redis
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
    let response = OrderResponse {
        order_id: order.id,
        status: match order.status {
            db::orders::DbOrderStatus::Open => OrderStatus::Open,
            db::orders::DbOrderStatus::Filled => OrderStatus::Filled,
            db::orders::DbOrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
            db::orders::DbOrderStatus::Cancelled => OrderStatus::Cancelled,
            db::orders::DbOrderStatus::Rejected => OrderStatus::Rejected,
        },
        pair: order.pair.clone(),
        side: match order.side {
            db::orders::DbOrderSide::Buy => OrderSide::Buy,
            db::orders::DbOrderSide::Sell => OrderSide::Sell,
        },
        price: order.price.clone(),
        qty: order.qty.clone(),
        filled_qty: order.filled_qty.clone(),
        created_at: order.created_at,
    };

    HttpResponse::Ok().json(response)
}

#[get("")]
async fn get_all_orders(
    req: HttpRequest,
    pg_pool: web::Data<PgPool>,
    query: web::Query<GetAllOrdersRequest>,
) -> impl Responder {
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

    let limit = query.limit.unwrap_or(50);
    if limit < 1 || limit > 200 {
        return HttpResponse::BadRequest().body("limit must be between 1 and 200");
    }

    // Fetch limit + 1 to check for has_more
    let db_orders = match orders::get_all_orders(
        &pg_pool,
        user_id,
        limit + 1,
        query.status,
        query.pair.clone(),
        query.before,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to fetch orders: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let has_more = db_orders.len() > limit;
    let results = if has_more {
        &db_orders[..limit]
    } else {
        &db_orders[..]
    };

    let next_cursor = if has_more {
        results.last().map(|o| o.id)
    } else {
        None
    };

    let orders_response: Vec<OrderResponse> = results
        .iter()
        .map(|o| OrderResponse {
            order_id: o.id,
            status: match o.status {
                db::orders::DbOrderStatus::Open => OrderStatus::Open,
                db::orders::DbOrderStatus::Filled => OrderStatus::Filled,
                db::orders::DbOrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
                db::orders::DbOrderStatus::Cancelled => OrderStatus::Cancelled,
                db::orders::DbOrderStatus::Rejected => OrderStatus::Rejected,
            },
            pair: o.pair.clone(),
            side: match o.side {
                db::orders::DbOrderSide::Buy => OrderSide::Buy,
                db::orders::DbOrderSide::Sell => OrderSide::Sell,
            },
            price: o.price.clone(),
            qty: o.qty.clone(),
            filled_qty: o.filled_qty.clone(),
            created_at: o.created_at,
        })
        .collect();

    let response = types::PaginatedOrders {
        orders: orders_response,
        next_cursor,
        has_more,
    };

    HttpResponse::Ok().json(response)
}

#[get("/health")]
async fn health_check() -> impl Responder {
    let response = OrderHealthCheckResponse {
        status: "ok".to_string(),
        service: "order-service".to_string(),
        version: "0.1.0".to_string(),
    };
    HttpResponse::Ok().json(response)
}

#[delete("/{order_id}")]
async fn cancel_order(
    req: HttpRequest,
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    kafka_producer: web::Data<FutureProducer>,
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

    // 2. Fetch Order from Database
    let order = match db::orders::get_single_order(&pg_pool, order_id).await {
        Ok(Some(o)) => o,
        Ok(None) => return HttpResponse::NotFound().body("Order not found"),
        Err(e) => {
            eprintln!("Failed to fetch order: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    // 3. Ownership Check (403)
    if order.user_id != user_id {
        return HttpResponse::Forbidden().body("Order belongs to different user");
    }

    // 4. Status Validation (400)
    if order.status != DbOrderStatus::Open && order.status != DbOrderStatus::PartiallyFilled {
        return HttpResponse::BadRequest().body("Order is already filled, cancelled or rejected");
    }

    // 5. Side Effect: UPDATE orders SET status=CANCELLED
    if let Err(e) = db::orders::update_order_status(&pg_pool, order_id, DbOrderStatus::Cancelled).await {
        eprintln!("Failed to update order status: {:?}", e);
        return HttpResponse::InternalServerError().finish();
    }

    // 6. Side Effect: Unlock funds in Redis
    let parts: Vec<&str> = order.pair.split('-').collect();
    if parts.len() != 2 {
        return HttpResponse::InternalServerError().body("invalid pair format in DB");
    }
    let base_asset = parts[0];
    let quote_asset = parts[1];

    let qty: f64 = order.qty.parse().unwrap_or(0.0);
    let filled_qty: f64 = order.filled_qty.parse().unwrap_or(0.0);
    let remaining_qty = qty - filled_qty;
    let price: f64 = order.price.as_ref().map(|p| p.parse().unwrap_or(0.0)).unwrap_or(0.0);

    let (asset_to_unlock, amount_to_unlock) = match order.side {
        db::orders::DbOrderSide::Buy => (quote_asset, price * remaining_qty),
        db::orders::DbOrderSide::Sell => (base_asset, remaining_qty),
    };

    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    let balance_key = format!("balance:{}:{}", user_id, asset_to_unlock);

    let current_balance: f64 = match redis_conn.get::<_, Option<String>>(&balance_key).await {
        Ok(Some(b)) => b.parse().unwrap_or(0.0),
        _ => 0.0,
    };

    let new_balance = current_balance + amount_to_unlock;
    let _: Result<(), _> = redis_conn
        .set(&balance_key, new_balance.to_string())
        .await;

    // Notify Gateway about UNLOCK via Pub/Sub
    let update = BalanceUpdate {
        user_id,
        asset: asset_to_unlock.to_string(),
        balance: new_balance.to_string(),
        change_type: "UNLOCK".to_string(),
        timestamp: Utc::now(),
    };
    let payload = serde_json::to_string(&update).unwrap_or_default();
    let _: Result<(), _> = redis_conn.publish(format!("balance:{}", user_id), payload).await;

    // 7. Side Effect: Produce orders.cancel to Kafka
    let kafka_event = OrderCancel {
        order_id,
        user_id,
        pair: order.pair.clone(),
        timestamp: Utc::now(),
    };

    if let Err(e) = produce_order_cancel(&kafka_producer, &kafka_event).await {
        eprintln!("Failed to produce kafka cancel message: {:?}", e);
    }

    // 8. Response 200
    let response = OrderCancelResponse {
        order_id,
        status: OrderStatus::Cancelled,
    };

    HttpResponse::Ok().json(response)
}
