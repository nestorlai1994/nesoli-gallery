use crate::{exif::extract_exif, processor, storage::S3Storage, watcher::WatchEvent};
use sqlx::PgPool;
use std::{fs, path::Path};
use uuid::Uuid;

/// Ingest a newly discovered image file into the gallery_images table,
/// upload it to MinIO, generate thumbnails, and optionally clean up the local file.
///
/// Image lifecycle (strict order — data safety rule):
///   1. DB INSERT → 2. MinIO original → 3. DB UPDATE storage_path
///   → 4. generate thumb+preview → 5. MinIO thumb+preview → 6. DB UPDATE processed
///   → 7a. If all succeeded + inbox_cleanup → delete local file
///   → 7b. If any step failed → move to quarantine (dead letter queue)
pub async fn ingest_image(
    pool: &PgPool,
    owner_id: Uuid,
    event: &WatchEvent,
    storage: &S3Storage,
    inbox_cleanup: bool,
    quarantine_dir: &Path,
) {
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

    // Step 1: Insert into DB
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

    // Step 2: Upload original to MinIO
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

    if let Err(e) = sqlx::query("UPDATE gallery_images SET storage_path = $1 WHERE id = $2")
        .bind(&s3_key)
        .bind(id)
        .execute(pool)
        .await
    {
        tracing::error!(id = %id, error = %e, "❌ failed to update storage_path");
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

    // Step 3: Generate thumbnail + preview (non-blocking on failure)
    let all_uploads_ok = match processor::generate_variants(path, id).await {
        Ok(images) => {
            let thumb_key = format!("{}/{}/thumb.webp", owner_id, id);
            let preview_key = format!("{}/{}/preview.webp", owner_id, id);

            let thumb_ok = storage
                .upload_file(&thumb_key, &images.thumb_path, "image/webp")
                .await;
            let preview_ok = storage
                .upload_file(&preview_key, &images.preview_path, "image/webp")
                .await;

            let variants_ok = if let (Ok(()), Ok(())) = (&thumb_ok, &preview_ok) {
                let update = sqlx::query(
                    "UPDATE gallery_images SET thumbnail_path = $1, preview_path = $2, processed = true WHERE id = $3",
                )
                .bind(&thumb_key)
                .bind(&preview_key)
                .bind(id)
                .execute(pool)
                .await;

                if let Err(e) = update {
                    tracing::error!(id = %id, error = %e, "❌ failed to update thumbnail_path");
                    false
                } else {
                    tracing::info!(id = %id, thumb = %thumb_key, preview = %preview_key, "✅ thumbnails uploaded");
                    true
                }
            } else {
                if let Err(e) = thumb_ok {
                    tracing::error!(id = %id, error = %e, "❌ failed to upload thumbnail");
                }
                if let Err(e) = preview_ok {
                    tracing::error!(id = %id, error = %e, "❌ failed to upload preview");
                }
                false
            };

            processor::cleanup_work_dir(id).await;
            variants_ok
        }
        Err(e) => {
            tracing::warn!(id = %id, error = %e, "⚠️ thumbnail generation failed (image still accessible as original)");
            false
        }
    };

    // Step 4: File disposition — cleanup on success, quarantine on failure
    if all_uploads_ok && inbox_cleanup {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                tracing::info!(id = %id, path = %path.display(), "🧹 inbox cleanup: local file deleted");
            }
            Err(e) => {
                tracing::warn!(id = %id, path = %path.display(), error = %e, "⚠️ inbox cleanup: failed to delete local file");
            }
        }
    } else if !all_uploads_ok {
        // Move to quarantine — don't leave poison pills in the inbox
        let quarantine_path = quarantine_dir.join(&filename);
        match tokio::fs::rename(path, &quarantine_path).await {
            Ok(()) => {
                tracing::warn!(
                    id = %id,
                    from = %path.display(),
                    to = %quarantine_path.display(),
                    "☠️ quarantined: file moved to dead letter queue (ingest partially failed)"
                );
            }
            Err(e) => {
                tracing::error!(
                    id = %id,
                    path = %path.display(),
                    error = %e,
                    "❌ quarantine failed: file stuck in inbox"
                );
            }
        }
    }
}
