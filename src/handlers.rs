use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::models::{ImageListResponse, ImageRecord, ImageSummary, PaginationParams};
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
        "SELECT id, filename, mime_type, size_bytes, metadata, created_at \
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
         size_bytes, metadata, processed, thumbnail_path, photographer_id, \
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
    let row: (String,) = sqlx::query_as(
        "SELECT storage_path FROM gallery_images WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, id = %id, "failed to fetch image storage_path");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let storage_path = row.0;
    if storage_path.is_empty() {
        tracing::warn!(id = %id, "storage_path is empty — image not yet uploaded to MinIO");
        return Err(StatusCode::NOT_FOUND);
    }

    let obj = storage.get_object_stream(&storage_path).await.map_err(|e| {
        tracing::error!(error = %e, id = %id, key = %storage_path, "failed to stream from MinIO");
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
