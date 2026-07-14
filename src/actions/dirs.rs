//! Directory-oriented tools: listing, recursive tree, glob search, regex grep,
//! and disk-usage aggregation. All heavy traversal runs on the blocking pool.

use serde_json::{Value, json};
use std::io::BufRead;
use walkdir::WalkDir;

use crate::actions::args::{get_opt_str, get_opt_str_array, get_str_arg};
use crate::actions::util::{blocking, is_hidden};
use crate::config::Config;
use crate::errors::{MCSError, Result};

pub async fn list_directory(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let resolved = config.sandbox().resolve(&path)?;

    let entries_raw = resolved.read_dir().await?;
    let mut entries: Vec<String> = entries_raw
        .into_iter()
        .map(|(name, is_dir)| {
            let prefix = if is_dir { "[DIR]" } else { "[FILE]" };
            format!("{prefix} {name}")
        })
        .collect();
    entries.sort_unstable();

    Ok(json!({ "entries": entries, "path": resolved.canonical.to_string_lossy() }))
}

pub async fn list_directory_with_sizes(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let sort_by = get_opt_str(args, "sortBy").unwrap_or_else(|| "name".to_string());
    let valid_path = config.sandbox().resolve_path(&path)?;

    let path_clone = valid_path.clone();
    let (mut entries, total_files, total_dirs, combined_size) = blocking(
        "Directory listing",
        move || -> std::result::Result<(Vec<Value>, u64, u64, u64), String> {
            let mut entries = Vec::new();
            let mut total_files = 0u64;
            let mut total_dirs = 0u64;
            let mut combined_size = 0u64;

            let read_dir = std::fs::read_dir(&path_clone)
                .map_err(|e| format!("Cannot read directory: {e}"))?;

            for entry in read_dir {
                let entry = entry.map_err(|e| format!("Error reading directory entry: {e}"))?;
                let name = entry.file_name().to_string_lossy().to_string();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };

                if file_type.is_dir() {
                    total_dirs += 1;
                    entries.push(
                        json!({ "name": name, "type": "dir", "display": format!("[DIR] {name}") }),
                    );
                } else {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    total_files += 1;
                    combined_size += size;
                    entries.push(json!({
                        "name": name,
                        "type": "file",
                        "size": size,
                        "display": format!("[FILE] {name} ({size} B)"),
                    }));
                }
            }

            Ok((entries, total_files, total_dirs, combined_size))
        },
    )
    .await?;

    match sort_by.as_str() {
        "size" => entries.sort_by(|a, b| {
            let a_size = a.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            let b_size = b.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            b_size.cmp(&a_size)
        }),
        _ => entries.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        }),
    }

    Ok(json!({
        "entries": entries,
        "summary": {
            "totalFiles": total_files,
            "totalDirectories": total_dirs,
            "combinedSize": combined_size,
        },
        "path": valid_path.to_string_lossy(),
    }))
}

pub async fn search_files(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let pattern = get_str_arg(args, "pattern")?;
    let exclude_patterns: Vec<String> =
        get_opt_str_array(args, "excludePatterns").unwrap_or_default();
    let valid_path = config.sandbox().resolve_path(&path)?;

    let glob = globset::Glob::new(&pattern)
        .map_err(|e| MCSError::InvalidParams(format!("Invalid glob pattern: {e}")))?
        .compile_matcher();

    let exclude_globs: Vec<globset::GlobMatcher> = exclude_patterns
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();

    let root = valid_path.clone();
    let follow = config.server.follow_symlinks;

    let results = blocking(
        "Search",
        move || -> std::result::Result<Vec<String>, String> {
            let mut res = Vec::new();
            const SEARCH_LIMIT: usize = 100_000;
            let walker = WalkDir::new(&root)
                .follow_links(follow)
                .into_iter()
                .filter_entry(|e| !is_hidden(e));
            for entry in walker.filter_map(|e| e.ok()) {
                if res.len() >= SEARCH_LIMIT {
                    break;
                }
                let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let relative_str = relative.to_string_lossy();
                if exclude_globs
                    .iter()
                    .any(|g| g.is_match(relative_str.as_ref()))
                {
                    continue;
                }
                if glob.is_match(relative_str.as_ref()) {
                    res.push(entry.path().to_string_lossy().to_string());
                }
            }
            Ok(res)
        },
    )
    .await?;

    Ok(json!({
        "results": results,
        "count": results.len(),
        "pattern": pattern,
        "path": valid_path.to_string_lossy(),
    }))
}

pub async fn directory_tree(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let exclude_patterns: Vec<String> =
        get_opt_str_array(args, "excludePatterns").unwrap_or_default();
    let valid_path = config.sandbox().resolve_path(&path)?;

    let root = valid_path.clone();
    let exclude_globs: Vec<globset::GlobMatcher> = exclude_patterns
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();

    let tree = tokio::task::spawn_blocking(move || {
        let mut nodes = 0usize;
        build_tree(&root, &root, &exclude_globs, 0, &mut nodes)
    })
    .await
    .map_err(|e| MCSError::FilesystemError(format!("Directory tree task failed: {e}")))?;

    Ok(json!(tree))
}

pub async fn grep_files(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let pattern = get_str_arg(args, "pattern")?;
    let valid_path = config.sandbox().resolve_path(&path)?;

    let re = regex::Regex::new(&pattern)
        .map_err(|e| MCSError::InvalidParams(format!("Invalid regex pattern: {e}")))?;

    let exclude_patterns: Vec<String> =
        get_opt_str_array(args, "excludePatterns").unwrap_or_default();
    let exclude_globs: Vec<globset::GlobMatcher> = exclude_patterns
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();

    let root = valid_path.clone();
    let follow = config.server.follow_symlinks;
    let max_bytes = config.max_file_size;

    let results = blocking(
        "Grep",
        move || -> std::result::Result<Vec<Value>, String> {
            let mut res = Vec::new();
            const GREP_LIMIT: usize = 100_000;

            let walker = WalkDir::new(&root)
                .follow_links(follow)
                .into_iter()
                .filter_entry(|e| !is_hidden(e));

            for entry in walker.filter_map(|e| e.ok()) {
                if res.len() >= GREP_LIMIT {
                    break;
                }
                if !entry.file_type().is_file() {
                    continue;
                }

                let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let relative_str = relative.to_string_lossy();

                if exclude_globs
                    .iter()
                    .any(|g| g.is_match(relative_str.as_ref()))
                {
                    continue;
                }

                let ext = entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if is_binary_extension(ext) {
                    continue;
                }

                if entry.metadata().map(|m| m.len()).unwrap_or(0) > max_bytes {
                    continue;
                }

                let file = match std::fs::File::open(entry.path()) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let reader = std::io::BufReader::new(file);

                for (idx, line) in reader.lines().enumerate() {
                    let Ok(line) = line else { break };
                    if res.len() >= GREP_LIMIT {
                        break;
                    }
                    if re.is_match(&line) {
                        res.push(json!({
                            "file": entry.path().to_string_lossy(),
                            "line": idx + 1,
                            "content": line,
                        }));
                    }
                }
            }
            Ok(res)
        },
    )
    .await?;

    Ok(json!({ "results": results, "count": results.len(), "pattern": pattern }))
}

pub async fn get_disk_usage(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let valid_path = config.sandbox().resolve_path(&path)?;

    let root = valid_path.clone();
    let follow = config.server.follow_symlinks;

    let usage = blocking(
        "Disk usage",
        move || -> std::result::Result<(u64, u64, u64), String> {
            let mut total_size = 0u64;
            let mut file_count = 0u64;
            let mut dir_count = 0u64;

            let walker = WalkDir::new(&root).follow_links(follow).into_iter();

            for entry in walker.filter_map(|e| e.ok()) {
                if entry.file_type().is_dir() {
                    dir_count += 1;
                } else if entry.file_type().is_file()
                    && let Ok(meta) = entry.metadata()
                {
                    total_size += meta.len();
                    file_count += 1;
                }
            }

            Ok((total_size, file_count, dir_count))
        },
    )
    .await?;

    Ok(json!({
        "path": valid_path.to_string_lossy(),
        "totalSize": usage.0,
        "fileCount": usage.1,
        "directoryCount": usage.2,
    }))
}

// ── Helpers ──────────────────────────────────────────────

/// Exact match against a static set of common binary file extensions.
fn is_binary_extension(ext: &str) -> bool {
    const BINARY_EXTS: &[&str] = &[
        "bin", "exe", "dll", "so", "dylib", "o", "class", "pyc", "jpg", "jpeg", "png", "gif",
        "bmp", "ico", "mp3", "mp4", "avi", "mov", "zip", "tar", "gz", "bz2", "xz", "zst", "7z",
        "rar", "pdf", "wasm",
    ];
    BINARY_EXTS.contains(&ext)
}

/// Maximum directory recursion depth and total node budget for `directory_tree`,
/// guarding against stack overflow and unbounded output on pathological trees.
const MAX_TREE_DEPTH: usize = 64;
const MAX_TREE_NODES: usize = 100_000;

fn build_tree(
    root: &std::path::Path,
    current: &std::path::Path,
    exclude_globs: &[globset::GlobMatcher],
    depth: usize,
    nodes: &mut usize,
) -> Value {
    let relative = current.strip_prefix(root).unwrap_or(current);
    let relative_str = relative.to_string_lossy();

    if exclude_globs
        .iter()
        .any(|g| g.is_match(relative_str.as_ref()))
    {
        return Value::Null;
    }

    *nodes += 1;
    if *nodes > MAX_TREE_NODES {
        return Value::Null;
    }

    let metadata = match std::fs::metadata(current) {
        Ok(m) => m,
        Err(_) => return Value::Null,
    };

    let name = current
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if metadata.is_dir() {
        if depth >= MAX_TREE_DEPTH {
            return json!({
                "name": name,
                "type": "directory",
                "children": [],
                "truncated": true,
            });
        }

        let mut children = Vec::new();
        let read_dir = match std::fs::read_dir(current) {
            Ok(d) => d,
            Err(_) => return Value::Null,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();

            if let Some(name_str) = path.file_name().and_then(|n| n.to_str())
                && name_str.starts_with('.')
                && name_str != "."
            {
                continue;
            }

            let child = build_tree(root, &path, exclude_globs, depth + 1, nodes);
            if !child.is_null() {
                children.push(child);
            }
        }

        json!({
            "name": name,
            "type": "directory",
            "children": children,
        })
    } else if metadata.is_file() {
        json!({ "name": name, "type": "file" })
    } else {
        Value::Null
    }
}
