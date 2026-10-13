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
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::{ClientConfig, Message, Offset, TopicPartitionList};
use std::collections::HashMap;
use std::time::Duration;

/// Partition -> first offset that has NOT been processed yet (committed offset).
async fn committed_positions(consumer: &StreamConsumer) -> (Vec<i32>, HashMap<i32, i64>) {
    loop {
        let partitions: Vec<i32> = match consumer.fetch_metadata(Some(ORDER_COMMANDS), Duration::from_secs(10)) {
            Ok(md) => md
                .topics()
                .iter()
                .filter(|t| t.error().is_none())
                .flat_map(|t| t.partitions().iter().map(|p| p.id()))
                .collect(),
            Err(e) => {
                eprintln!("Metadata fetch failed: {:?}", e);
                vec![]
            }
        };
        if partitions.is_empty() {
            eprintln!("Topic {ORDER_COMMANDS} not available yet (run `make topics`), retrying...");
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        let mut tpl = TopicPartitionList::new();
        for p in &partitions {
            tpl.add_partition(ORDER_COMMANDS, *p);
        }
        let committed = consumer
            .committed_offsets(tpl, Duration::from_secs(10))
            .expect("Failed to read committed offsets");
        let positions = committed
            .elements()
            .iter()
            .filter_map(|e| match e.offset() {
                Offset::Offset(o) => Some((e.partition(), o)),
                _ => None,
            })
            .collect();
        return (partitions, positions);
    }
}

/// The order book lives only in memory. On startup it is rebuilt by replaying
/// `orders.commands` from the beginning: everything before the committed offset is
/// applied silently (its events were already published), everything after is live.
/// Offsets are committed only after a command's events have been produced.
pub async fn run_consumer(
    brokers: &str,
    state: SharedState,
    producer: FutureProducer,
    redis_pool: RedisPool,
) {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("group.id", "matching-engine-group")
        .set("bootstrap.servers", brokers)
        .set("enable.auto.commit", "false")
        .create()
        .expect("Kafka consumer creation failed");

    let (partitions, replay_until) = committed_positions(&consumer).await;
    let mut replaying: HashMap<i32, i64> = replay_until
        .into_iter()
        .filter(|(_, offset)| *offset > 0)
        .collect();
    println!("Replaying {ORDER_COMMANDS} up to {:?}", replaying);

    // single engine instance owns every partition: assign instead of subscribe
    let mut assignment = TopicPartitionList::new();
    for p in &partitions {
        assignment
            .add_partition_offset(ORDER_COMMANDS, *p, Offset::Beginning)
            .expect("invalid partition offset");
    }
    consumer.assign(&assignment).expect("Kafka assign failed");

    if replaying.is_empty() {
        println!("Nothing to replay, live");
    }

    let mut stream = consumer.stream();
    while let Some(result) = stream.next().await {
        let msg = match result {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Kafka consumer error: {:?}", e);
                continue;
            }
        };

        let replay_until = replaying.get(&msg.partition()).copied();
        let live = !matches!(replay_until, Some(until) if msg.offset() < until);

        let cmd: Option<OrderCommand> = match msg.payload_view::<str>().and_then(|r| r.ok()).map(serde_json::from_str) {
            Some(Ok(c)) => Some(c),
            Some(Err(e)) => {
                eprintln!("Skipping malformed order command: {:?}", e);
                None
            }
            None => None,
        };

        match cmd {
            Some(OrderCommand::New(o)) => handle_order_new(o, &state, &producer, &redis_pool, live).await,
            Some(OrderCommand::Cancel(c)) => handle_order_cancel(c, &state, &producer, &redis_pool, live).await,
            None => {}
        }

        if live {
            if let Err(e) = consumer.commit_message(&msg, CommitMode::Sync) {
                eprintln!("Offset commit failed: {:?}", e);
            }
        } else if replay_until.is_some_and(|until| msg.offset() + 1 >= until) {
            replaying.remove(&msg.partition());
            if replaying.is_empty() {
                println!(
                    "Replay finished, live. {} orders in book",
                    state.read().await.orders_in_book()
                );
                publish_all_depths(&state, &redis_pool).await;
            }
        }
    }
}

async fn publish_all_depths(state: &SharedState, redis_pool: &RedisPool) {
    let pairs: Vec<String> = state.read().await.books.keys().cloned().collect();
    for pair in pairs {
        publish_depth_to_redis(&pair, state, redis_pool).await;
    }
}

async fn handle_order_new(
    event: OrderNew,
    state: &SharedState,
    producer: &FutureProducer,
    redis_pool: &RedisPool,
    live: bool,
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
    if !live {
        return;
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
    live: bool,
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
    if !live {
        return;
    }
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
