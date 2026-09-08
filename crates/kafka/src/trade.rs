use crate::events::ENGINE_EVENTS;
use crate::produce_message;
use chrono::{DateTime, Utc};
use rdkafka::producer::FutureProducer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single fill between a resting (maker) order and an incoming (taker) order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeExecuted {
    pub trade_id: Uuid,
    pub pair: String,
    pub price: String,
    pub qty: String,
    pub maker_order_id: Uuid,
    pub taker_order_id: Uuid,
    pub maker_user_id: Uuid,
    pub taker_user_id: Uuid,
    /// "BUY" or "SELL" - side of the taker
    pub taker_side: String,
    pub executed_at: DateTime<Utc>,
}

/// Why an order left the book without being fully filled.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum CloseReason {
    /// user cancelled a resting order
    Cancelled,
    /// IOC / MARKET order whose unfilled remainder was dropped
    Expired,
    /// FOK order that could not be filled completely
    Rejected,
}

/// Order removed from the matching engine with an unfilled remainder.
/// Settlement uses this to release whatever funds are still locked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderClosed {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub pair: String,
    pub reason: CloseReason,
    pub remaining_qty: String,
    pub closed_at: DateTime<Utc>,
}

/// Everything the matching engine emits, in the exact order it happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EngineEvent {
    Trade(TradeExecuted),
    OrderClosed(OrderClosed),
}

impl EngineEvent {
    pub fn pair(&self) -> &str {
        match self {
            EngineEvent::Trade(t) => &t.pair,
            EngineEvent::OrderClosed(c) => &c.pair,
        }
    }
}

pub async fn produce_engine_event(
    producer: &FutureProducer,
    event: &EngineEvent,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(event)?;
    produce_message(producer, ENGINE_EVENTS, event.pair(), &payload).await
}
