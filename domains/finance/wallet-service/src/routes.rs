use actix_web::{HttpResponse, Responder, get, post, web};
use db::Decimal;
use db::balances::{self, kind};
use deadpool_redis::Pool as RedisPool;
use sqlx::PgPool;
use types::{DepositRequest, WithdrawRequest};
use utils::auth::AuthUser;

// Mock deposit / withdraw: there is no chain integration yet, the amounts are
// simply credited / debited. Every movement is recorded in `ledger_entries`.

const SUPPORTED_ASSETS: [&str; 3] = ["BTC", "ETH", "USDT"];
const MAX_SCALE: u32 = 8;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/wallet")
            .service(deposit)
            .service(withdraw)
            .service(get_balances),
    );
}

fn parse_amount(asset: &str, amount: &str) -> Result<Decimal, HttpResponse> {
    if !SUPPORTED_ASSETS.contains(&asset) {
        return Err(HttpResponse::BadRequest().body("unsupported asset"));
    }
    match amount.trim().parse::<Decimal>() {
        Ok(a) if a > Decimal::ZERO && a.normalize().scale() <= MAX_SCALE => Ok(a),
        Ok(a) if a > Decimal::ZERO => {
            Err(HttpResponse::BadRequest().body(format!("max {MAX_SCALE} decimals")))
        }
        _ => Err(HttpResponse::BadRequest().body("amount must be greater than 0")),
    }
}

#[post("/deposit")]
async fn deposit(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<DepositRequest>,
) -> impl Responder {
    let user_id = auth.0;
    let amount = match parse_amount(&body.asset, &body.amount) {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let result: Result<_, sqlx::Error> = async {
        let mut conn = pg_pool.acquire().await?;
        balances::adjust(&mut conn, user_id, &body.asset, amount, Decimal::ZERO, kind::DEPOSIT, None).await
    }
    .await;

    let balance = match result {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Deposit failed: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    redis_client::publish_balance(&redis_pool, user_id, &balance.asset, balance.available.normalize().to_string(), "DEPOSIT").await;
    HttpResponse::Ok().json(balance)
}

#[post("/withdraw")]
async fn withdraw(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    body: web::Json<WithdrawRequest>,
) -> impl Responder {
    let user_id = auth.0;
    let amount = match parse_amount(&body.asset, &body.amount) {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let result: Result<_, sqlx::Error> = async {
        let mut conn = pg_pool.acquire().await?;
        balances::withdraw(&mut conn, user_id, &body.asset, amount).await
    }
    .await;

    let balance = match result {
        Ok(Some(b)) => b,
        Ok(None) => return HttpResponse::BadRequest().body("Insufficient balance"),
        Err(e) => {
            eprintln!("Withdraw failed: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    redis_client::publish_balance(&redis_pool, user_id, &balance.asset, balance.available.normalize().to_string(), "WITHDRAW").await;
    HttpResponse::Ok().json(balance)
}

#[get("/balances")]
async fn get_balances(auth: AuthUser, pg_pool: web::Data<PgPool>) -> impl Responder {
    match balances::get_balances(&pg_pool, auth.0).await {
        Ok(b) => HttpResponse::Ok().json(b),
        Err(e) => {
            eprintln!("Failed to load balances: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}
