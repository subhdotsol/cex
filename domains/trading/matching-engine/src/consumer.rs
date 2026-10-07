use crate::matching::{cancel_order, match_order, TradeResult};
use crate::state::{PublicTrade, SharedState, Side};
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use futures::StreamExt;
use kafka::FutureProducer;
use kafka::events::ORDER_COMMANDS;
use kafka::order::{OrderCancel, OrderCommand, OrderNew};
use kafka::trade::{
    CloseReason, EngineEvent, OrderClosed, TradeExecuted, produce_engine_event,
};
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
        .subscribe(&[ORDER_COMMANDS])
        .expect("Kafka subscribe failed");

    let mut stream = consumer.stream();

    while let Some(result) = stream.next().await {
        match result {
            Ok(msg) => {
                let cmd: OrderCommand = match msg
                    .payload_view::<str>()
                    .and_then(|r| r.ok())
                    .map(serde_json::from_str)
                {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => {
                        eprintln!("Skipping malformed order command: {:?}", e);
                        continue;
                    }
                    None => continue,
                };
                match cmd {
                    OrderCommand::New(o) => handle_order_new(o, &state, &producer, &redis_pool).await,
                    OrderCommand::Cancel(c) => {
                        handle_order_cancel(c, &state, &producer, &redis_pool).await
                    }
                }
            }
            Err(e) => eprintln!("Kafka consumer error: {:?}", e),
        }
    }
}

async fn handle_order_new(
    event: OrderNew,
    state: &SharedState,
    producer: &FutureProducer,
    redis_pool: &RedisPool,
) {
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

    let outcome = {
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

    let mut events: Vec<EngineEvent> = outcome.trades.iter().map(trade_event).collect();
    if outcome.dropped_qty > 1e-10 {
        events.push(EngineEvent::OrderClosed(OrderClosed {
            order_id: event.order_id,
            user_id: event.user_id,
            pair: event.pair.clone(),
            reason: if outcome.rejected {
                CloseReason::Rejected
            } else {
                CloseReason::Expired
            },
            remaining_qty: format!("{:.8}", outcome.dropped_qty),
            closed_at: Utc::now(),
        }));
    }

    if !outcome.trades.is_empty() {
        update_market_data(&event.pair, &outcome.trades, state).await;
    }
    emit_engine_events(&events, producer).await;
    // a resting limit order changes depth even when nothing traded
    publish_depth_to_redis(&event.pair, state, redis_pool).await;
}

async fn handle_order_cancel(
    event: OrderCancel,
    state: &SharedState,
    producer: &FutureProducer,
    redis_pool: &RedisPool,
) {
    let removed = {
        let mut st = state.write().await;
        st.books
            .get_mut(&event.pair)
            .and_then(|book| cancel_order(book, event.order_id))
    };

    // Not in the book any more (already filled / closed) -> nothing to release.
    let Some((owner, remaining)) = removed else {
        return;
    };
    if owner != event.user_id {
        eprintln!("Cancel for order {} by non-owner {}", event.order_id, event.user_id);
        return;
    }

    let closed = EngineEvent::OrderClosed(OrderClosed {
        order_id: event.order_id,
        user_id: owner,
        pair: event.pair.clone(),
        reason: CloseReason::Cancelled,
        remaining_qty: format!("{:.8}", remaining),
        closed_at: Utc::now(),
    });
    emit_engine_events(&[closed], producer).await;
    publish_depth_to_redis(&event.pair, state, redis_pool).await;
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

async fn emit_engine_events(events: &[EngineEvent], producer: &FutureProducer) {
    // sequential on purpose: settlement relies on the order of events within a pair
    for event in events {
        if let Err(e) = produce_engine_event(producer, event).await {
            eprintln!("Failed to produce engine event {:?}: {:?}", event, e);
        }
    }
}

fn trade_event(trade: &TradeResult) -> EngineEvent {
    EngineEvent::Trade(TradeExecuted {
        trade_id: trade.trade_id,
        pair: trade.pair.clone(),
        price: format!("{:.2}", trade.price),
        qty: format!("{:.8}", trade.qty),
        maker_order_id: trade.maker_order_id,
        taker_order_id: trade.taker_order_id,
        maker_user_id: trade.maker_user_id,
        taker_user_id: trade.taker_user_id,
        taker_side: format!("{:?}", trade.taker_side).to_uppercase(),
        executed_at: trade.executed_at,
    })
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
