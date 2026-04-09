use actix_web::{App, HttpServer, web};
use std::env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let kafka_brokers = env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string());

    let pg_pool = db::connection_pool(&database_url)
        .await
        .expect("Failed to create postgres pool");

    let redis_pool = redis_client::create_pool(&redis_url);
    let kafka_producer = kafka::create_producer(&kafka_brokers);

    println!("Starting order-service on 127.0.0.1:8081");
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pg_pool.clone()))
            .app_data(web::Data::new(redis_pool.clone()))
            .app_data(web::Data::new(kafka_producer.clone()))
            .configure(order_service::routes::init)
    })
    .bind(("127.0.0.1", 8081))?
    .run()
    .await
}
