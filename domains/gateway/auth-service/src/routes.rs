use actix_web::{HttpResponse, Responder, post, web};
use bcrypt::{hash, verify};
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use sqlx::PgPool;
use std::env;
use types::{
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, RefreshTokenRequest,
    RefreshTokenResponse, RegisterRequest, RegisterResponse,
};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/auth")
            .service(register)
            .service(login)
            .service(refresh)
            .service(logout),
    );
}

#[post("/register")]
async fn register(pool: web::Data<PgPool>, req: web::Json<RegisterRequest>) -> impl Responder {
    if req.password.len() < 8 {
        return HttpResponse::BadRequest().body("password too short");
    }

    if !req.email.contains('@') || !req.email.contains('.') {
        return HttpResponse::BadRequest().body("invalid email format");
    }

    let cost = 12;
    let hashed_password = match hash(&req.password, cost) {
        Ok(h) => h,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    let user_id = Uuid::new_v4();

    if let Err(e) = db::users::create_user(&pool, &req.email, &hashed_password).await {
        eprintln!("Failed to create user: {:?}", e);
        return HttpResponse::InternalServerError().body("Failed to create user");
    }

    let response = RegisterResponse {
        user_id,
        email: req.email.clone(),
        created_at: Utc::now(),
    };

    HttpResponse::Ok().json(response)
}

#[post("/login")]
async fn login(
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    req: web::Json<LoginRequest>,
) -> impl Responder {
    if !req.email.contains('@') || !req.email.contains('.') {
        return HttpResponse::BadRequest().body("invalid email format");
    }

    let user = match db::users::get_user_by_email(&pg_pool, &req.email).await {
        Ok(Some(u)) => u,
        Ok(None) => return HttpResponse::Unauthorized().body("Invalid credentials"),
        Err(e) => {
            eprintln!("Db error : {:?}", e);
            return HttpResponse::InternalServerError().body("Internal server error");
        }
    };

    let valid = match verify(&req.password, &user.password) {
        Ok(v) => v,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    if !valid {
        return HttpResponse::Unauthorized().body("Invalid credentials");
    }

    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    let tokens = match utils::generate_tokens(user.id, &token_secret) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Token generation error: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    // Store refresh token in Redis with 7-day TTL
    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to get redis connection: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let redis_key = format!("refresh:{}", user.id);
    let ttl_seconds = 7 * 24 * 3600;

    let _: Result<(), _> = redis_conn
        .set_ex(redis_key, &tokens.refresh_token, ttl_seconds)
        .await;

    let response = LoginResponse {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
    };

    HttpResponse::Ok().json(response)
}

#[post("/refresh")]
async fn refresh(req: web::Json<RefreshTokenRequest>) -> impl Responder {
    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    let claims = match utils::verify_token(&req.refresh_token, &token_secret) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Refresh token verification failed: {:?}", e);
            return HttpResponse::Unauthorized().body("expired or invalid refresh token");
        }
    };

    let (access_token, expires_in) = match utils::generate_access_token(claims.sub, &token_secret) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Access token generation failed: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let response = RefreshTokenResponse {
        access_token,
        expires_in,
    };

    HttpResponse::Ok().json(response)
}

#[post("/logout")]
async fn logout(req: web::Json<LogoutRequest>, redis_pool: web::Data<RedisPool>) -> impl Responder {
    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    // verify refresh token
    let claims = match utils::verify_token(&req.refresh_token, &token_secret) {
        Ok(c) => c,
        Err(_) => {
            return HttpResponse::Unauthorized().body("Invalid refresh token");
        }
    };

    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to get redis connection: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let redis_key = format!("refresh:{}", claims.sub);

    let stored_token: Option<String> = match redis_conn.get(&redis_key).await {
        Ok(token) => token,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    if stored_token.is_none() || stored_token.unwrap() != req.refresh_token {
        return HttpResponse::Unauthorized().body("invalid refresh token");
    }

    // Explicitly specify return type for del operation
    let result: Result<(), _> = redis_conn.del(&redis_key).await;
    if result.is_err() {
        return HttpResponse::InternalServerError().finish();
    }

    let response = LogoutResponse { ok: true };
    HttpResponse::Ok().json(response)
}
