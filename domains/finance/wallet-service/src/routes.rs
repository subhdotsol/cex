use actix_web::{HttpResponse, Responder, post, web};
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use utils::auth::AuthUser;
use types::{BalanceUpdate, DepositRequest, WithdrawRequest};

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/wallet").service(deposit).service(withdraw));
}

#[post("/deposit")]
async fn deposit(
    auth: AuthUser,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<DepositRequest>,
) -> impl Responder {
    let user_id = auth.0;

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

    // 3. Redis Pub/Sub: Notify WebSocket Gateway
    let update = BalanceUpdate {
        user_id,
        asset: body.asset.clone(),
        balance: new_balance.to_string(),
        change_type: "DEPOSIT".to_string(),
        timestamp: Utc::now(),
    };
    let payload = serde_json::to_string(&update).unwrap_or_default();
    let _: Result<(), _> = redis_conn.publish(format!("balance:{}", user_id), payload).await;

    HttpResponse::Ok().body(format!(
        "Deposited {} {}. New balance: {}",
        amount, body.asset, new_balance
    ))
}

#[post("/withdraw")]
async fn withdraw(
    auth: AuthUser,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<WithdrawRequest>,
) -> impl Responder {
    let user_id = auth.0;

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

    // 3. Redis Pub/Sub: Notify WebSocket Gateway
    let update = BalanceUpdate {
        user_id,
        asset: body.asset.clone(),
        balance: new_balance.to_string(),
        change_type: "WITHDRAW".to_string(),
        timestamp: Utc::now(),
    };
    let payload = serde_json::to_string(&update).unwrap_or_default();
    let _: Result<(), _> = redis_conn.publish(format!("balance:{}", user_id), payload).await;

    HttpResponse::Ok().body(format!(
        "Withdrew {} {}. New balance: {}",
        amount, body.asset, new_balance
    ))
}
