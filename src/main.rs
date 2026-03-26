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
use std::{collections::HashMap, env, path::PathBuf, sync::Arc};
use tokio::{sync::mpsc, time};
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
    let inbox_cleanup  = env::var("INBOX_CLEANUP")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let debounce_secs: u64 = env::var("INGEST_DEBOUNCE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let quarantine_dir = PathBuf::from(
        env::var("QUARANTINE_DIR").unwrap_or_else(|_| "/photos/quarantine".to_string()),
    );

    // Ensure quarantine directory exists
    if let Err(e) = tokio::fs::create_dir_all(&quarantine_dir).await {
        tracing::warn!(error = %e, path = %quarantine_dir.display(), "failed to create quarantine dir");
    }

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

    // Event consumer with debounce — waits for files to stabilize before ingesting
    let pool_consumer = pool.clone();
    let s3_consumer = Arc::clone(&s3);
    tokio::spawn(async move {
        // Tracks pending files: path → (last_seen_size, last_change_time, original_event)
        let mut pending: HashMap<PathBuf, (u64, time::Instant, WatchEvent)> = HashMap::new();
        let mut tick = time::interval(time::Duration::from_secs(1));
        let debounce = time::Duration::from_secs(debounce_secs);

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match &event.kind {
                        WatchEventKind::Created | WatchEventKind::Renamed { .. } => {
                            let size = tokio::fs::metadata(&event.path)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0);
                            tracing::debug!(
                                path = %event.path.display(),
                                size,
                                "📥 file detected, starting debounce"
                            );
                            pending.insert(event.path.clone(), (size, time::Instant::now(), event));
                        }
                        WatchEventKind::Modified => {
                            // If file is in debounce, update its size + timestamp
                            if let Some(entry) = pending.get_mut(&event.path) {
                                let new_size = tokio::fs::metadata(&event.path)
                                    .await
                                    .map(|m| m.len())
                                    .unwrap_or(0);
                                if new_size != entry.0 {
                                    entry.0 = new_size;
                                    entry.1 = time::Instant::now();
                                    tracing::debug!(
                                        path = %event.path.display(),
                                        size = new_size,
                                        "📝 file still copying, debounce reset"
                                    );
                                }
                            }
                        }
                        WatchEventKind::Deleted => {
                            pending.remove(&event.path);
                            tracing::info!(path = %event.path.display(), "🗑️  image deleted");
                        }
                    }
                }
                _ = tick.tick() => {
                    // Check each pending file for stability
                    let now = time::Instant::now();
                    let ready: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, (_, last_change, _))| now.duration_since(*last_change) >= debounce)
                        .map(|(path, _)| path.clone())
                        .collect();

                    for path in ready {
                        if let Some((size, _, event)) = pending.remove(&path) {
                            // Final size check — verify file still exists and size matches
                            let current_size = tokio::fs::metadata(&path)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0);
                            if current_size != size || current_size == 0 {
                                tracing::debug!(
                                    path = %path.display(),
                                    expected = size,
                                    actual = current_size,
                                    "⏳ size changed during final check, re-debouncing"
                                );
                                pending.insert(path, (current_size, now, event));
                                continue;
                            }

                            tracing::info!(
                                path = %path.display(),
                                size,
                                "✅ file stable, starting ingest"
                            );
                            ingestor::ingest_image(
                                &pool_consumer,
                                owner_id,
                                &event,
                                &s3_consumer,
                                inbox_cleanup,
                                &quarantine_dir,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/images", get(handlers::list_images))
        .route("/api/images/:id", get(handlers::get_image))
        .route("/api/images/:id/file", get(handlers::stream_image))
        .route("/api/images/:id/thumbnail", get(handlers::stream_thumbnail))
        .route("/api/images/:id/preview", get(handlers::stream_preview))
        .route("/api/images/:id/watermark", get(handlers::watermark_image))
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
