use actix_web::{App, HttpServer, web};
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;

mod consumer;
mod matching;
mod routes;
mod state;

use state::EngineState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let brokers = env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string());
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let port = env::var("MATCHING_ENGINE_PORT").unwrap_or_else(|_| "8083".to_string());

    let supported_pairs = ["BTC-USDT", "ETH-USDT"];
    let engine_state = Arc::new(RwLock::new(EngineState::new(&supported_pairs)));
    let redis_pool = redis_client::create_pool(&redis_url);
    let kafka_producer = kafka::create_producer(&brokers);

    // Spawn Kafka consumer as a background task
    {
        let state = Arc::clone(&engine_state);
        let producer = kafka_producer.clone();
        let pool = redis_pool.clone();
        let brokers_clone = brokers.clone();
        tokio::spawn(async move {
            consumer::run_consumer(&brokers_clone, state, producer, pool).await;
        });
    }

    let state_data = web::Data::new(Arc::clone(&engine_state));

    println!("Matching engine listening on 0.0.0.0:{}", port);
    HttpServer::new(move || {
        App::new()
            .app_data(state_data.clone())
            .configure(routes::init)
    })
    .bind(format!("0.0.0.0:{}", port))
    .expect("Failed to bind port")
    .run()
    .await
    .expect("Server error");
}
