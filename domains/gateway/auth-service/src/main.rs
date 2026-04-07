use actix_web::{App, HttpServer, web};
use std::env;

mod routes;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let pg_pool = db::connection_pool(&database_url)
        .await
        .expect("Failed to create postgres pool");

    let redis_pool = redis_client::create_pool(&redis_url);

    println!("Starting auth-service on 127.0.0.1:8083");
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pg_pool.clone()))
            .app_data(web::Data::new(redis_pool.clone()))
            .configure(routes::init)
    })
    .bind(("127.0.0.1", 8083))?
    .run()
    .await
}
