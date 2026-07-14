//! Cross-cutting helpers for action handlers: a `spawn_blocking` wrapper that
//! collapses the repeated join-error / domain-error mapping, and output-path
//! derivation shared by the compression and crypto tools.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::errors::{MCSError, Result};

/// Run a blocking closure on the tokio blocking pool, mapping both a join
/// failure and the closure's `String` error into `FilesystemError`.
///
/// Replaces the `spawn_blocking(...).await.map_err(...)?.map_err(...)` triple
/// that every filesystem action would otherwise repeat. `label` names the
/// operation in the join-failure message (e.g. `"read_text_file"`).
pub(crate) async fn blocking<T, F>(label: &'static str, f: F) -> Result<T>
where
    F: FnOnce() -> std::result::Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| MCSError::FilesystemError(format!("{label} task failed: {e}")))?
        .map_err(MCSError::FilesystemError)
}

/// How to derive a default output filename from a source filename when the
/// caller did not pass an explicit `output` path.
#[derive(Clone, Copy)]
pub(crate) enum Suffix<'a> {
    /// Append `ext` to the source filename (e.g. `data` → `data.gz`).
    Add(&'a str),
    /// Strip `ext` from the source filename (e.g. `data.gz` → `data`).
    Strip(&'a str),
}

/// Resolve the output path for a transform tool (compress/decompress/encrypt/
/// decrypt). An explicit `output` is validated through the sandbox; otherwise a
/// sibling of `source` is derived by adding or stripping `suffix`. The derived
/// sibling stays in the (already-validated) source directory, so it needs no
/// further sandbox check.
pub(crate) fn derive_output(
    source: &Path,
    explicit: Option<&str>,
    suffix: Suffix<'_>,
    config: &Config,
) -> Result<PathBuf> {
    if let Some(out) = explicit {
        return config.sandbox().resolve_destination_path(out);
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let new_name = match suffix {
        Suffix::Add(ext) => format!("{name}{ext}"),
        Suffix::Strip(ext) => name.strip_suffix(ext).unwrap_or(&name).to_string(),
    };
    let mut result = source.to_path_buf();
    result.set_file_name(new_name);
    Ok(result)
}

/// Whether a directory entry's name begins with `.` (dotfile/dotdir). Used to
/// prune hidden entries from directory walks (search, grep, tar collection).
pub(crate) fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}
