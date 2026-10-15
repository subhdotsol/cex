use actix_web::{HttpResponse, Responder, delete, get, post, web};
use bcrypt::{hash, verify};
use chrono::Utc;
use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::Pool as RedisPool;
use sqlx::PgPool;
use std::env;
use types::{
    CreateApiKeyRequest, CreateApiKeyResponse, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, RefreshTokenRequest,
    RefreshTokenResponse, RegisterRequest, RegisterResponse,
};
use utils::apikey;
use utils::auth::AuthUser;
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/auth")
            .service(register)
            .service(login)
            .service(refresh)
            .service(logout)
            .service(create_api_key)
            .service(list_api_keys)
            .service(revoke_api_key),
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
async fn refresh(
    redis_pool: web::Data<RedisPool>,
    req: web::Json<RefreshTokenRequest>,
) -> impl Responder {
    let token_secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");

    let claims = match utils::verify_token(&req.refresh_token, &token_secret) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Refresh token verification failed: {:?}", e);
            return HttpResponse::Unauthorized().body("expired or invalid refresh token");
        }
    };

    // Check Redis for the token
    let mut redis_conn = match redis_pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to get redis connection: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let redis_key = format!("refresh:{}", claims.sub);
    let stored_token: Option<String> = match redis_conn.get(&redis_key).await {
        Ok(t) => t,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    if stored_token.is_none() || stored_token.unwrap() != req.refresh_token {
        return HttpResponse::Unauthorized().body("expired or invalid refresh token");
    }

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

fn encryption_key() -> Option<[u8; 32]> {
    let raw = env::var("API_KEY_ENCRYPTION_KEY").ok()?;
    match apikey::parse_encryption_key(&raw) {
        Ok(k) => Some(k),
        Err(_) => {
            eprintln!("API_KEY_ENCRYPTION_KEY must be 64 hex chars (32 bytes)");
            None
        }
    }
}

#[post("/api-keys")]
async fn create_api_key(
    auth: AuthUser,
    pool: web::Data<PgPool>,
    body: web::Json<CreateApiKeyRequest>,
) -> impl Responder {
    let Some(enc_key) = encryption_key() else {
        return HttpResponse::InternalServerError().body("api keys are not configured");
    };

    let label = body.label.as_deref().map(str::trim).filter(|l| !l.is_empty());
    if label.is_some_and(|l| l.len() > 64) {
        return HttpResponse::BadRequest().body("label too long (max 64)");
    }

    match db::api_keys::count_active(&pool, auth.0).await {
        Ok(n) if n >= db::api_keys::MAX_KEYS_PER_USER => {
            return HttpResponse::BadRequest().body("api key limit reached, revoke one first");
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to count api keys: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    }

    let (key, secret) = apikey::generate_key_pair();
    let (ciphertext, nonce) = match apikey::encrypt_secret(&enc_key, &secret) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to encrypt api secret: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    match db::api_keys::create_api_key(&pool, auth.0, &key, &ciphertext, &nonce, label).await {
        Ok(info) => HttpResponse::Ok().json(CreateApiKeyResponse {
            key: info.key,
            secret,
            label: info.label,
            created_at: info.created_at,
        }),
        Err(e) => {
            eprintln!("Failed to store api key: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[get("/api-keys")]
async fn list_api_keys(auth: AuthUser, pool: web::Data<PgPool>) -> impl Responder {
    match db::api_keys::list_api_keys(&pool, auth.0).await {
        Ok(keys) => HttpResponse::Ok().json(keys),
        Err(e) => {
            eprintln!("Failed to list api keys: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[delete("/api-keys/{key}")]
async fn revoke_api_key(
    auth: AuthUser,
    pool: web::Data<PgPool>,
    path: web::Path<String>,
) -> impl Responder {
    match db::api_keys::revoke_api_key(&pool, auth.0, &path).await {
        Ok(true) => HttpResponse::Ok().json(serde_json::json!({ "ok": true })),
        Ok(false) => HttpResponse::NotFound().body("api key not found"),
        Err(e) => {
            eprintln!("Failed to revoke api key: {e:?}");
            HttpResponse::InternalServerError().finish()
        }
    }
}
