pub mod order;

pub use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::ClientConfig;
use std::time::Duration;

pub fn create_producer(brokers: &str) -> FutureProducer {
    ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("message.timeout.ms", "5000")
        .create()
        .expect("Producer creation error")
}

pub async fn produce_message(
    producer: &FutureProducer,
    topic: &str,
    key: &str,
    payload: &str,
) -> anyhow::Result<()> {
    producer
        .send(
            FutureRecord::to(topic).key(key).payload(payload),
            Duration::from_secs(0),
        )
        .await
        .map_err(|(e, _)| anyhow::anyhow!("Kafka send error: {:?}", e))?;
    Ok(())
}
