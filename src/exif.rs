use exif::{In, Reader, Tag};
use serde_json::{json, Value};
use std::{fs::File, io::BufReader, path::Path};

/// Extract EXIF metadata from `path` and return a JSONB-compatible Value.
/// Returns an empty object on any read/parse error (non-fatal).
pub fn extract_exif(path: &Path) -> Value {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return json!({}),
    };

    let mut reader = BufReader::new(file);
    let exif = match Reader::new().read_from_container(&mut reader) {
        Ok(e) => e,
        Err(_) => return json!({}),
    };

    let get = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY)
            .map(|f: &exif::Field| f.display_value().with_unit(&exif).to_string())
    };

    let mut meta = serde_json::Map::new();

    if let Some(v) = get(Tag::Make)         { meta.insert("camera_make".into(),   Value::String(v)); }
    if let Some(v) = get(Tag::Model)        { meta.insert("camera_model".into(),  Value::String(v)); }
    if let Some(v) = get(Tag::LensModel)    { meta.insert("lens".into(),          Value::String(v)); }
    if let Some(v) = get(Tag::FocalLength)  { meta.insert("focal_length".into(),  Value::String(v)); }
    if let Some(v) = get(Tag::FNumber)      { meta.insert("aperture".into(),      Value::String(v)); }
    if let Some(v) = get(Tag::PhotographicSensitivity) {
        meta.insert("iso".into(), Value::String(v));
    }
    if let Some(v) = get(Tag::ExposureTime) { meta.insert("shutter_speed".into(), Value::String(v)); }
    if let Some(v) = get(Tag::DateTimeOriginal) {
        meta.insert("taken_at".into(), Value::String(v));
    }
    if let Some(lat) = get(Tag::GPSLatitude) {
        meta.insert("gps_lat".into(), Value::String(lat));
    }
    if let Some(lon) = get(Tag::GPSLongitude) {
        meta.insert("gps_lon".into(), Value::String(lon));
    }

    Value::Object(meta)
}
