mod db;
mod exif;
mod handlers;
mod ingestor;
mod models;
mod processor;
mod storage;
mod watcher;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use std::{env, path::PathBuf, sync::Arc};
use tokio::sync::mpsc;
use uuid::Uuid;
use watcher::{WatchEvent, WatchEventKind};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nesoli_gallery=debug,tower_http=info".into()),
        )
        .init();

    let port      = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let watch_dir = PathBuf::from(env::var("WATCH_DIR").unwrap_or_else(|_| "/photos".to_string()));
    let db_url    = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let owner_id  = env::var("INGEST_OWNER_ID")
        .unwrap_or_else(|_| "00000000-0000-0000-0000-000000000001".to_string());
    let owner_id: Uuid = owner_id.parse().expect("INGEST_OWNER_ID must be a valid UUID");

    let minio_endpoint = env::var("MINIO_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
    let minio_user     = env::var("MINIO_USER").expect("MINIO_USER must be set");
    let minio_pass     = env::var("MINIO_PASS").expect("MINIO_PASS must be set");
    let minio_bucket   = env::var("MINIO_BUCKET").unwrap_or_else(|_| "gallery".to_string());

    let pool = db::create_pool(&db_url).await;
    let s3 = Arc::new(
        storage::S3Storage::init(&minio_endpoint, &minio_user, &minio_pass, &minio_bucket).await,
    );

    // File system watcher pipeline
    let (tx, mut rx) = mpsc::channel::<WatchEvent>(100);

    let watch_dir_clone = watch_dir.clone();
    tokio::task::spawn_blocking(move || {
        watcher::start_watcher(watch_dir_clone, tx);
    });

    // Event consumer — ingest Created/Renamed images into DB + upload to MinIO
    let pool_consumer = pool.clone();
    let s3_consumer = Arc::clone(&s3);
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match &event.kind {
                WatchEventKind::Created | WatchEventKind::Renamed { .. } => {
                    ingestor::ingest_image(&pool_consumer, owner_id, &event, &s3_consumer).await;
                }
                WatchEventKind::Modified => {
                    tracing::debug!(path = %event.path.display(), "✏️  image modified (skipping re-ingest)");
                }
                WatchEventKind::Deleted => {
                    tracing::info!(path = %event.path.display(), "🗑️  image deleted");
                }
            }
        }
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/images", get(handlers::list_images))
        .route("/api/images/:id", get(handlers::get_image))
        .route("/api/images/:id/file", get(handlers::stream_image))
        .with_state(models::AppState { pool, storage: s3 });

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("nesoli-gallery listening on {addr}, watching {}", watch_dir.display());
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<Value> {
    Json(json!({
        "status":  "ok",
        "service": "nesoli-gallery",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
