use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Full gallery_images row — returned by GET /api/images/:id
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ImageRecord {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub filename: String,
    pub original_path: String,
    pub storage_path: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub metadata: serde_json::Value,
    pub processed: bool,
    pub thumbnail_path: Option<String>,
    pub photographer_id: Option<Uuid>,
    pub shoot_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lightweight summary — returned in list endpoint
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ImageSummary {
    pub id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Paginated list response
#[derive(Debug, Serialize)]
pub struct ImageListResponse {
    pub images: Vec<ImageSummary>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

/// Query parameters for pagination
#[derive(Debug, serde::Deserialize)]
pub struct PaginationParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}
