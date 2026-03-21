use crate::{exif::extract_exif, storage::S3Storage, watcher::WatchEvent};
use sqlx::PgPool;
use std::fs;
use uuid::Uuid;

/// Ingest a newly discovered image file into the gallery_images table,
/// upload it to MinIO, and update storage_path to the S3 key.
pub async fn ingest_image(pool: &PgPool, owner_id: Uuid, event: &WatchEvent, storage: &S3Storage) {
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

    // Insert with a temporary storage_path (will update after upload)
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
    .bind(&original_path)
    .bind(&mime_type)
    .bind(size_bytes)
    .bind(&metadata)
    .fetch_one(pool)
    .await;

    let id = match result {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(
                path  = %path.display(),
                error = %e,
                "❌ failed to ingest image"
            );
            return;
        }
    };

    // Upload to MinIO: key = {owner_id}/{image_id}/{filename}
    let s3_key = format!("{}/{}/{}", owner_id, id, filename);

    if let Err(e) = storage.upload_file(&s3_key, path, &mime_type).await {
        tracing::error!(
            id    = %id,
            path  = %path.display(),
            error = %e,
            "❌ failed to upload to MinIO"
        );
        return;
    }

    // Update storage_path to the S3 key
    if let Err(e) = sqlx::query("UPDATE gallery_images SET storage_path = $1 WHERE id = $2")
        .bind(&s3_key)
        .bind(id)
        .execute(pool)
        .await
    {
        tracing::error!(
            id    = %id,
            error = %e,
            "❌ failed to update storage_path"
        );
        return;
    }

    tracing::info!(
        id   = %id,
        path = %path.display(),
        key  = %s3_key,
        mime = %mime_type,
        size = size_bytes,
        "✅ ingested image and uploaded to MinIO"
    );
}
