use crate::{Event, Hub};
use actix_ws::{AggregatedMessage, Session};
use deadpool_redis::redis::AsyncCommands;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const PAIRS: [&str; 2] = ["BTC-USDT", "ETH-USDT"];
const MAX_SUBSCRIPTIONS: usize = 20;
const HEARTBEAT: Duration = Duration::from_secs(15);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum ClientMsg {
    Subscribe { channel: String },
    Unsubscribe { channel: String },
}

#[derive(Debug, Clone, Copy)]
pub struct SessionUser {
    pub id: Uuid,
    /// unix seconds, session is closed once the token expires
    pub expires_at: i64,
}

/// Maps a client channel name to the Redis channel it reads from.
/// Private channels resolve to the authenticated user's own channel only.
fn resolve_channel(channel: &str, user: Option<&SessionUser>) -> Result<String, String> {
    if channel == "balance" || channel == "orders" {
        return match user {
            Some(u) => Ok(format!("{channel}:{}", u.id)),
            None => Err(format!("{channel} requires authentication, connect with ?token=<jwt>")),
        };
    }
    let (kind, pair) = channel
        .split_once(':')
        .ok_or_else(|| format!("unknown channel {channel}"))?;
    match kind {
        "depth" | "trades" if PAIRS.contains(&pair) => Ok(channel.to_string()),
        "depth" | "trades" => Err(format!("unknown pair {pair}")),
        _ => Err(format!("unknown channel {channel}")),
    }
}

async fn send_json(session: &mut Session, value: serde_json::Value) -> bool {
    session.text(value.to_string()).await.is_ok()
}

/// Sends the cached depth snapshot so the client does not wait for the next book change.
async fn send_snapshot(session: &mut Session, hub: &Hub, redis_channel: &str) -> bool {
    if !redis_channel.starts_with("depth:") {
        return true;
    }
    let Ok(mut conn) = hub.redis.get().await else {
        return true;
    };
    let snapshot: Option<String> = conn.get(redis_channel).await.unwrap_or(None);
    match snapshot.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        Some(data) => send_json(session, serde_json::json!({ "channel": redis_channel, "data": data })).await,
        None => true,
    }
}

pub async fn run(
    mut session: Session,
    stream: actix_ws::MessageStream,
    hub: Arc<Hub>,
    user: Option<SessionUser>,
) {
    let mut stream = stream.aggregate_continuations().max_continuation_size(64 * 1024);
    let mut events = hub.tx.subscribe();
    // redis channel -> name the client subscribed with
    let mut subscriptions: HashMap<String, String> = HashMap::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut last_seen = Instant::now();

    let close_reason = loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break None };
                last_seen = Instant::now();
                match msg {
                    AggregatedMessage::Text(text) => {
                        let reply = match serde_json::from_str::<ClientMsg>(&text) {
                            Ok(ClientMsg::Subscribe { channel }) => match resolve_channel(&channel, user.as_ref()) {
                                Ok(_) if subscriptions.len() >= MAX_SUBSCRIPTIONS => {
                                    serde_json::json!({ "type": "error", "message": "too many subscriptions" })
                                }
                                Ok(redis_channel) => {
                                    subscriptions.insert(redis_channel.clone(), channel.clone());
                                    if !send_json(&mut session, serde_json::json!({ "type": "subscribed", "channel": channel })).await
                                        || !send_snapshot(&mut session, &hub, &redis_channel).await
                                    {
                                        break None;
                                    }
                                    continue;
                                }
                                Err(e) => serde_json::json!({ "type": "error", "message": e }),
                            },
                            Ok(ClientMsg::Unsubscribe { channel }) => {
                                if let Ok(redis_channel) = resolve_channel(&channel, user.as_ref()) {
                                    subscriptions.remove(&redis_channel);
                                }
                                serde_json::json!({ "type": "unsubscribed", "channel": channel })
                            }
                            Err(_) => serde_json::json!({ "type": "error", "message": "invalid message" }),
                        };
                        if !send_json(&mut session, reply).await {
                            break None;
                        }
                    }
                    AggregatedMessage::Ping(bytes) => {
                        if session.pong(&bytes).await.is_err() {
                            break None;
                        }
                    }
                    AggregatedMessage::Close(reason) => break reason,
                    _ => {}
                }
            }

            event = events.recv() => {
                let event: Event = match event {
                    Ok(e) => e,
                    // slow client: drop what it missed, keep the connection
                    Err(RecvError::Lagged(n)) => {
                        let _ = send_json(&mut session, serde_json::json!({ "type": "lagged", "missed": n })).await;
                        continue;
                    }
                    Err(RecvError::Closed) => break None,
                };
                let (channel, payload) = (&event.0, &event.1);
                let Some(client_channel) = subscriptions.get(channel) else {
                    continue;
                };
                let frame = format!(r#"{{"channel":{},"data":{}}}"#, serde_json::Value::from(client_channel.as_str()), payload);
                if session.text(frame).await.is_err() {
                    break None;
                }
            }

            _ = heartbeat.tick() => {
                if last_seen.elapsed() > CLIENT_TIMEOUT {
                    break None;
                }
                if user.is_some_and(|u| u.expires_at <= chrono_now()) {
                    break Some(actix_ws::CloseReason {
                        code: actix_ws::CloseCode::Policy,
                        description: Some("token expired".into()),
                    });
                }
                if session.ping(b"").await.is_err() {
                    break None;
                }
            }
        }
    };

    let _ = session.close(close_reason).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_resolution() {
        assert_eq!(resolve_channel("depth:BTC-USDT", None).unwrap(), "depth:BTC-USDT");
        assert!(resolve_channel("depth:DOGE-USDT", None).is_err());
        assert!(resolve_channel("candles:BTC-USDT", None).is_err());
        assert!(resolve_channel("depth", None).is_err());
    }

    #[test]
    fn private_channels_are_scoped_to_the_user() {
        assert!(resolve_channel("balance", None).is_err());
        let u = SessionUser { id: Uuid::nil(), expires_at: i64::MAX };
        assert_eq!(resolve_channel("balance", Some(&u)).unwrap(), format!("balance:{}", Uuid::nil()));
        assert_eq!(resolve_channel("orders", Some(&u)).unwrap(), format!("orders:{}", Uuid::nil()));
        // cannot read someone else's channel by naming it directly
        assert!(resolve_channel(&format!("balance:{}", Uuid::new_v4()), Some(&u)).is_err());
    }

    #[test]
    fn parses_client_messages() {
        let m: ClientMsg = serde_json::from_str(r#"{"op":"subscribe","channel":"trades:ETH-USDT"}"#).unwrap();
        assert!(matches!(m, ClientMsg::Subscribe { channel } if channel == "trades:ETH-USDT"));
    }
}
