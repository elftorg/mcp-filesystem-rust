//! Single-file tools: read (text/media/range), write, edit, create/delete,
//! move/copy, metadata, hashing, permissions, and symlink creation.

use serde_json::{Value, json};

#[cfg(unix)]
use cap_std::fs::PermissionsExt as CapPermissionsExt;
use sha2::{Digest, Sha256, Sha512};
use std::io::{BufRead, Read, Seek};

use crate::actions::args::{
    get_edits_arg, get_i64_arg, get_opt_bool, get_opt_i64, get_opt_str, get_str_arg,
};
use crate::actions::util::blocking;
use crate::config::Config;
use crate::errors::{MCSError, Result};
use crate::structures::RingBuffer;
use memmap2::Mmap;

/// Files below this size are read with a plain `read` syscall; at or above it
/// memory-mapping wins. Avoids mmap/munmap + page-fault overhead on tiny files.
const MMAP_THRESHOLD: u64 = 256 * 1024;

pub async fn read_text_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let head = get_opt_i64(args, "head").map(|v| v.clamp(0, 100_000));
    let tail = get_opt_i64(args, "tail").map(|v| v.clamp(0, 100_000));

    if head.is_some() && tail.is_some() {
        return Err(MCSError::InvalidParams(
            "Cannot specify both head and tail simultaneously".into(),
        ));
    }

    // Resolve once; every subsequent metadata/read reuses this handle.
    let resolved = config.sandbox().resolve(&path)?;
    let valid_path = resolved.canonical.clone();
    if !valid_path.exists() {
        return Err(MCSError::PathNotFound(format!(
            "Path does not exist: {path}"
        )));
    }
    let cap_meta = resolved.metadata().await?;

    if !cap_meta.is_file() {
        return Err(MCSError::InvalidParams(format!(
            "Path is not a file: {path}"
        )));
    }

    let file_size = cap_meta.len();
    if file_size > config.max_file_size {
        return Err(MCSError::FilesystemError(format!(
            "File size {size} exceeds maximum allowed size {max}",
            size = file_size,
            max = config.max_file_size
        )));
    }

    if let Some(h) = head {
        let h = h as usize;
        let path_clone = valid_path.clone();
        let (result_lines, count) = blocking(
            "read_text_file",
            move || -> std::result::Result<(Vec<String>, usize), String> {
                let file = std::fs::File::open(&path_clone)
                    .map_err(|e| format!("Cannot open file: {e}"))?;
                let reader = std::io::BufReader::new(file);
                let mut result_lines = Vec::with_capacity(h);
                let mut count = 0usize;
                for line in reader.lines() {
                    if count >= h {
                        break;
                    }
                    count += 1;
                    result_lines.push(line.map_err(|e| format!("Cannot read file: {e}"))?);
                }
                Ok((result_lines, count))
            },
        )
        .await?;
        return Ok(json!({
            "content": result_lines.join("\n"),
            "size": file_size,
            "totalLines": count,
            "path": valid_path.to_string_lossy(),
        }));
    }

    if let Some(t) = tail {
        let t = t as usize;
        let path_clone = valid_path.clone();
        let (total_lines, lines) = blocking(
            "read_text_file",
            move || -> std::result::Result<(usize, Vec<String>), String> {
                let file = std::fs::File::open(&path_clone)
                    .map_err(|e| format!("Cannot open file: {e}"))?;
                let reader = std::io::BufReader::new(file);
                let mut ring = RingBuffer::new(t);
                let mut total_lines = 0usize;
                for line in reader.lines() {
                    total_lines += 1;
                    ring.push(line.map_err(|e| format!("Cannot read file: {e}"))?);
                }
                Ok((total_lines, ring.into_vec()))
            },
        )
        .await?;
        return Ok(json!({
            "content": lines.join("\n"),
            "size": file_size,
            "totalLines": total_lines,
            "path": valid_path.to_string_lossy(),
        }));
    }

    let path_clone = valid_path.clone();
    let content = blocking(
        "read_text_file",
        move || -> std::result::Result<String, String> {
            if file_size < MMAP_THRESHOLD {
                let bytes =
                    std::fs::read(&path_clone).map_err(|e| format!("Cannot read file: {e}"))?;
                Ok(match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                })
            } else {
                let file = std::fs::File::open(&path_clone)
                    .map_err(|e| format!("Cannot open file: {e}"))?;
                let mmap =
                    unsafe { Mmap::map(&file).map_err(|e| format!("Cannot mmap file: {e}"))? };
                Ok(String::from_utf8_lossy(&mmap).into_owned())
            }
        },
    )
    .await?;

    let total_lines = content.lines().count();

    Ok(json!({
        "content": content,
        "size": file_size,
        "totalLines": total_lines,
        "path": valid_path.to_string_lossy(),
    }))
}

pub async fn read_media_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let resolved = config.sandbox().resolve(&path)?;
    let valid_path = resolved.canonical.clone();
    let cap_meta = resolved.metadata().await?;

    if !cap_meta.is_file() {
        return Err(MCSError::InvalidParams(format!(
            "Path is not a file: {path}"
        )));
    }

    let file_size = cap_meta.len();
    if file_size > config.max_file_size {
        return Err(MCSError::FilesystemError(format!(
            "File size {size} exceeds maximum allowed size {max}",
            size = file_size,
            max = config.max_file_size
        )));
    }

    let path_clone = valid_path.clone();
    let data = blocking(
        "read_media_file",
        move || -> std::result::Result<Vec<u8>, String> {
            if file_size < MMAP_THRESHOLD {
                std::fs::read(&path_clone).map_err(|e| format!("Cannot read file: {e}"))
            } else {
                let file = std::fs::File::open(&path_clone)
                    .map_err(|e| format!("Cannot open file: {e}"))?;
                let mmap =
                    unsafe { Mmap::map(&file).map_err(|e| format!("Cannot mmap file: {e}"))? };
                Ok(mmap.to_vec())
            }
        },
    )
    .await?;

    let mime_type = infer::get(&data)
        .map(|t| t.mime_type())
        .unwrap_or("application/octet-stream");

    let encoded = base64_simd::STANDARD.encode_to_string(&data);

    // Return spec-compliant typed content: ImageContent/AudioContent for
    // recognised media, otherwise a text note with the base64 in structuredContent.
    let kind = if mime_type.starts_with("image/") {
        "image"
    } else if mime_type.starts_with("audio/") {
        "audio"
    } else {
        ""
    };

    if kind.is_empty() {
        let content_type = content_inspector::inspect(&data);
        let detected_mime = if content_type == content_inspector::ContentType::BINARY {
            "application/octet-stream"
        } else {
            "text/plain"
        };
        Ok(json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "Binary file ({mime_type}, {file_size} bytes); base64 data in structuredContent.data"
                )
            }],
            "structuredContent": {
                "data": encoded,
                "mimeType": mime_type,
                "detectedType": detected_mime,
                "size": file_size,
                "path": valid_path.to_string_lossy(),
            },
            "isError": false
        }))
    } else {
        Ok(json!({
            "content": [{
                "type": kind,
                "data": encoded,
                "mimeType": mime_type,
            }],
            "isError": false
        }))
    }
}

pub async fn write_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let content = get_str_arg(args, "content")?;
    let resolved = config.sandbox().resolve(&path)?;

    resolved.write(content.as_bytes()).await?;

    Ok(json!({ "success": true, "path": resolved.canonical.to_string_lossy() }))
}

pub async fn edit_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let edits = get_edits_arg(args)?;
    let dry_run = get_opt_bool(args, "dryRun").unwrap_or(false);

    // Resolve once; the size check, read, and write below reuse this handle.
    let resolved = config.sandbox().resolve(&path)?;

    let cap_meta = resolved.metadata().await?;
    if cap_meta.len() > config.max_file_size {
        return Err(MCSError::FilesystemError(format!(
            "File size {size} exceeds maximum allowed size {max}",
            size = cap_meta.len(),
            max = config.max_file_size
        )));
    }

    let content = resolved.read_to_string().await?;

    let indent = detect_indent(&content);
    let mut result = content;
    let mut diffs = Vec::new();

    for edit in &edits {
        let old_text = edit
            .get("oldText")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                MCSError::InvalidParams("Each edit must have 'oldText' string".into())
            })?;
        let new_text = edit.get("newText").and_then(|v| v.as_str()).unwrap_or("");

        let normalized_old = normalize_whitespace(old_text, indent);
        let normalized_new = normalize_whitespace(new_text, indent);

        if let Some(pos) = result.find(&normalized_old) {
            let end = pos + normalized_old.len();
            let context_start = floor_char_boundary(&result, pos.saturating_sub(40));
            let context_end = ceil_char_boundary(&result, (end + 40).min(result.len()));

            diffs.push(json!({
                "oldText": old_text,
                "newText": new_text,
                "position": pos,
                "context": format!("...{}...", &result[context_start..context_end].replace('\n', "\\n")),
            }));

            result.replace_range(pos..end, &normalized_new);
        } else {
            diffs.push(json!({
                "oldText": old_text,
                "newText": new_text,
                "error": "Pattern not found in file",
            }));
        }
    }

    if !dry_run {
        resolved.write(result.as_bytes()).await?;
    }

    Ok(json!({
        "success": !dry_run,
        "dryRun": dry_run,
        "edits": diffs,
        "path": resolved.canonical.to_string_lossy(),
    }))
}

pub async fn create_directory(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let resolved = config.sandbox().resolve(&path)?;

    resolved.create_dir_all().await?;

    Ok(json!({ "success": true, "path": resolved.canonical.to_string_lossy() }))
}

pub async fn move_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let source = get_str_arg(args, "source")?;
    let destination = get_str_arg(args, "destination")?;

    let valid_source = config.sandbox().resolve_path(&source)?;
    let valid_dest = config.sandbox().resolve_destination_path(&destination)?;

    config.sandbox().rename(&source, &destination).await?;

    Ok(json!({
        "success": true,
        "source": valid_source.to_string_lossy(),
        "destination": valid_dest.to_string_lossy(),
    }))
}

pub async fn copy_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let source = get_str_arg(args, "source")?;
    let destination = get_str_arg(args, "destination")?;

    let valid_source = config.sandbox().resolve_path(&source)?;
    let valid_dest = config.sandbox().resolve_destination_path(&destination)?;

    config.sandbox().copy(&source, &destination).await?;

    Ok(json!({
        "success": true,
        "source": valid_source.to_string_lossy(),
        "destination": valid_dest.to_string_lossy(),
    }))
}

pub async fn delete_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let resolved = config.sandbox().resolve(&path)?;

    // `metadata` surfaces a missing path as `PathNotFound`; a present non-file
    // path is rejected as invalid params, preserving the original distinction.
    if !resolved.metadata().await?.is_file() {
        return Err(MCSError::InvalidParams(format!(
            "Path is not a file: {path}"
        )));
    }

    resolved.remove_file().await?;

    Ok(json!({ "success": true, "path": resolved.canonical.to_string_lossy() }))
}

pub async fn delete_directory(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let recursive = get_opt_bool(args, "recursive").unwrap_or(false);
    let resolved = config.sandbox().resolve(&path)?;

    if recursive {
        resolved.remove_dir_all().await?;
    } else {
        resolved.remove_dir().await?;
    }

    Ok(
        json!({ "success": true, "path": resolved.canonical.to_string_lossy(), "recursive": recursive }),
    )
}

pub async fn get_file_info(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let resolved = config.sandbox().resolve(&path)?;
    let valid_path = resolved.canonical.clone();
    let cap_meta = resolved.metadata().await?;

    let file_type = if cap_meta.is_dir() {
        "directory"
    } else if cap_meta.file_type().is_symlink() {
        "symlink"
    } else {
        "file"
    };

    let created = cap_meta.created().ok().and_then(|t| {
        t.into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs_f64())
    });
    let modified = cap_meta.modified().ok().and_then(|t| {
        t.into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs_f64())
    });
    let accessed = cap_meta.accessed().ok().and_then(|t| {
        t.into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs_f64())
    });

    #[cfg(unix)]
    let permissions = Some(format!("{:o}", cap_meta.permissions().mode() & 0o777));
    #[cfg(not(unix))]
    let permissions: Option<String> = None;

    Ok(json!({
        "path": valid_path.to_string_lossy(),
        "type": file_type,
        "size": cap_meta.len(),
        "permissions": permissions,
        "created": created,
        "modified": modified,
        "accessed": accessed,
        "readonly": cap_meta.permissions().readonly(),
    }))
}

pub async fn list_allowed_directories(_args: Option<&Value>, config: &Config) -> Result<Value> {
    Ok(json!({ "directories": config.allowed_directories }))
}

pub async fn hash_file(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let algorithm = get_opt_str(args, "algorithm").unwrap_or_else(|| "sha256".to_string());
    let resolved = config.sandbox().resolve(&path)?;

    // A missing path stats as "not a file", matching the pre-handle behaviour.
    let is_file = resolved
        .metadata()
        .await
        .map(|m| m.is_file())
        .unwrap_or(false);
    if !is_file {
        return Err(MCSError::InvalidParams(format!(
            "Path is not a file: {path}"
        )));
    }
    let valid_path = resolved.canonical.clone();

    let max_size = config.max_file_size;
    let path_clone = valid_path.clone();
    let alg = algorithm.clone();
    let (hash, _file_size) = blocking(
        "Hash",
        move || -> std::result::Result<(String, u64), String> {
            let meta =
                std::fs::metadata(&path_clone).map_err(|e| format!("Cannot get metadata: {e}"))?;
            let size = meta.len();
            if size > max_size {
                return Err(format!(
                    "File size {size} exceeds maximum allowed size {max_size}"
                ));
            }
            let result = if size < MMAP_THRESHOLD {
                let data = std::fs::read(&path_clone)
                    .map_err(|e| format!("Cannot read file for hashing: {e}"))?;
                hash_bytes(&alg, &data)
            } else {
                let file = std::fs::File::open(&path_clone)
                    .map_err(|e| format!("Cannot open file for hashing: {e}"))?;
                let mmap =
                    unsafe { Mmap::map(&file).map_err(|e| format!("Cannot mmap file: {e}"))? };
                hash_bytes(&alg, &mmap)
            }?;
            Ok((result, size))
        },
    )
    .await?;

    Ok(json!({
        "path": valid_path.to_string_lossy(),
        "algorithm": algorithm,
        "hash": hash,
    }))
}

pub async fn set_permissions(args: Option<&Value>, config: &Config) -> Result<Value> {
    #[cfg(unix)]
    {
        let path = get_str_arg(args, "path")?;
        let mode_str = get_str_arg(args, "mode")?;
        let resolved = config.sandbox().resolve(&path)?;
        let mode = u32::from_str_radix(&mode_str, 8).map_err(|_| {
            MCSError::InvalidParams(format!(
                "Invalid mode: {mode_str}. Use octal format (e.g. 644, 755)"
            ))
        })?;

        use cap_std::fs::PermissionsExt;
        let permissions = cap_std::fs::Permissions::from_mode(mode);
        resolved.set_permissions(permissions).await?;

        Ok(json!({
            "success": true,
            "path": resolved.canonical.to_string_lossy(),
            "mode": mode_str,
        }))
    }

    #[cfg(not(unix))]
    {
        let _ = (args, config);
        Err(MCSError::FilesystemError(
            "Permission changes are not supported on this platform".into(),
        ))
    }
}

pub async fn create_symlink(args: Option<&Value>, config: &Config) -> Result<Value> {
    let source = get_str_arg(args, "source")?;
    let link_path = get_str_arg(args, "linkPath")?;

    let valid_source = config.sandbox().resolve_path(&source)?;
    let valid_link = config.sandbox().resolve_destination_path(&link_path)?;

    config.sandbox().create_symlink(&source, &link_path).await?;

    Ok(json!({
        "success": true,
        "source": valid_source.to_string_lossy(),
        "linkPath": valid_link.to_string_lossy(),
    }))
}

pub async fn read_file_range(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let offset = get_i64_arg(args, "offset")?;
    let length = get_i64_arg(args, "length")?;

    if offset < 0 || length < 0 {
        return Err(MCSError::InvalidParams(
            "offset and length must be non-negative".into(),
        ));
    }

    let valid_path = config.sandbox().resolve_path(&path)?;

    let max_size = config.max_file_size;
    let path_clone = valid_path.clone();
    let content = blocking(
        "read_file_range",
        move || -> std::result::Result<(String, i64), String> {
            let meta = std::fs::metadata(&path_clone)
                .map_err(|e| format!("Cannot get file metadata: {e}"))?;
            let file_size = meta.len() as i64;
            if offset >= file_size {
                return Err(format!("Offset {offset} exceeds file size {file_size}"));
            }
            let actual = (offset as u64)
                .saturating_add(length as u64)
                .min(file_size as u64)
                .saturating_sub(offset as u64);
            if actual > max_size {
                return Err(format!(
                    "Requested range {actual} exceeds maximum allowed size {max_size}"
                ));
            }
            let mut file =
                std::fs::File::open(&path_clone).map_err(|e| format!("Cannot open file: {e}"))?;
            file.seek(std::io::SeekFrom::Start(offset as u64))
                .map_err(|e| format!("Cannot seek: {e}"))?;
            let mut buf = Vec::with_capacity(actual as usize);
            file.take(actual)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Cannot read range: {e}"))?;
            Ok((String::from_utf8_lossy(&buf).into_owned(), actual as i64))
        },
    )
    .await?;

    Ok(json!({
        "content": content.0,
        "offset": offset,
        "length": content.1,
        "path": valid_path.to_string_lossy(),
    }))
}

// ── Helpers ──────────────────────────────────────────────

fn hash_bytes(alg: &str, data: &[u8]) -> std::result::Result<String, String> {
    match alg {
        "sha256" => {
            let mut hasher = Sha256::new();
            hasher.update(data);
            Ok(hex::encode(hasher.finalize()))
        }
        "sha512" => {
            let mut hasher = Sha512::new();
            hasher.update(data);
            Ok(hex::encode(hasher.finalize()))
        }
        "md5" => {
            let mut hasher = md5::Md5::new();
            hasher.update(data);
            Ok(hex::encode(hasher.finalize()))
        }
        "blake3" => Ok(blake3::hash(data).to_hex().to_string()),
        _ => Err(format!("Unsupported hash algorithm: {alg}")),
    }
}

/// Largest char-boundary index `<= i`.
const fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char-boundary index `>= i`.
const fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    let n = s.len();
    while i < n && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn detect_indent(content: &str) -> &'static str {
    let mut spaces = 0;
    let mut tabs = 0;
    for line in content.lines() {
        if line.starts_with('\t') {
            tabs += 1;
        } else if line.starts_with("  ") {
            spaces += 1;
        }
    }
    if tabs > spaces { "\t" } else { "    " }
}

fn normalize_whitespace(text: &str, indent: &str) -> String {
    if indent == "\t" {
        text.replace("    ", "\t")
    } else {
        text.replace('\t', "    ")
    }
}
