use actix_web::{HttpResponse, Responder, post, web};
use bcrypt::{hash, verify};
use chrono::Utc;
use sqlx::PgPool;
use types::{
    LoginRequest, LoginResponse, RefreshTokenRequest, RefreshTokenResponse, RegisterRequest,
    RegisterResponse,
};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/auth")
            .service(register)
            .service(login)
            .service(refresh),
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
async fn login(pool: web::Data<PgPool>, req: web::Json<LoginRequest>) -> impl Responder {
    if !req.email.contains('@') || !req.email.contains('.') {
        return HttpResponse::BadRequest().body("invalid email format");
    }

    let user = match db::users::get_user_by_email(&pool, &req.email).await {
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

    let tokens = match utils::generate_tokens(user.id, "secret") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Token generation error: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let response = LoginResponse {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
    };

    HttpResponse::Ok().json(response)
}

#[post("/refresh")]
async fn refresh(req: web::Json<RefreshTokenRequest>) -> impl Responder {
    let claims = match utils::verify_token(&req.refresh_token, "secret") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Refresh token verification failed: {:?}", e);
            return HttpResponse::Unauthorized().body("expired or invalid refresh token");
        }
    };

    let (access_token, expires_in) = match utils::generate_access_token(claims.sub, "secret") {
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
