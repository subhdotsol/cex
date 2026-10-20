use actix_web::{App, HttpServer, web};
use std::env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let pg_pool = db::connection_pool(&database_url)
        .await
        .expect("Failed to create postgres pool");
    let redis_pool = redis_client::create_pool(&redis_url);

    println!("Starting wallet-service on 127.0.0.1:8082");
    HttpServer::new(move || {
        App::new()
            .wrap(actix_web::middleware::from_fn(utils::auth::api_key_middleware))
            .wrap(actix_web::middleware::from_fn(utils::ratelimit::rate_limit_middleware))
            .app_data(web::Data::new(utils::ratelimit::RateLimit::new("wallet", 10, 1)))
            .app_data(web::Data::new(pg_pool.clone()))
            .app_data(web::Data::new(redis_pool.clone()))
            .configure(wallet_service::routes::init)
    })
    .bind(("127.0.0.1", 8082))?
    .run()
    .await
}
