use crate::matching::{cancel_order, match_order, TradeResult};
use crate::state::{PublicTrade, SharedState, Side};
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use futures::StreamExt;
use kafka::FutureProducer;
use kafka::order::{OrderCancel, OrderNew};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::{ClientConfig, Message};

pub async fn run_consumer(
    brokers: &str,
    state: SharedState,
    producer: FutureProducer,
    redis_pool: RedisPool,
) {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("group.id", "matching-engine-group")
        .set("bootstrap.servers", brokers)
        .set("enable.auto.commit", "true")
        .set("auto.offset.reset", "earliest")
        .create()
        .expect("Kafka consumer creation failed");

    consumer
        .subscribe(&["orders.new", "orders.cancel"])
        .expect("Kafka subscribe failed");

    let mut stream = consumer.stream();

    while let Some(result) = stream.next().await {
        match result {
            Ok(msg) => {
                let topic = msg.topic().to_string();
                let payload = match msg.payload_view::<str>() {
                    Some(Ok(s)) => s.to_string(),
                    _ => continue,
                };
                match topic.as_str() {
                    "orders.new" => {
                        handle_order_new(&payload, &state, &producer, &redis_pool).await;
                    }
                    "orders.cancel" => {
                        handle_order_cancel(&payload, &state).await;
                    }
                    _ => {}
                }
            }
            Err(e) => eprintln!("Kafka consumer error: {:?}", e),
        }
    }
}

async fn handle_order_new(
    payload: &str,
    state: &SharedState,
    producer: &FutureProducer,
    redis_pool: &RedisPool,
) {
    let event: OrderNew = match serde_json::from_str(payload) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to deserialize OrderNew: {:?}", e);
            return;
        }
    };

    let side = match event.side.as_str() {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        _ => {
            eprintln!("Unknown order side: {}", event.side);
            return;
        }
    };

    let price: Option<f64> = event.price.as_ref().and_then(|p| p.parse().ok());
    let qty: f64 = match event.qty.parse() {
        Ok(q) => q,
        Err(_) => return,
    };

    let trades = {
        let mut st = state.write().await;
        let book = match st.books.get_mut(&event.pair) {
            Some(b) => b,
            None => {
                eprintln!("Unknown pair: {}", event.pair);
                return;
            }
        };
        match_order(
            book,
            event.order_id,
            event.user_id,
            &event.pair,
            side,
            &event.order_type,
            price,
            qty,
            event.timestamp,
        )
    };

    if trades.is_empty() {
        return;
    }

    update_market_data(&event.pair, &trades, state).await;
    emit_trade_events(&trades, producer).await;
    publish_depth_to_redis(&event.pair, state, redis_pool).await;
}

async fn handle_order_cancel(payload: &str, state: &SharedState) {
    let event: OrderCancel = match serde_json::from_str(payload) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to deserialize OrderCancel: {:?}", e);
            return;
        }
    };

    let mut st = state.write().await;
    if let Some(book) = st.books.get_mut(&event.pair) {
        cancel_order(book, event.order_id);
    }
}

async fn update_market_data(pair: &str, trades: &[TradeResult], state: &SharedState) {
    let mut st = state.write().await;

    // Update ring buffer (separate borrow scope from ticker)
    {
        let ring = st.recent_trades.entry(pair.to_string()).or_default();
        for trade in trades {
            if ring.len() >= 200 {
                ring.pop_front();
            }
            ring.push_back(PublicTrade {
                price: format!("{:.2}", trade.price),
                qty: format!("{:.8}", trade.qty),
                side: format!("{:?}", trade.taker_side).to_uppercase(),
                executed_at: trade.executed_at,
            });
        }
    }

    // Update 24h ticker (separate borrow scope from ring)
    {
        let ticker = st.ticker.entry(pair.to_string()).or_default();
        for trade in trades {
            if ticker.open_price == 0.0 {
                ticker.open_price = trade.price;
            }
            ticker.last_price = trade.price;
            if ticker.high_24h == 0.0 || trade.price > ticker.high_24h {
                ticker.high_24h = trade.price;
            }
            if ticker.low_24h == 0.0 || trade.price < ticker.low_24h {
                ticker.low_24h = trade.price;
            }
            ticker.volume_24h += trade.qty;
            ticker.updated_at = Utc::now();
        }
    }
}

async fn emit_trade_events(trades: &[TradeResult], producer: &FutureProducer) {
    for trade in trades {
        // Publish to trades topic
        let trade_payload = serde_json::json!({
            "trade_id": trade.trade_id,
            "pair": trade.pair,
            "price": format!("{:.2}", trade.price),
            "qty": format!("{:.8}", trade.qty),
            "maker_order_id": trade.maker_order_id,
            "taker_order_id": trade.taker_order_id,
            "maker_user_id": trade.maker_user_id,
            "taker_user_id": trade.taker_user_id,
            "taker_side": format!("{:?}", trade.taker_side).to_uppercase(),
            "executed_at": trade.executed_at,
        });
        let _ = kafka::produce_message(
            producer,
            "trades",
            &trade.trade_id.to_string(),
            &trade_payload.to_string(),
        )
        .await;

        // Notify order service to update maker status
        let maker_status = if trade.maker_fully_filled { "FILLED" } else { "PARTIALLY_FILLED" };
        let maker_update = serde_json::json!({
            "order_id": trade.maker_order_id,
            "filled_qty": format!("{:.8}", trade.qty),
            "status": maker_status,
        });
        let _ = kafka::produce_message(
            producer,
            "orders.update",
            &trade.maker_order_id.to_string(),
            &maker_update.to_string(),
        )
        .await;

        // Notify order service to update taker status (order service checks total fill)
        let taker_update = serde_json::json!({
            "order_id": trade.taker_order_id,
            "filled_qty": format!("{:.8}", trade.qty),
            "status": "PARTIALLY_FILLED",
        });
        let _ = kafka::produce_message(
            producer,
            "orders.update",
            &trade.taker_order_id.to_string(),
            &taker_update.to_string(),
        )
        .await;
    }
}

async fn publish_depth_to_redis(pair: &str, state: &SharedState, redis_pool: &RedisPool) {
    let snapshot = {
        let st = state.read().await;
        let book = match st.books.get(pair) {
            Some(b) => b,
            None => return,
        };
        let (bids, asks) = book.depth_snapshot(20);
        serde_json::json!({
            "pair": pair,
            "bids": bids.iter().map(|(p, q)| serde_json::json!({ "price": format!("{:.2}", p), "qty": format!("{:.8}", q) })).collect::<Vec<_>>(),
            "asks": asks.iter().map(|(p, q)| serde_json::json!({ "price": format!("{:.2}", p), "qty": format!("{:.8}", q) })).collect::<Vec<_>>(),
            "timestamp": Utc::now(),
        })
    };

    let mut conn = match redis_pool.get().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let _: Result<(), _> = conn
        .set(format!("depth:{}", pair), snapshot.to_string())
        .await;
    // Also publish to channel so ws-gateway can push to subscribers
    let _: Result<(), _> = conn
        .publish(format!("depth:{}", pair), snapshot.to_string())
        .await;
}
