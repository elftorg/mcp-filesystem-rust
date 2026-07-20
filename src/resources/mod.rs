//! MCP resources exposed by the filesystem server and the ChatGPT Apps UI.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

use crate::config::Config;
use crate::errors::{MCSError, Result};

pub const UI_RESOURCE_URI: &str = "ui://filesystem/browser-v1.html";
pub const UI_RESOURCE_MIME: &str = "text/html;profile=mcp-app";

const PAGE_SIZE: usize = 250;
const MAX_LISTED_FILES: usize = 10_000;
const PREVIEW_BYTES: usize = 8 * 1024;

pub async fn list(params: Option<&Value>, config: &Config) -> Result<Value> {
    let cursor = params
        .and_then(|p| p.get("cursor"))
        .and_then(Value::as_str)
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| MCSError::InvalidParams("resources/list cursor must be an integer".into()))?;

    let allowed = config.allowed_directories.clone();
    let follow = config.server.follow_symlinks;
    let mut resources = tokio::task::spawn_blocking(move || scan_resources(allowed, follow))
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Resource scan task failed: {e}")))??;

    resources.sort_by(|a, b| {
        a.get("uri")
            .and_then(Value::as_str)
            .cmp(&b.get("uri").and_then(Value::as_str))
    });

    resources.insert(0, ui_resource_descriptor());

    if cursor > resources.len() {
        return Err(MCSError::InvalidParams(
            "resources/list cursor is outside the result set".into(),
        ));
    }

    let end = (cursor + PAGE_SIZE).min(resources.len());
    let page = resources[cursor..end].to_vec();
    let mut result = json!({ "resources": page });
    if end < resources.len() {
        result["nextCursor"] = Value::String(end.to_string());
    }
    Ok(result)
}

pub async fn read(params: Option<&Value>, config: &Config) -> Result<Value> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| MCSError::InvalidParams("Missing resources/read 'uri' parameter".into()))?;

    if uri == UI_RESOURCE_URI {
        return Ok(json!({
            "contents": [{
                "uri": UI_RESOURCE_URI,
                "mimeType": UI_RESOURCE_MIME,
                "text": browser_html(),
                "_meta": {
                    "ui": {
                        "prefersBorder": true,
                        "csp": {
                            "connectDomains": [],
                            "resourceDomains": []
                        }
                    }
                }
            }]
        }));
    }

    let path = file_uri_to_path(uri)?;
    let requested = path.to_string_lossy().to_string();
    let resolved = config.sandbox().resolve(&requested)?;
    let metadata = resolved.metadata().await?;

    if !metadata.is_file() {
        return Err(MCSError::InvalidParams(format!(
            "Resource URI does not identify a regular file: {uri}"
        )));
    }
    if metadata.len() > config.max_file_size {
        return Err(MCSError::FilesystemError(format!(
            "Resource size {} exceeds maximum allowed size {}",
            metadata.len(),
            config.max_file_size
        )));
    }

    let bytes = resolved.read().await?;
    let mime = detect_mime(&resolved.canonical, &bytes);
    let modified = std::fs::metadata(&resolved.canonical)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(format_modified);

    let meta = json!({
        "size": metadata.len(),
        "modifiedTime": modified,
        "preview": preview(&bytes, &mime)
    });

    let content = if is_text(&mime, &bytes) {
        json!({
            "uri": uri,
            "mimeType": mime,
            "text": String::from_utf8_lossy(&bytes),
            "_meta": meta
        })
    } else {
        json!({
            "uri": uri,
            "mimeType": mime,
            "blob": base64_simd::STANDARD.encode_to_string(&bytes),
            "_meta": meta
        })
    };

    Ok(json!({ "contents": [content] }))
}

pub fn templates() -> Value {
    json!({
        "resourceTemplates": [{
            "uriTemplate": "file:///{path}",
            "name": "filesystem-file",
            "title": "Sandboxed filesystem file",
            "description": "A file inside one of the server's allowed directories.",
            "mimeType": "application/octet-stream"
        }]
    })
}

fn scan_resources(allowed: Vec<String>, follow: bool) -> Result<Vec<Value>> {
    let mut roots = Vec::with_capacity(allowed.len());
    for root in allowed {
        let path = absolute_path(&root);
        let canonical = path.canonicalize().map_err(|e| {
            MCSError::FilesystemError(format!(
                "Cannot canonicalize allowed directory {}: {e}",
                path.display()
            ))
        })?;
        roots.push(canonical);
    }

    let mut output = Vec::new();
    for root in &roots {
        let walker = WalkDir::new(root)
            .follow_links(follow)
            .into_iter()
            .filter_entry(|entry| !is_hidden(entry.path(), root));

        for entry in walker.filter_map(std::result::Result::ok) {
            if output.len() >= MAX_LISTED_FILES {
                return Ok(output);
            }
            if !entry.file_type().is_file() {
                continue;
            }

            let path = if follow {
                match entry.path().canonicalize() {
                    Ok(path) if roots.iter().any(|allowed| path.starts_with(allowed)) => path,
                    _ => continue,
                }
            } else {
                entry.path().to_path_buf()
            };

            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let sample = std::fs::read(&path)
                .map(|bytes| bytes.into_iter().take(PREVIEW_BYTES).collect::<Vec<_>>())
                .unwrap_or_default();
            let mime = detect_mime(&path, &sample);
            let modified = metadata.modified().ok().map(format_modified);
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string());

            output.push(json!({
                "uri": path_to_file_uri(&path),
                "name": name.clone(),
                "title": name,
                "description": format!("Sandboxed file {}", path.display()),
                "mimeType": mime,
                "size": metadata.len(),
                "annotations": {
                    "audience": ["user", "assistant"],
                    "priority": 0.5,
                    "lastModified": modified
                },
                "_meta": {
                    "preview": preview(&sample, &mime)
                }
            }));
        }
    }
    Ok(output)
}

fn ui_resource_descriptor() -> Value {
    json!({
        "uri": UI_RESOURCE_URI,
        "name": "filesystem-browser",
        "title": "Filesystem Browser",
        "description": "Interactive file browser and preview component for ChatGPT.",
        "mimeType": UI_RESOURCE_MIME,
        "annotations": {
            "audience": ["user"],
            "priority": 1.0
        },
        "_meta": {
            "ui": {
                "prefersBorder": true,
                "csp": {
                    "connectDomains": [],
                    "resourceDomains": []
                }
            }
        }
    })
}

fn browser_html() -> String {
    include_str!("../../app/ui/index.html")
        .replace("/*__APP_STYLE__*/", include_str!("../../app/ui/style.css"))
        .replace("/*__APP_SCRIPT__*/", include_str!("../../app/ui/app.js"))
}

fn absolute_path(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn path_to_file_uri(path: &Path) -> String {
    let mut rendered = path.to_string_lossy().replace('\\', "/");
    if !rendered.starts_with('/') {
        rendered.insert(0, '/');
    }
    format!("file://{}", encode_uri_path(&rendered))
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf> {
    let encoded = uri.strip_prefix("file://").ok_or_else(|| {
        MCSError::InvalidParams("Only file:// and ui:// resources are supported".into())
    })?;
    let mut path = decode_uri_path(encoded)?;

    #[cfg(windows)]
    {
        if path.starts_with('/') && path.as_bytes().get(2).is_some_and(|value| *value == b':') {
            path.remove(0);
        }
        path = path.replace('/', "\\");
    }

    Ok(PathBuf::from(path))
}

fn is_hidden(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).ok().is_some_and(|relative| {
        relative.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with('.') && name != ".")
        })
    })
}

fn detect_mime(path: &Path, bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }

    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "rs" => "text/x-rust",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ if content_inspector::inspect(bytes) != content_inspector::ContentType::BINARY => {
            "text/plain"
        }
        _ => "application/octet-stream",
    }
    .to_string()
}

fn is_text(mime: &str, bytes: &[u8]) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/toml"
                | "application/yaml"
                | "image/svg+xml"
        )
        || content_inspector::inspect(bytes) != content_inspector::ContentType::BINARY
}

fn preview(bytes: &[u8], mime: &str) -> Value {
    if !is_text(mime, bytes) {
        return Value::Null;
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(PREVIEW_BYTES)]);
    Value::String(text.chars().take(2_000).collect())
}

fn encode_uri_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
        {
            output.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}

fn decode_uri_path(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(MCSError::InvalidParams(
                    "Incomplete percent escape in resource URI".into(),
                ));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).map_err(|_| {
                MCSError::InvalidParams("Invalid percent escape in resource URI".into())
            })?;
            let byte = u8::from_str_radix(hex, 16).map_err(|_| {
                MCSError::InvalidParams("Invalid percent escape in resource URI".into())
            })?;
            output.push(byte);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output)
        .map_err(|_| MCSError::InvalidParams("Resource URI is not valid UTF-8".into()))
}

fn format_modified(value: SystemTime) -> String {
    let seconds = value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096)
            / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_round_trip() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\Data Folder\readme.md")
        } else {
            PathBuf::from("/tmp/data folder/readme.md")
        };
        let uri = path_to_file_uri(&path);
        assert!(uri.starts_with("file:///"));
        assert_eq!(file_uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn detects_common_text_types() {
        assert_eq!(
            detect_mime(Path::new("README.md"), b"# Hello"),
            "text/markdown"
        );
        assert_eq!(
            detect_mime(Path::new("data.json"), br#"{"ok":true}"#),
            "application/json"
        );
    }

    #[test]
    fn exposes_ui_template() {
        let html = browser_html();
        assert!(html.contains("Filesystem Browser"));
        assert!(!html.contains("/*__APP_STYLE__*/"));
        assert!(!html.contains("/*__APP_SCRIPT__*/"));
    }
}
