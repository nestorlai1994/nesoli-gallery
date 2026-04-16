use crate::{ingestor, storage::S3Storage, watcher::{WatchEvent, WatchEventKind}};
use sqlx::PgPool;
use std::{path::{Path, PathBuf}, time::Duration};
use tokio::time;
use uuid::Uuid;

/// Background sweeper that periodically scans the inbox for stuck files.
///
/// A stuck file is one that has been sitting in the inbox longer than `max_age`
/// without being ingested (likely due to a transient error like MinIO restart,
/// DB connection pool exhaustion, or network blip).
///
/// For each stuck file: attempt re-ingest once. If it fails again, the ingestor
/// will quarantine it automatically.
pub async fn run_sweeper(
    pool: PgPool,
    storage: std::sync::Arc<S3Storage>,
    owner_id: Uuid,
    watch_dir: PathBuf,
    quarantine_dir: PathBuf,
    inbox_cleanup: bool,
    interval: Duration,
    max_age: Duration,
) {
    tracing::info!(
        interval_secs = interval.as_secs(),
        max_age_secs = max_age.as_secs(),
        dir = %watch_dir.display(),
        "🧹 background sweeper started"
    );

    let mut tick = time::interval(interval);
    // Skip the first immediate tick — let the system warm up
    tick.tick().await;

    loop {
        tick.tick().await;
        sweep_once(&pool, &storage, owner_id, &watch_dir, &quarantine_dir, inbox_cleanup, max_age).await;
    }
}

async fn sweep_once(
    pool: &PgPool,
    storage: &S3Storage,
    owner_id: Uuid,
    watch_dir: &Path,
    quarantine_dir: &Path,
    inbox_cleanup: bool,
    max_age: Duration,
) {
    let mut entries = match tokio::fs::read_dir(watch_dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "sweeper: failed to read inbox dir");
            return;
        }
    };

    let mut stuck_count = 0u32;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();

        // Skip directories (including quarantine subdir)
        if path.is_dir() {
            continue;
        }

        // Skip non-image files
        if !is_image(&path) {
            continue;
        }

        // Check file age
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };

        let created = metadata
            .created()
            .or_else(|_| metadata.modified())
            .unwrap_or(std::time::SystemTime::now());

        let age = created.elapsed().unwrap_or_default();
        if age < max_age {
            continue;
        }

        stuck_count += 1;
        tracing::info!(
            path = %path.display(),
            age_hours = age.as_secs() / 3600,
            "🔄 sweeper: found stuck file, attempting re-ingest"
        );

        let event = WatchEvent {
            path: path.clone(),
            kind: WatchEventKind::Created,
            detected_at: std::time::SystemTime::now(),
        };

        ingestor::ingest_image(pool, owner_id, &event, storage, inbox_cleanup, quarantine_dir).await;
    }

    if stuck_count > 0 {
        tracing::info!(count = stuck_count, "🧹 sweeper: processed stuck files");
    } else {
        tracing::debug!("🧹 sweeper: inbox clean, no stuck files found");
    }
}

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "raw", "cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2", "pef",
];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}
