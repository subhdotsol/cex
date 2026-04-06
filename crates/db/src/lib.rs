use sqlx::{PgPool, postgres::PgPoolOptions};

pub async fn connection_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::var;
    #[tokio::test]
    async fn test_database_connection() {
        dotenvy::dotenv().ok();
        let database_url =
            var("DATABASE_URL").expect("CRITICAL DATABASE_URL must be set in .env or system");
        let pool = connection_pool(&database_url)
            .await
            .expect("failed to connect to DB");

        let row: (i32,) = sqlx::query_as("SELECT 1")
            .fetch_one(&pool)
            .await
            .expect("Failed to execute Select 1");
        assert_eq!(row.0, 1);
        println!("Database connection successful");
    }
}
