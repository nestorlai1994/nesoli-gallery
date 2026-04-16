use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::models::{AppState, ImageListResponse, ImageRecord, ImageSummary, PaginationParams, WatermarkParams};
use crate::processor;
use crate::storage::S3Storage;

pub async fn list_images(
    Query(params): Query<PaginationParams>,
    State(pool): State<PgPool>,
) -> Result<Json<ImageListResponse>, StatusCode> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100).max(1);
    let offset = ((page - 1) * per_page) as i64;
    let limit = per_page as i64;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gallery_images")
        .fetch_one(&pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to count images");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let images: Vec<ImageSummary> = sqlx::query_as(
        "SELECT id, filename, mime_type, size_bytes, metadata, thumbnail_path, preview_path, created_at \
         FROM gallery_images ORDER BY created_at DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "failed to list images");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ImageListResponse {
        images,
        total,
        page,
        per_page,
    }))
}

pub async fn get_image(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
) -> Result<Json<ImageRecord>, StatusCode> {
    let record: ImageRecord = sqlx::query_as(
        "SELECT id, owner_id, filename, original_path, storage_path, mime_type, \
         size_bytes, metadata, processed, thumbnail_path, preview_path, photographer_id, \
         shoot_id, created_at, updated_at \
         FROM gallery_images WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, id = %id, "failed to fetch image");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(record))
}

pub async fn stream_image(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    State(storage): State<Arc<S3Storage>>,
) -> Result<Response, StatusCode> {
    stream_from_minio(id, "storage_path", &pool, &storage).await
}

pub async fn stream_thumbnail(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    State(storage): State<Arc<S3Storage>>,
) -> Result<Response, StatusCode> {
    stream_from_minio(id, "thumbnail_path", &pool, &storage).await
}

pub async fn stream_preview(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    State(storage): State<Arc<S3Storage>>,
) -> Result<Response, StatusCode> {
    stream_from_minio(id, "preview_path", &pool, &storage).await
}

/// Generic helper: stream any path column from MinIO.
async fn stream_from_minio(
    id: Uuid,
    column: &str,
    pool: &PgPool,
    storage: &S3Storage,
) -> Result<Response, StatusCode> {
    let query = format!("SELECT {} FROM gallery_images WHERE id = $1", column);
    let row: Option<(Option<String>,)> = sqlx::query_as(&query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, id = %id, column, "failed to fetch path");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let key = row
        .ok_or(StatusCode::NOT_FOUND)?
        .0
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            tracing::warn!(id = %id, column, "path is empty or null — not yet processed");
            StatusCode::NOT_FOUND
        })?;

    let obj = storage.get_object_stream(&key).await.map_err(|e| {
        tracing::error!(error = %e, id = %id, key = %key, "failed to stream from MinIO");
        StatusCode::NOT_FOUND
    })?;

    let stream = ReaderStream::new(obj.body.into_async_read());
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, obj.content_type)
        .header(header::CONTENT_LENGTH, obj.content_length)
        .body(body)
        .unwrap())
}

/// GET /api/images/:id/watermark?text=NesOli
/// Generate a watermarked preview image on-demand (not persisted).
/// Uses `tempfile::NamedTempFile` — OS auto-deletes when stream completes.
pub async fn watermark_image(
    Path(id): Path<Uuid>,
    Query(params): Query<WatermarkParams>,
    State(pool): State<PgPool>,
    State(storage): State<Arc<S3Storage>>,
) -> Result<Response, StatusCode> {
    let text = params.text.unwrap_or_else(|| "NesOli".to_string());

    // Fetch preview_path from DB
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT preview_path FROM gallery_images WHERE id = $1")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, id = %id, "failed to fetch preview_path");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let preview_key = row
        .ok_or(StatusCode::NOT_FOUND)?
        .0
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            tracing::warn!(id = %id, "preview not yet generated — cannot watermark");
            StatusCode::NOT_FOUND
        })?;

    // Download preview to a temp file for vips processing
    let mut preview_tmp = tempfile::Builder::new()
        .suffix(".webp")
        .tempfile()
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create temp preview file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let obj = storage.get_object_stream(&preview_key).await.map_err(|e| {
        tracing::error!(error = %e, id = %id, "failed to download preview from MinIO");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let bytes = obj
        .body
        .collect()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to collect preview bytes");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .into_bytes();

    {
        use std::io::Write;
        preview_tmp.write_all(&bytes).map_err(|e| {
            tracing::error!(error = %e, "failed to write temp preview");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    // Generate watermarked image — returns NamedTempFile (OS auto-deletes on drop)
    let output_tmp = processor::generate_watermark(preview_tmp.path(), &text)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, id = %id, "watermark generation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Convert NamedTempFile → tokio::fs::File for async streaming
    let std_file = output_tmp.reopen().map_err(|e| {
        tracing::error!(error = %e, "failed to reopen watermarked file");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let file_size = std_file.metadata().map(|m| m.len() as i64).unwrap_or(0);
    let file = tokio::fs::File::from_std(std_file);

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    // output_tmp is moved into response scope — when Axum finishes streaming,
    // it drops the Body → drops the stream → drops nothing extra.
    // But output_tmp is dropped HERE at end of this function, AFTER body is built.
    // The OS file handle from reopen() keeps the data readable even after unlink.
    // When the reopened File is dropped (stream done), OS reclaims disk space.
    drop(output_tmp);
    drop(preview_tmp);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "image/webp")
        .header(header::CONTENT_LENGTH, file_size)
        .body(body)
        .unwrap())
}

/// POST /api/images/upload
/// Accept multipart file upload, save to the watch directory, let the watcher ingest.
pub async fn upload_image(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let watch_dir = &state.watch_dir;
    let mut uploaded = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        tracing::error!(error = %e, "failed to read multipart field");
        StatusCode::BAD_REQUEST
    })? {
        let filename = match field.file_name() {
            Some(name) => sanitize_filename(name),
            None => continue,
        };

        if filename.is_empty() {
            continue;
        }

        let data = field.bytes().await.map_err(|e| {
            tracing::error!(error = %e, filename = %filename, "failed to read upload bytes");
            StatusCode::BAD_REQUEST
        })?;

        // Write to watch directory — the file watcher will auto-ingest
        let dest = watch_dir.join(&filename);
        let mut file = tokio::fs::File::create(&dest).await.map_err(|e| {
            tracing::error!(error = %e, path = %dest.display(), "failed to create upload file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        file.write_all(&data).await.map_err(|e| {
            tracing::error!(error = %e, path = %dest.display(), "failed to write upload data");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        file.flush().await.ok();

        tracing::info!(
            filename = %filename,
            size = data.len(),
            "📤 file uploaded to watch directory"
        );
        uploaded.push(filename);
    }

    if uploaded.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok(Json(serde_json::json!({
        "uploaded": uploaded,
        "message": "Files saved. Processing will begin automatically."
    })))
}

/// Sanitize a user-provided filename: keep only safe characters.
fn sanitize_filename(name: &str) -> String {
    let name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// DELETE /api/images/:id
/// Remove image from DB, MinIO (original + thumb + preview), and local inbox.
pub async fn delete_image(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    State(storage): State<Arc<S3Storage>>,
) -> Result<StatusCode, StatusCode> {
    // Fetch paths before deleting the DB row
    let row: Option<(String, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT storage_path, thumbnail_path, preview_path, original_path FROM gallery_images WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, id = %id, "failed to fetch image for delete");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (storage_path, thumb_path, preview_path, original_path) = row.ok_or(StatusCode::NOT_FOUND)?;

    // Delete from MinIO (best-effort — log errors but don't fail)
    for key in [Some(storage_path), thumb_path, preview_path].into_iter().flatten() {
        if !key.is_empty() {
            if let Err(e) = storage.delete_object(&key).await {
                tracing::warn!(id = %id, key = %key, error = %e, "failed to delete from MinIO");
            }
        }
    }

    // Delete local file if still in inbox
    let local = std::path::Path::new(&original_path);
    if local.exists() {
        if let Err(e) = tokio::fs::remove_file(local).await {
            tracing::warn!(id = %id, path = %original_path, error = %e, "failed to delete local file");
        }
    }

    // Delete from DB
    sqlx::query("DELETE FROM gallery_images WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, id = %id, "failed to delete image from DB");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(id = %id, "🗑️ image deleted");
    Ok(StatusCode::NO_CONTENT)
}
