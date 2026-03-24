use std::path::{Path, PathBuf};
use tokio::process::Command;
use uuid::Uuid;

/// Output paths from thumbnail/preview generation.
pub struct ProcessedImages {
    pub thumb_path: PathBuf,
    pub preview_path: PathBuf,
}

const THUMB_WIDTH: u32 = 400;
const PREVIEW_WIDTH: u32 = 1600;
const THUMB_QUALITY: u32 = 80;
const PREVIEW_QUALITY: u32 = 85;

/// Generate WebP thumbnail (400px) and preview (1600px) from an original image.
/// Uses `vipsthumbnail` CLI subprocess — avoids C-binding complexity with musl/Alpine.
pub async fn generate_variants(
    input_path: &Path,
    image_id: Uuid,
) -> Result<ProcessedImages, String> {
    let work_dir = PathBuf::from(format!("/tmp/nesoli-processed/{}", image_id));
    tokio::fs::create_dir_all(&work_dir)
        .await
        .map_err(|e| format!("failed to create work dir: {e}"))?;

    let thumb_path = work_dir.join("thumb.webp");
    let preview_path = work_dir.join("preview.webp");

    run_vipsthumbnail(input_path, &thumb_path, THUMB_WIDTH, THUMB_QUALITY).await?;
    run_vipsthumbnail(input_path, &preview_path, PREVIEW_WIDTH, PREVIEW_QUALITY).await?;

    Ok(ProcessedImages {
        thumb_path,
        preview_path,
    })
}

/// Shell out to `vipsthumbnail` for a single resize + WebP conversion.
async fn run_vipsthumbnail(
    input: &Path,
    output: &Path,
    width: u32,
    quality: u32,
) -> Result<(), String> {
    // vipsthumbnail input.jpg --size 400 -o output.webp[Q=80]
    let output_spec = format!("{}[Q={}]", output.display(), quality);

    let result = Command::new("vipsthumbnail")
        .arg(input)
        .arg("--size")
        .arg(format!("{}x", width))
        .arg("-o")
        .arg(&output_spec)
        .output()
        .await
        .map_err(|e| format!("failed to spawn vipsthumbnail: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "vipsthumbnail failed (exit {}): {}",
            result.status,
            stderr.trim()
        ));
    }

    tracing::debug!(
        input = %input.display(),
        output = %output.display(),
        width,
        "generated variant"
    );
    Ok(())
}

/// Clean up the temporary processing directory for an image.
pub async fn cleanup_work_dir(image_id: Uuid) {
    let work_dir = PathBuf::from(format!("/tmp/nesoli-processed/{}", image_id));
    if let Err(e) = tokio::fs::remove_dir_all(&work_dir).await {
        tracing::warn!(id = %image_id, error = %e, "failed to clean up work dir");
    }
}
