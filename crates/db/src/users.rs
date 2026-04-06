// all user realted functions here

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_user(pool: &PgPool, email: &str, password: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO users (id, email, password, balance, kyc_status, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        Uuid::new_v4(),
        email,
        password,
        0_i64,
        "pending",
        Utc::now()
    )
    .execute(pool)
    .await?;

    Ok(())
}
