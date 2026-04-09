use core::fmt;

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

#[derive(Debug, Deserialize, Serialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LogoutResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType {
    Limit,
    Market,
    Ioc,
    Fok,
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Rejected,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderRequest {
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Option<String>,
    pub qty: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderResponse {
    pub order_id: Uuid,
    pub status: OrderStatus,
    pub pair: String,
    pub side: OrderSide,
    pub price: Option<String>,
    pub qty: String,
    pub filled_qty: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DepositRequest {
    pub asset: String,
    pub amount: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WithdrawRequest {
    pub asset: String,
    pub amount: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetAllOrdersRequest {
    pub status: Option<OrderStatus>,
    pub pair: Option<String>,
    pub limit: Option<usize>,
    pub before: Option<Uuid>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PaginatedOrders {
    pub orders: Vec<OrderResponse>,
    pub next_cursor: Option<Uuid>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderHealthCheckResponse {
    pub status: String,
    pub service: String,
    pub version: f64,
}
