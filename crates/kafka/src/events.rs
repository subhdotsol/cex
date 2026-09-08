// Kafka topic names shared by every service.

/// order-service -> matching-engine
pub const ORDERS_NEW: &str = "orders.new";
/// order-service -> matching-engine
pub const ORDERS_CANCEL: &str = "orders.cancel";
/// matching-engine -> settlement-service (and anyone else interested).
/// Keyed by pair so that all events of one book stay ordered on a single partition.
pub const ENGINE_EVENTS: &str = "engine.events";
