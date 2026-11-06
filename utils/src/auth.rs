// Request authentication shared by every HTTP service.
//
// Handlers take `auth: AuthUser` instead of parsing headers themselves:
//
//     async fn handler(auth: AuthUser) -> impl Responder { let user_id = auth.0; ... }
//
// Resolution order:
//   1. an AuthUser already placed in request extensions (e.g. by API-key middleware)
//   2. `Authorization: Bearer <jwt>`

use actix_web::{FromRequest, HttpMessage, HttpRequest, HttpResponse, ResponseError, dev::Payload};
use std::fmt;
use std::future::{Ready, ready};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuthUser(pub Uuid);

#[derive(Debug)]
pub enum AuthError {
    MissingHeader,
    BadFormat,
    InvalidToken,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            AuthError::MissingHeader => "Missing Authorization header",
            AuthError::BadFormat => "Invalid Authorization header format",
            AuthError::InvalidToken => "Invalid or expired token",
        };
        f.write_str(msg)
    }
}

impl ResponseError for AuthError {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::Unauthorized().body(self.to_string())
    }
}

/// Validates an `Authorization` header value and returns the user id.
pub fn user_from_bearer(header: Option<&str>, secret: &str) -> Result<Uuid, AuthError> {
    let header = header.ok_or(AuthError::MissingHeader)?;
    let token = header.strip_prefix("Bearer ").ok_or(AuthError::BadFormat)?;
    crate::jwt::verify_token(token, secret)
        .map(|c| c.sub)
        .map_err(|_| AuthError::InvalidToken)
}

impl FromRequest for AuthUser {
    type Error = AuthError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        if let Some(user) = req.extensions().get::<AuthUser>() {
            return ready(Ok(*user));
        }
        let secret = std::env::var("TOKEN_SECRET").expect("TOKEN_SECRET must be set");
        let header = req
            .headers()
            .get("Authorization")
            .and_then(|h| h.to_str().ok());
        ready(user_from_bearer(header, &secret).map(AuthUser))
    }
}

// ---------------------------------------------------------------------------
// API key (HMAC) authentication middleware
// ---------------------------------------------------------------------------
//
// App::new().wrap(actix_web::middleware::from_fn(utils::auth::api_key_middleware))
//
// Requests without X-API-KEY pass through untouched (JWT path). Signed requests are
// verified and the resolved user is stored in extensions, where `AuthUser` picks it up.
// Needs `web::Data<PgPool>` registered on the app and API_KEY_ENCRYPTION_KEY set.

use actix_web::body::{EitherBody, MessageBody};
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::web;
use sqlx::PgPool;

pub const HEADER_API_KEY: &str = "X-API-KEY";
pub const HEADER_SIGNATURE: &str = "X-SIGNATURE";
pub const HEADER_TIMESTAMP: &str = "X-TIMESTAMP";

fn header<'a>(req: &'a ServiceRequest, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|h| h.to_str().ok())
}

fn reject<B>(req: ServiceRequest, msg: &'static str) -> ServiceResponse<EitherBody<B>> {
    req.into_response(HttpResponse::Unauthorized().body(msg))
        .map_into_right_body()
}

pub async fn api_key_middleware(
    mut req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<EitherBody<impl MessageBody>>, actix_web::Error> {
    let Some(key) = header(&req, HEADER_API_KEY).map(str::to_owned) else {
        return next.call(req).await.map(ServiceResponse::map_into_left_body);
    };
    let (Some(signature), Some(timestamp)) = (
        header(&req, HEADER_SIGNATURE).map(str::to_owned),
        header(&req, HEADER_TIMESTAMP).map(str::to_owned),
    ) else {
        return Ok(reject(req, "missing X-SIGNATURE or X-TIMESTAMP"));
    };

    if !crate::apikey::timestamp_fresh(&timestamp, chrono::Utc::now().timestamp()) {
        return Ok(reject(req, "stale or invalid X-TIMESTAMP"));
    }

    let Some(pool) = req.app_data::<web::Data<PgPool>>().cloned() else {
        eprintln!("api_key_middleware needs web::Data<PgPool>");
        return Ok(reject(req, "api keys not supported here"));
    };
    let enc_key = match std::env::var("API_KEY_ENCRYPTION_KEY")
        .ok()
        .and_then(|k| crate::apikey::parse_encryption_key(&k).ok())
    {
        Some(k) => k,
        None => return Ok(reject(req, "api keys are not configured")),
    };

    let record = match db::api_keys::find_active_secret(&pool, &key).await {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(reject(req, "invalid api key")),
        Err(e) => {
            eprintln!("api key lookup failed: {e:?}");
            return Ok(reject(req, "invalid api key"));
        }
    };
    let Ok(secret) =
        crate::apikey::decrypt_secret(&enc_key, &record.secret_ciphertext, &record.secret_nonce)
    else {
        return Ok(reject(req, "invalid api key"));
    };

    // Read the body to sign it, then put it back for the handler.
    let body = req.extract::<web::Bytes>().await?;
    req.set_payload(body.clone().into());

    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let method = req.method().as_str().to_owned();

    if !crate::apikey::verify_signature(&secret, &timestamp, &method, &path, &body, &signature) {
        return Ok(reject(req, "invalid signature"));
    }

    req.extensions_mut().insert(AuthUser(record.user_id));
    let _ = db::api_keys::touch_last_used(&pool, &key).await;

    next.call(req).await.map(ServiceResponse::map_into_left_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parsing() {
        let secret = "test-secret";
        let user = Uuid::new_v4();
        let token = crate::jwt::generate_access_token(user, secret).unwrap().0;

        assert_eq!(user_from_bearer(Some(&format!("Bearer {token}")), secret).unwrap(), user);
        assert!(matches!(user_from_bearer(None, secret), Err(AuthError::MissingHeader)));
        assert!(matches!(user_from_bearer(Some(&token), secret), Err(AuthError::BadFormat)));
        assert!(matches!(
            user_from_bearer(Some(&format!("Bearer {token}")), "other"),
            Err(AuthError::InvalidToken)
        ));
    }
}
