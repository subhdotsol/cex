use actix_web::{App, HttpServer, web};
use std::env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let redis_pool = redis_client::create_pool(&redis_url);

    println!("Starting wallet-service on 127.0.0.1:8082");
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(redis_pool.clone()))
            .configure(wallet_service::routes::init)
    })
    .bind(("127.0.0.1", 8082))?
    .run()
    .await
}
