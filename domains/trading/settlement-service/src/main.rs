// Settlement service
//
// Consumes `engine.events` from the matching engine and makes them durable:
//   Trade        -> record trade, update both orders, move funds between buyer and seller
//   OrderClosed  -> mark order CANCELLED / EXPIRED / REJECTED and release its reserved funds
//
// Offsets are committed only after the Postgres transaction commits (at-least-once),
// and every handler is idempotent so redelivery is safe.

mod accounting;
mod settle;

use futures::StreamExt;
use kafka::events::ENGINE_EVENTS;
use kafka::trade::EngineEvent;
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::{ClientConfig, Message};
use sqlx::PgPool;
use std::env;
use std::time::Duration;

async fn handle(pool: &PgPool, event: &EngineEvent) -> anyhow::Result<settle::Changes> {
    match event {
        EngineEvent::Trade(t) => settle::settle_trade(pool, t).await,
        EngineEvent::OrderClosed(c) => settle::settle_close(pool, c).await,
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let brokers = env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string());

    let pool = db::connection_pool(&database_url)
        .await
        .expect("Failed to create postgres pool");

    let consumer: StreamConsumer = ClientConfig::new()
        .set("group.id", "settlement-service")
        .set("bootstrap.servers", &brokers)
        .set("enable.auto.commit", "false")
        .set("auto.offset.reset", "earliest")
        .create()
        .expect("Kafka consumer creation failed");
    consumer
        .subscribe(&[ENGINE_EVENTS])
        .expect("Kafka subscribe failed");

    println!("settlement-service consuming {ENGINE_EVENTS}");

    let mut stream = consumer.stream();
    while let Some(result) = stream.next().await {
        let msg = match result {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Kafka error: {:?}", e);
                continue;
            }
        };

        let event: Option<EngineEvent> = msg
            .payload_view::<str>()
            .and_then(|r| r.ok())
            .and_then(|s| match serde_json::from_str(s) {
                Ok(e) => Some(e),
                Err(err) => {
                    eprintln!("Skipping malformed engine event: {err:?} payload={s}");
                    None
                }
            });

        if let Some(event) = event {
            // Retry until it sticks: skipping an event would corrupt balances.
            let mut backoff = Duration::from_millis(100);
            loop {
                match handle(&pool, &event).await {
                    Ok(_changes) => break,
                    Err(e) => {
                        eprintln!("Settlement failed, retrying in {:?}: {e:?}", backoff);
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                }
            }
        }

        if let Err(e) = consumer.commit_message(&msg, CommitMode::Async) {
            eprintln!("Offset commit failed: {:?}", e);
        }
    }
}
