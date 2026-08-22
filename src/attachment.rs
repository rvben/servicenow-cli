use std::path::{Path, PathBuf};

use crate::api::ApiError;

pub const LIST_FIELDS: &[&str] = &[
    "file_name",
    "content_type",
    "size_bytes",
    "sys_created_by",
    "sys_created_on",
    "sys_id",
];

pub fn upload_file_name(path: &Path, override_name: Option<&str>) -> Result<String, ApiError> {
    let name = override_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            ApiError::InvalidInput(
                "could not derive an attachment name; provide --name explicitly".into(),
            )
        })?;
    validate_file_name(&name)?;
    Ok(name)
}

pub fn safe_download_name(name: &str) -> Result<String, ApiError> {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .unwrap_or_default();
    validate_file_name(name)?;
    Ok(name.to_string())
}

pub fn destination_path(destination: Option<&Path>, file_name: &str) -> Result<PathBuf, ApiError> {
    let safe_name = safe_download_name(file_name)?;
    match destination {
        None => Ok(PathBuf::from(safe_name)),
        Some(path) if path == Path::new("-") => Ok(path.to_path_buf()),
        Some(path) if path.is_dir() => Ok(path.join(safe_name)),
        Some(path) => Ok(path.to_path_buf()),
    }
}

pub fn content_type(path: &Path, explicit: Option<&str>) -> Result<String, ApiError> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| ApiError::InvalidInput(format!("invalid HTTP content type '{value}'")))?;
        return Ok(value.to_string());
    }
    Ok(mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string())
}

pub fn human_size(value: &str) -> String {
    let Ok(bytes) = value.parse::<u64>() else {
        return value.to_string();
    };
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0usize;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn validate_file_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        Err(ApiError::InvalidInput(format!(
            "invalid attachment file name '{name}'"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_cannot_escape_the_destination() {
        assert_eq!(
            safe_download_name("../../report.txt").unwrap(),
            "report.txt"
        );
        assert_eq!(safe_download_name(r"..\report.txt").unwrap(), "report.txt");
        assert!(safe_download_name("..").is_err());
        assert!(safe_download_name("\0").is_err());
    }

    #[test]
    fn destination_uses_server_name_for_directories() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            destination_path(Some(directory.path()), "report.txt").unwrap(),
            directory.path().join("report.txt")
        );
        assert_eq!(
            destination_path(Some(Path::new("renamed.txt")), "report.txt").unwrap(),
            PathBuf::from("renamed.txt")
        );
    }

    #[test]
    fn content_types_and_sizes_are_friendly() {
        assert_eq!(
            content_type(Path::new("report.pdf"), None).unwrap(),
            "application/pdf"
        );
        assert_eq!(human_size("1536"), "1.5 KiB");
        assert_eq!(human_size("unknown"), "unknown");
    }
}
