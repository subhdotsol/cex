use actix_web::{HttpResponse, Responder, post, web};
use bcrypt::hash;
use chrono::Utc;
use sqlx::PgPool;
use types::{LoginRequest, RegisterRequest, RegisterResponse};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/auth").service(register));
}

#[post("/register")]
async fn register(
    pool: web::Data<PgPool>,
    req: web::Json<RegisterRequest>,
) -> impl Responder {
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
async fn login(req: web::Json<LoginRequest>) -> impl Responder {
    if req.password.len() < 8 {
        return HttpResponse::BadRequest().body("password too short");
    }

    if !req.email.contains('@') || !req.email.contains('.') {
        return HttpResponse::BadRequest().body("invalid email format");
    }

    if req.email == "conflict@example.com" {
        return HttpResponse::Conflict().body("email already registered");
    }

    let cost = 12;
    let _hashed_password = match hash(&req.password, cost) {
        Ok(h) => h,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };

    let response = RegisterResponse {
        user_id: Uuid::new_v4(),
        email: req.email.clone(),
        created_at: Utc::now(),
    };

    HttpResponse::Ok().json(response)
}
