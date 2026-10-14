use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Public view of a key (never includes the secret).
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ApiKeyInfo {
    pub key: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Row needed to authenticate a request.
#[derive(Debug, Clone, FromRow)]
pub struct ApiKeySecret {
    pub user_id: Uuid,
    pub secret_ciphertext: Vec<u8>,
    pub secret_nonce: Vec<u8>,
}

pub const MAX_KEYS_PER_USER: i64 = 10;

pub async fn create_api_key(
    pool: &PgPool,
    user_id: Uuid,
    key: &str,
    secret_ciphertext: &[u8],
    secret_nonce: &[u8],
    label: Option<&str>,
) -> Result<ApiKeyInfo, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyInfo>(
        r#"
        INSERT INTO api_keys (id, user_id, key, secret_ciphertext, secret_nonce, label)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING key, label, created_at, last_used_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(key)
    .bind(secret_ciphertext)
    .bind(secret_nonce)
    .bind(label)
    .fetch_one(pool)
    .await
}

pub async fn count_active(pool: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id)
        .fetch_one(pool)
        .await
}

pub async fn list_api_keys(pool: &PgPool, user_id: Uuid) -> Result<Vec<ApiKeyInfo>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyInfo>(
        r#"
        SELECT key, label, created_at, last_used_at FROM api_keys
        WHERE user_id = $1 AND revoked_at IS NULL
        ORDER BY created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Returns false if the key does not exist, belongs to someone else or is already revoked.
pub async fn revoke_api_key(pool: &PgPool, user_id: Uuid, key: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE api_keys SET revoked_at = NOW() WHERE key = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(key)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

pub async fn find_active_secret(pool: &PgPool, key: &str) -> Result<Option<ApiKeySecret>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeySecret>(
        r#"
        SELECT user_id, secret_ciphertext, secret_nonce FROM api_keys
        WHERE key = $1 AND revoked_at IS NULL
        "#,
    )
    .bind(key)
    .fetch_optional(pool)
    .await
}

pub async fn touch_last_used(pool: &PgPool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE api_keys SET last_used_at = NOW() WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}
