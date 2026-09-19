// Fan-out after a settlement commits. Redis is a cache + pub/sub bus here,
// Postgres stays the source of truth, so failures are only logged.
//
// keys:      balance:{user_id}:{asset}  -> available balance (string)
// channels:  balance:{user_id}          -> BalanceUpdate json
//            orders:{user_id}           -> order status json
//            trades:{pair}              -> public trade json

use crate::settle::Changes;
use chrono::Utc;
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis::AsyncCommands;
use std::collections::HashMap;
use types::{BalanceUpdate, OrderStatus};

pub async fn publish(redis: &RedisPool, changes: &Changes) {
    let mut conn = match redis.get().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Redis unavailable, skipping notifications: {e:?}");
            return;
        }
    };

    // Several ledger steps can touch the same balance; only the final state matters.
    let mut latest = HashMap::new();
    for b in &changes.balances {
        latest.insert((b.user_id, b.asset.clone()), b);
    }

    for ((user_id, asset), b) in latest {
        let available = b.available.normalize().to_string();
        let _: Result<(), _> = conn
            .set(format!("balance:{}:{}", user_id, asset), &available)
            .await;

        let update = BalanceUpdate {
            user_id,
            asset,
            balance: available,
            change_type: "SETTLEMENT".to_string(),
            timestamp: Utc::now(),
        };
        if let Ok(payload) = serde_json::to_string(&update) {
            let _: Result<(), _> = conn.publish(format!("balance:{}", user_id), payload).await;
        }
    }

    for o in &changes.orders {
        let payload = serde_json::json!({
            "order_id": o.order_id,
            "pair": o.pair,
            "status": OrderStatus::from(o.status),
            "filled_qty": o.filled_qty.normalize().to_string(),
            "timestamp": Utc::now(),
        });
        let _: Result<(), _> = conn
            .publish(format!("orders:{}", o.user_id), payload.to_string())
            .await;
    }

    if let Some(t) = &changes.trade {
        let payload = serde_json::json!({
            "trade_id": t.trade_id,
            "pair": t.pair,
            "price": t.price,
            "qty": t.qty,
            "side": t.taker_side,
            "executed_at": t.executed_at,
        });
        let _: Result<(), _> = conn
            .publish(format!("trades:{}", t.pair), payload.to_string())
            .await;
    }
}
