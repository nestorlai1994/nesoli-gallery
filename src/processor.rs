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

/// Camera RAW extensions that need dcraw preprocessing before vipsthumbnail.
const RAW_EXTENSIONS: &[&str] = &[
    "raw", "cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2", "pef",
];

/// Check if a file is a camera RAW format.
fn is_camera_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Convert a camera RAW file to TIFF using dcraw_emu (libraw).
/// Returns the path to the generated TIFF in the work directory.
async fn dcraw_to_tiff(input_path: &Path, work_dir: &Path) -> Result<PathBuf, String> {
    // dcraw_emu -T -w -H 0 -o 1 -q 3 <input>
    //   -T  = output TIFF
    //   -w  = use camera white balance
    //   -H 0 = clip highlights
    //   -o 1 = sRGB color space
    //   -q 3 = AHD interpolation (best quality)
    let result = Command::new("dcraw_emu")
        .arg("-T")
        .arg("-w")
        .arg("-H")
        .arg("0")
        .arg("-o")
        .arg("1")
        .arg("-q")
        .arg("3")
        .arg(input_path)
        .output()
        .await
        .map_err(|e| format!("failed to spawn dcraw_emu: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("dcraw_emu failed (exit {}): {}", result.status, stderr.trim()));
    }

    // dcraw_emu appends .tiff to the full filename (e.g. photo.NEF → photo.NEF.tiff)
    let dcraw_output = PathBuf::from(format!("{}.tiff", input_path.display()));
    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("cannot determine file stem")?;

    // Copy (not rename — /photos and /tmp may be on different filesystems)
    let tiff_path = work_dir.join(format!("{stem}.tiff"));
    tokio::fs::copy(&dcraw_output, &tiff_path)
        .await
        .map_err(|e| format!("failed to copy dcraw output: {e}"))?;
    let _ = tokio::fs::remove_file(&dcraw_output).await;

    tracing::debug!(input = %input_path.display(), output = %tiff_path.display(), "dcraw converted RAW → TIFF");
    Ok(tiff_path)
}

/// Generate WebP thumbnail (400px) and preview (1600px) from an original image.
/// For camera RAW files, first converts to TIFF via dcraw.
pub async fn generate_variants(
    input_path: &Path,
    image_id: Uuid,
) -> Result<ProcessedImages, String> {
    let work_dir = PathBuf::from(format!("/tmp/nesoli-processed/{}", image_id));
    tokio::fs::create_dir_all(&work_dir)
        .await
        .map_err(|e| format!("failed to create work dir: {e}"))?;

    // If camera RAW, preprocess with dcraw first
    let effective_input = if is_camera_raw(input_path) {
        tracing::info!(path = %input_path.display(), "camera RAW detected, converting via dcraw");
        dcraw_to_tiff(input_path, &work_dir).await?
    } else {
        input_path.to_path_buf()
    };

    let thumb_path = work_dir.join("thumb.webp");
    let preview_path = work_dir.join("preview.webp");

    run_vipsthumbnail(&effective_input, &thumb_path, THUMB_WIDTH, THUMB_QUALITY).await?;
    run_vipsthumbnail(&effective_input, &preview_path, PREVIEW_WIDTH, PREVIEW_QUALITY).await?;

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

/// Generate a watermarked image by compositing translucent text over a preview.
/// Returns a `NamedTempFile` — OS auto-deletes when the file handle is dropped.
/// This eliminates the race condition of `sleep(5s) → delete`.
pub async fn generate_watermark(
    preview_path: &Path,
    text: &str,
) -> Result<tempfile::NamedTempFile, String> {
    let text_file = tempfile::Builder::new()
        .suffix(".png")
        .tempfile()
        .map_err(|e| format!("failed to create text tempfile: {e}"))?;
    let text_path = text_file.path().to_path_buf();

    let output_file = tempfile::Builder::new()
        .suffix(".webp")
        .tempfile()
        .map_err(|e| format!("failed to create output tempfile: {e}"))?;
    let output_path = output_file.path().to_path_buf();

    // 1. Render text to RGBA PNG (white text with alpha)
    let text_result = Command::new("vips")
        .args(["text", &text_path.to_string_lossy(), text])
        .args(["--width", "800", "--dpi", "120", "--rgba"])
        .output()
        .await
        .map_err(|e| format!("failed to spawn vips text: {e}"))?;

    if !text_result.status.success() {
        let stderr = String::from_utf8_lossy(&text_result.stderr);
        return Err(format!("vips text failed: {}", stderr.trim()));
    }

    // 2. Composite text over preview (mode 2 = OVER)
    let composite_result = Command::new("vips")
        .args(["composite"])
        .arg(format!(
            "{} {}",
            preview_path.display(),
            text_path.display()
        ))
        .arg(&output_path)
        .arg("2")
        .args(["--x", "50", "--y", "50"])
        .output()
        .await
        .map_err(|e| format!("failed to spawn vips composite: {e}"))?;

    if !composite_result.status.success() {
        let stderr = String::from_utf8_lossy(&composite_result.stderr);
        return Err(format!("vips composite failed: {}", stderr.trim()));
    }

    // text_file is dropped here → OS deletes the text PNG automatically
    // output_file is returned → caller streams it, OS deletes on drop
    Ok(output_file)
}
