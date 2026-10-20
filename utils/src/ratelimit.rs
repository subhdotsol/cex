// Fixed-window rate limiting backed by Redis (shared across service replicas).
//
// App::new()
//     .app_data(web::Data::new(RateLimit::new("orders", 20, 1)))   // 20 req / second
//     .wrap(actix_web::middleware::from_fn(utils::ratelimit::rate_limit_middleware))
//
// Caller identity: X-API-KEY, else the JWT subject, else the peer IP.
// Requires `web::Data<deadpool_redis::Pool>`. Fails open if Redis is unavailable.

use actix_web::body::{EitherBody, MessageBody};
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{HttpResponse, web};
use deadpool_redis::Pool as RedisPool;
use deadpool_redis::redis;

#[derive(Debug, Clone)]
pub struct RateLimit {
    /// key namespace, e.g. "orders"
    pub scope: &'static str,
    pub max_requests: u64,
    pub window_secs: u64,
}

impl RateLimit {
    pub fn new(scope: &'static str, max_requests: u64, window_secs: u64) -> Self {
        Self { scope, max_requests, window_secs }
    }
}

fn identity(req: &ServiceRequest) -> String {
    if let Some(key) = req
        .headers()
        .get(crate::auth::HEADER_API_KEY)
        .and_then(|h| h.to_str().ok())
    {
        return format!("key:{key}");
    }
    let bearer = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if let (Some(token), Ok(secret)) = (bearer, std::env::var("TOKEN_SECRET")) {
        if let Ok(claims) = crate::jwt::verify_token(token, &secret) {
            return format!("user:{}", claims.sub);
        }
    }
    let ip = req
        .connection_info()
        .realip_remote_addr()
        .unwrap_or("unknown")
        .to_string();
    format!("ip:{ip}")
}

/// Key for the current window and seconds until it resets.
pub fn window_key(scope: &str, identity: &str, window_secs: u64, now_secs: u64) -> (String, u64) {
    let window = now_secs / window_secs;
    let reset_in = window_secs - (now_secs % window_secs);
    (format!("rl:{scope}:{identity}:{window}"), reset_in)
}

pub async fn rate_limit_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<EitherBody<impl MessageBody>>, actix_web::Error> {
    let (Some(limit), Some(pool)) = (
        req.app_data::<web::Data<RateLimit>>().cloned(),
        req.app_data::<web::Data<RedisPool>>().cloned(),
    ) else {
        return next.call(req).await.map(ServiceResponse::map_into_left_body);
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (key, reset_in) = window_key(limit.scope, &identity(&req), limit.window_secs, now);

    let count: Option<u64> = match pool.get().await {
        Ok(mut conn) => redis::pipe()
            .atomic()
            .incr(&key, 1)
            .expire(&key, (limit.window_secs + 1) as i64)
            .ignore()
            .query_async::<_, (u64,)>(&mut conn)
            .await
            .map(|(c,)| c)
            .map_err(|e| eprintln!("rate limiter redis error: {e:?}"))
            .ok(),
        Err(e) => {
            eprintln!("rate limiter redis unavailable: {e:?}");
            None
        }
    };

    match count {
        Some(c) if c > limit.max_requests => {
            let resp = HttpResponse::TooManyRequests()
                .insert_header(("Retry-After", reset_in.to_string()))
                .insert_header(("X-RateLimit-Limit", limit.max_requests.to_string()))
                .insert_header(("X-RateLimit-Remaining", "0"))
                .body("rate limit exceeded");
            Ok(req.into_response(resp).map_into_right_body())
        }
        _ => {
            let remaining = count.map(|c| limit.max_requests.saturating_sub(c));
            let mut res = next.call(req).await?;
            if let Some(remaining) = remaining {
                let headers = res.headers_mut();
                if let (Ok(l), Ok(r)) = (
                    limit.max_requests.to_string().parse(),
                    remaining.to_string().parse(),
                ) {
                    headers.insert(actix_web::http::header::HeaderName::from_static("x-ratelimit-limit"), l);
                    headers.insert(actix_web::http::header::HeaderName::from_static("x-ratelimit-remaining"), r);
                }
            }
            Ok(res.map_into_left_body())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        let (k1, reset) = window_key("orders", "user:1", 10, 1_000_003);
        assert_eq!(k1, "rl:orders:user:1:100000");
        assert_eq!(reset, 7);
        let (k2, _) = window_key("orders", "user:1", 10, 1_000_010);
        assert_ne!(k1, k2);
    }
}
