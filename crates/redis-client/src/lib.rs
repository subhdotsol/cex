use chrono::Utc;
use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::{Config, Pool, Runtime};
use types::BalanceUpdate;
use uuid::Uuid;

pub fn create_pool(url: &str) -> Pool {
    let cfg = Config::from_url(url);
    cfg.create_pool(Some(Runtime::Tokio1))
        .expect("Failed to create redis pool")
}

/// Refreshes the cached available balance (`balance:{user}:{asset}`) and notifies
/// websocket subscribers on `balance:{user}`. Best effort: Postgres is the source of truth.
pub async fn publish_balance(
    pool: &Pool,
    user_id: Uuid,
    asset: &str,
    available: String,
    change_type: &str,
) {
    let Ok(mut conn) = pool.get().await else {
        eprintln!("Redis unavailable, balance cache not updated");
        return;
    };
    let _: Result<(), _> = conn
        .set(format!("balance:{}:{}", user_id, asset), &available)
        .await;

    let update = BalanceUpdate {
        user_id,
        asset: asset.to_string(),
        balance: available,
        change_type: change_type.to_string(),
        timestamp: Utc::now(),
    };
    if let Ok(payload) = serde_json::to_string(&update) {
        let _: Result<(), _> = conn.publish(format!("balance:{}", user_id), payload).await;
    }
}
