use aws_sdk_s3::{
    config::{BehaviorVersion, Credentials, Region},
    primitives::ByteStream,
    Client,
};
use std::path::Path;

/// Result of a successful GetObject call — ready for streaming.
pub struct S3Object {
    pub body: ByteStream,
    pub content_type: String,
    pub content_length: i64,
}

pub struct S3Storage {
    client: Client,
    bucket: String,
}

impl S3Storage {
    pub async fn init(endpoint: &str, user: &str, pass: &str, bucket: &str) -> Self {
        let creds = Credentials::new(user, pass, None, None, "env");
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .region(Region::new("us-east-1"))
            .credentials_provider(creds)
            .force_path_style(true)
            .build();
        let client = Client::from_conf(config);

        let storage = Self {
            client,
            bucket: bucket.to_string(),
        };
        storage.ensure_bucket().await;
        storage
    }

    async fn ensure_bucket(&self) {
        match self.client.create_bucket().bucket(&self.bucket).send().await {
            Ok(_) => tracing::info!("created MinIO bucket '{}'", self.bucket),
            Err(e) => {
                let msg = format!("{}", e);
                if msg.contains("BucketAlreadyOwnedByYou")
                    || msg.contains("BucketAlreadyExists")
                {
                    tracing::debug!("bucket '{}' already exists", self.bucket);
                } else {
                    tracing::warn!("bucket creation response: {}", msg);
                }
            }
        }
    }

    pub async fn upload_file(
        &self,
        key: &str,
        path: &Path,
        content_type: &str,
    ) -> Result<(), String> {
        let body = ByteStream::from_path(path)
            .await
            .map_err(|e| format!("failed to read file: {}", e))?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("failed to upload to MinIO: {}", e))?;

        tracing::info!(key = %key, bucket = %self.bucket, "uploaded to MinIO");
        Ok(())
    }

    /// Stream an object from MinIO by key.
    pub async fn get_object_stream(&self, key: &str) -> Result<S3Object, String> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| format!("failed to get object from MinIO: {}", e))?;

        let content_type = resp
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let content_length = resp.content_length().unwrap_or(0);

        Ok(S3Object {
            body: resp.body,
            content_type,
            content_length,
        })
    }

    /// Delete an object from MinIO by key.
    pub async fn delete_object(&self, key: &str) -> Result<(), String> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| format!("failed to delete from MinIO: {e}"))?;

        tracing::info!(key = %key, bucket = %self.bucket, "deleted from MinIO");
        Ok(())
    }
}
