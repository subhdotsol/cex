use actix_web::{HttpResponse, Responder, post, web, HttpRequest};
use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::Pool as RedisPool;
use std::env;
use types::{DepositRequest, WithdrawRequest};

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/wallet")
            .service(deposit)
            .service(withdraw),
    );
}

#[post("/deposit")]
async fn deposit(
    req: HttpRequest,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<DepositRequest>,
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
    let amount: f64 = match body.amount.parse() {
        Ok(a) if a > 0.0 => a,
        _ => return HttpResponse::BadRequest().body("amount must be greater than 0"),
    };

    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().body("Redis error"),
    };

    let balance_key = format!("balance:{}:{}", user_id, body.asset);
    
    // Increase balance
    let current_balance: f64 = match redis_conn.get::<_, Option<String>>(&balance_key).await {
        Ok(Some(b)) => b.parse().unwrap_or(0.0),
        _ => 0.0,
    };

    let new_balance = current_balance + amount;
    let _: Result<(), _> = redis_conn.set(&balance_key, new_balance.to_string()).await;

    HttpResponse::Ok().body(format!("Deposited {} {}. New balance: {}", amount, body.asset, new_balance))
}

#[post("/withdraw")]
async fn withdraw(
    req: HttpRequest,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<WithdrawRequest>,
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
    let amount: f64 = match body.amount.parse() {
        Ok(a) if a > 0.0 => a,
        _ => return HttpResponse::BadRequest().body("amount must be greater than 0"),
    };

    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().body("Redis error"),
    };

    let balance_key = format!("balance:{}:{}", user_id, body.asset);
    
    // Decrease balance
    let current_balance: f64 = match redis_conn.get::<_, Option<String>>(&balance_key).await {
        Ok(Some(b)) => b.parse().unwrap_or(0.0),
        _ => 0.0,
    };

    if current_balance < amount {
        return HttpResponse::BadRequest().body("Insufficient balance");
    }

    let new_balance = current_balance - amount;
    let _: Result<(), _> = redis_conn.set(&balance_key, new_balance.to_string()).await;

    HttpResponse::Ok().body(format!("Withdrew {} {}. New balance: {}", amount, body.asset, new_balance))
}
