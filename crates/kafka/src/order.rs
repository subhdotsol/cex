use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use rdkafka::producer::FutureProducer;
use crate::events::ORDER_COMMANDS;
use crate::produce_message;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCancel {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub timestamp: DateTime<Utc>,
}

/// Everything the matching engine is asked to do, in submission order per pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderCommand {
    New(OrderNew),
    Cancel(OrderCancel),
}

impl OrderCommand {
    pub fn pair(&self) -> &str {
        match self {
            OrderCommand::New(o) => &o.pair,
            OrderCommand::Cancel(c) => &c.pair,
        }
    }
}

pub async fn produce_command(producer: &FutureProducer, cmd: &OrderCommand) -> anyhow::Result<()> {
    let payload = serde_json::to_string(cmd)?;
    produce_message(producer, ORDER_COMMANDS, cmd.pair(), &payload).await
}

pub async fn produce_order_new(producer: &FutureProducer, order: &OrderNew) -> anyhow::Result<()> {
    produce_command(producer, &OrderCommand::New(order.clone())).await
}

pub async fn produce_order_cancel(
    producer: &FutureProducer,
    order: &OrderCancel,
) -> anyhow::Result<()> {
    produce_command(producer, &OrderCommand::Cancel(order.clone())).await
}
