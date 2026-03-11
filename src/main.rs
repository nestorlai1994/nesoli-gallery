use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use std::env;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nesoli_gallery=debug,tower_http=info".into()),
        )
        .init();

    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{port}");

    let app = Router::new().route("/health", get(health));

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("nesoli-gallery listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<Value> {
    Json(json!({
        "status":  "ok",
        "service": "nesoli-gallery",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
