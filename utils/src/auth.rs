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
