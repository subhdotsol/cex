// Kafka topic names shared by every service.

/// order-service -> matching-engine: new orders and cancels (`OrderCommand`).
/// One topic keyed by pair, so a cancel can never overtake the order it cancels.
pub const ORDER_COMMANDS: &str = "orders.commands";
/// matching-engine -> settlement-service (and anyone else interested).
/// Keyed by pair so that all events of one book stay ordered on a single partition.
pub const ENGINE_EVENTS: &str = "engine.events";

pub const ALL_TOPICS: [&str; 2] = [ORDER_COMMANDS, ENGINE_EVENTS];
