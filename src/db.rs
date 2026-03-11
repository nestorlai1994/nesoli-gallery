use sqlx::{postgres::PgPoolOptions, PgPool};

pub async fn create_pool(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("failed to connect to Postgres")
}
