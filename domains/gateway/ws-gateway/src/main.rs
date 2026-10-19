// Websocket gateway
//
// One Redis pattern subscription per gateway process fans out to every connected client
// through a tokio broadcast channel; each session forwards only the channels it subscribed to.
// Gateways are stateless, so they scale horizontally behind a load balancer.
//
// Connect:  ws://host:8085/ws                 public channels only
//           ws://host:8085/ws?token=<jwt>     + private `balance` and `orders` channels
//           (or `Authorization: Bearer <jwt>` for non-browser clients)
//
// Client -> server:
//   {"op":"subscribe","channel":"depth:BTC-USDT"}
//   {"op":"subscribe","channel":"balance"}
//   {"op":"unsubscribe","channel":"trades:BTC-USDT"}
//
// Server -> client:
//   {"channel":"depth:BTC-USDT","data":{...}}
//   {"type":"subscribed","channel":"..."} / {"type":"error","message":"..."}

mod session;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, get, web};
use futures::StreamExt;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// (redis channel, raw json payload)
pub type Event = Arc<(String, String)>;

pub struct Hub {
    pub tx: broadcast::Sender<Event>,
    pub redis: deadpool_redis::Pool,
}

/// Redis channels relayed to websocket clients.
const PATTERNS: [&str; 4] = ["depth:*", "trades:*", "balance:*", "orders:*"];

async fn relay_redis(redis_url: String, tx: broadcast::Sender<Event>) {
    loop {
        let result: redis::RedisResult<()> = async {
            let client = redis::Client::open(redis_url.as_str())?;
            let mut pubsub = client.get_async_pubsub().await?;
            for p in PATTERNS {
                pubsub.psubscribe(p).await?;
            }
            println!("Relaying redis channels {:?}", PATTERNS);
            let mut messages = pubsub.into_on_message();
            while let Some(msg) = messages.next().await {
                let Ok(payload) = msg.get_payload::<String>() else {
                    continue;
                };
                // Err only means nobody is connected right now
                let _ = tx.send(Arc::new((msg.get_channel_name().to_string(), payload)));
            }
            Ok(())
        }
        .await;

        eprintln!("Redis pubsub ended ({:?}), reconnecting in 1s", result.err());
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[derive(serde::Deserialize)]
struct WsQuery {
    token: Option<String>,
}

#[get("/ws")]
async fn ws(
    req: HttpRequest,
    body: web::Payload,
    query: web::Query<WsQuery>,
    hub: web::Data<Hub>,
) -> Result<HttpResponse, actix_web::Error> {
    let header_token = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned);

    // Reject bad credentials before upgrading; no credentials = anonymous session.
    let user = match query.token.clone().or(header_token) {
        Some(token) => {
            let secret = env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");
            match utils::verify_token(&token, &secret) {
                Ok(claims) => Some(session::SessionUser { id: claims.sub, expires_at: claims.exp }),
                Err(_) => return Ok(HttpResponse::Unauthorized().body("Invalid or expired token")),
            }
        }
        None => None,
    };

    let (response, session, stream) = actix_ws::handle(&req, body)?;
    actix_web::rt::spawn(session::run(session, stream, hub.into_inner(), user));
    Ok(response)
}

#[get("/health")]
async fn health(hub: web::Data<Hub>) -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
        "connections": hub.tx.receiver_count(),
    }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let port: u16 = env::var("WS_GATEWAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8085);

    let (tx, _) = broadcast::channel::<Event>(4096);
    tokio::spawn(relay_redis(redis_url.clone(), tx.clone()));

    let hub = web::Data::new(Hub {
        tx,
        redis: redis_client::create_pool(&redis_url),
    });

    println!("Starting ws-gateway on 0.0.0.0:{port}");
    HttpServer::new(move || App::new().app_data(hub.clone()).service(ws).service(health))
        .bind(("0.0.0.0", port))?
        .run()
        .await
}
