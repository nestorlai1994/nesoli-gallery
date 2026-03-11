mod watcher;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use std::{env, path::PathBuf};
use tokio::sync::mpsc;
use watcher::{WatchEvent, WatchEventKind};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nesoli_gallery=debug,tower_http=info".into()),
        )
        .init();

    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let watch_dir = PathBuf::from(env::var("WATCH_DIR").unwrap_or_else(|_| "/photos".to_string()));

    // File system watcher pipeline
    let (tx, mut rx) = mpsc::channel::<WatchEvent>(100);

    let watch_dir_clone = watch_dir.clone();
    tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            watcher::start_watcher(watch_dir_clone, tx);
        })
        .await
        .ok();
    });

    // Event consumer — logs now, DB insert in Day 9
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match &event.kind {
                WatchEventKind::Created => {
                    tracing::info!(path = %event.path.display(), "📷 image created");
                }
                WatchEventKind::Modified => {
                    tracing::info!(path = %event.path.display(), "✏️  image modified");
                }
                WatchEventKind::Deleted => {
                    tracing::info!(path = %event.path.display(), "🗑️  image deleted");
                }
                WatchEventKind::Renamed { from } => {
                    tracing::info!(
                        from = %from.display(),
                        to   = %event.path.display(),
                        "🔀 image renamed"
                    );
                }
            }
        }
    });

    let addr = format!("0.0.0.0:{port}");
    let app = Router::new().route("/health", get(health));

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
