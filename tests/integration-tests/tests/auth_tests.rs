use actix_web::{App, test, web};
use auth_service::routes;
use serde_json::json;
use std::env;
use types::{LoginResponse, RegisterResponse, RefreshTokenResponse};
use deadpool_redis::Pool as RedisPool;
use sqlx::PgPool;

async fn setup_test_data() -> (PgPool, RedisPool) {
    dotenvy::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let pg_pool = db::connection_pool(&database_url)
        .await
        .expect("Failed to create postgres pool");

    let redis_pool = redis_client::create_pool(&redis_url);
    (pg_pool, redis_pool)
}

#[tokio::test]
async fn test_db_connection() {
    let (pg_pool, _): (PgPool, RedisPool) = setup_test_data().await;
    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pg_pool)
        .await
        .expect("DB connection failed");
    assert_eq!(row.0, 1);
}

#[tokio::test]
async fn test_redis_connection() {
    let (_, redis_pool): (PgPool, RedisPool) = setup_test_data().await;
    let mut conn = redis_pool.get().await.expect("Redis connection failed");
    let _: () = deadpool_redis::redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("Redis PING failed");
}

#[tokio::test]
async fn test_full_auth_flow() {
    let (pg_pool, redis_pool): (PgPool, RedisPool) = setup_test_data().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pg_pool.clone()))
            .app_data(web::Data::new(redis_pool.clone()))
            .configure(routes::init),
    )
    .await;

    let email = format!("test-{}@example.com", uuid::Uuid::new_v4());
    let password = "password123";

    // 1. Register
    let req = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(json!({ "email": email, "password": password }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
    let reg_resp: RegisterResponse = test::read_body_json(resp).await;
    assert_eq!(reg_resp.email, email);

    // 2. Login
    let req = test::TestRequest::post()
        .uri("/auth/login")
        .set_json(json!({ "email": email, "password": password }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
    let login_resp: LoginResponse = test::read_body_json(resp).await;
    assert!(!login_resp.access_token.is_empty());
    assert!(!login_resp.refresh_token.is_empty());

    // 3. Refresh
    let req = test::TestRequest::post()
        .uri("/auth/refresh")
        .set_json(json!({ "refresh_token": login_resp.refresh_token }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
    let refresh_resp: RefreshTokenResponse = test::read_body_json(resp).await;
    assert!(!refresh_resp.access_token.is_empty());

    // 4. Logout
    let req = test::TestRequest::post()
        .uri("/auth/logout")
        .set_json(json!({ "refresh_token": login_resp.refresh_token }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    // 5. Try Refresh again (should fail because logout deleted it from Redis)
    let req = test::TestRequest::post()
        .uri("/auth/refresh")
        .set_json(json!({ "refresh_token": login_resp.refresh_token }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}
