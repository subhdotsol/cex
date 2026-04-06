use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterResponse {
    pub user_id: Uuid,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub expires_in: u64,
}
