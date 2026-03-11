use crate::{exif::extract_exif, watcher::WatchEvent};
use sqlx::PgPool;
use std::fs;
use uuid::Uuid;

/// Ingest a newly discovered image file into the gallery_images table.
/// `owner_id` is the user who owns this image (system user until auth exists).
pub async fn ingest_image(pool: &PgPool, owner_id: Uuid, event: &WatchEvent) {
    let path = &event.path;

    let filename = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => {
            tracing::warn!(path = %path.display(), "skipping: cannot determine filename");
            return;
        }
    };

    let original_path = path.to_string_lossy().to_string();

    let size_bytes = fs::metadata(path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    let mime_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();

    let metadata = extract_exif(path);

    let result: Result<Uuid, _> = sqlx::query_scalar(
        r#"
        INSERT INTO gallery_images
            (owner_id, filename, original_path, storage_path, mime_type, size_bytes, metadata)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
    )
    .bind(owner_id)
    .bind(&filename)
    .bind(&original_path)
    .bind(&original_path)   // storage_path = local path for now; Day 11 updates to MinIO key
    .bind(&mime_type)
    .bind(size_bytes)
    .bind(&metadata)
    .fetch_one(pool)
    .await;

    match result {
        Ok(id) => tracing::info!(
            id   = %id,
            path = %path.display(),
            mime = %mime_type,
            size = size_bytes,
            "✅ ingested image"
        ),
        Err(e) => tracing::error!(
            path  = %path.display(),
            error = %e,
            "❌ failed to ingest image"
        ),
    }
}
