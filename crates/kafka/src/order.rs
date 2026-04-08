use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use rdkafka::producer::FutureProducer;
use crate::produce_message;

#[derive(Debug, Serialize, Deserialize)]
pub struct OrderNew {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub side: String,
    pub order_type: String,
    pub price: Option<String>,
    pub qty: String,
    pub timestamp: DateTime<Utc>,
}

pub async fn produce_order_new(
    producer: &FutureProducer,
    order: &OrderNew,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(order)?;
    produce_message(producer, "orders.new", &order.order_id.to_string(), &payload).await
}
