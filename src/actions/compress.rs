//! Compression and archival tools: gzip/zstd single-file (de)compression and
//! tar archive create/extract, with decompression-bomb guards.

use async_compression::tokio::bufread::GzipDecoder as AsyncGzipDecoder;
use async_compression::tokio::bufread::ZstdDecoder as AsyncZstdDecoder;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use std::io::{Read, Seek};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;
use walkdir::WalkDir;

use crate::actions::args::{get_opt_i64, get_opt_str, get_str_arg};
use crate::actions::util::{Suffix, blocking, derive_output, is_hidden};
use crate::config::Config;
use crate::errors::{MCSError, Result};

pub async fn compress_gzip(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let level = get_opt_i64(args, "level").unwrap_or(6);
    let level = level.clamp(0, 9) as u32;
    let output = get_opt_str(args, "output");

    let valid_path = config.sandbox().resolve_path(&path)?;
    let output_path = derive_output(&valid_path, output.as_deref(), Suffix::Add(".gz"), config)?;

    if output_path == valid_path {
        return Err(MCSError::InvalidParams(
            "Output path must differ from source".into(),
        ));
    }

    let src = valid_path.clone();
    let dst = output_path.clone();
    let (original_size, compressed_size) = blocking(
        "Compression",
        move || -> std::result::Result<(u64, u64), String> {
            let meta =
                std::fs::metadata(&src).map_err(|e| format!("Cannot get source metadata: {e}"))?;
            let original_size = meta.len();
            let mut input =
                std::fs::File::open(&src).map_err(|e| format!("Cannot open file: {e}"))?;
            let output = std::fs::File::create(&dst)
                .map_err(|e| format!("Cannot create output file: {e}"))?;
            let mut encoder = GzEncoder::new(output, Compression::new(level));
            std::io::copy(&mut input, &mut encoder)
                .map_err(|e| format!("gzip compression failed: {e}"))?;
            let output_file = encoder
                .finish()
                .map_err(|e| format!("gzip compression finalize failed: {e}"))?;
            let size = output_file
                .metadata()
                .map_err(|e| format!("Cannot get output metadata: {e}"))?
                .len();
            Ok((original_size, size))
        },
    )
    .await?;

    let ratio = compute_ratio(original_size, compressed_size);

    Ok(json!({
        "success": true,
        "source": valid_path.to_string_lossy(),
        "output": output_path.to_string_lossy(),
        "algorithm": "gzip",
        "level": level,
        "originalSize": original_size,
        "compressedSize": compressed_size,
        "ratio": ratio,
    }))
}

/// Async writer with a byte limit — decompression bomb protection.
struct AsyncLimitedWriter<W: AsyncWrite + Unpin> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W: AsyncWrite + Unpin> AsyncLimitedWriter<W> {
    const fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            written: 0,
            limit,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for AsyncLimitedWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let new_total = self.written.saturating_add(buf.len() as u64);
        if new_total > self.limit {
            return Poll::Ready(Err(std::io::Error::other(format!(
                "Decompressed output exceeds maximum allowed size of {} bytes",
                self.limit
            ))));
        }
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(count)) => {
                self.written += count as u64;
                Poll::Ready(Ok(count))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub async fn decompress_gzip(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let output = get_opt_str(args, "output");

    let valid_path = config.sandbox().resolve_path(&path)?;
    let output_path = derive_output(&valid_path, output.as_deref(), Suffix::Strip(".gz"), config)?;

    if output_path == valid_path {
        return Err(MCSError::InvalidParams(
            "Output path must differ from source".into(),
        ));
    }

    let src = valid_path.clone();
    let dst = output_path.clone();
    let max_out = config.max_decompressed_size;

    let reader = tokio::io::BufReader::new(
        tokio::fs::File::open(&src)
            .await
            .map_err(|e| MCSError::FilesystemError(format!("Cannot open compressed file: {e}")))?,
    );
    let mut decoder = AsyncGzipDecoder::new(reader);
    let output = tokio::fs::File::create(&dst)
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Cannot create output file: {e}")))?;
    let mut writer = AsyncLimitedWriter::new(tokio::io::BufWriter::new(output), max_out);
    tokio::io::copy(&mut decoder, &mut writer)
        .await
        .map_err(|e| MCSError::FilesystemError(format!("gzip decompression failed: {e}")))?;
    let decompressed_size = writer.written;

    Ok(json!({
        "success": true,
        "source": valid_path.to_string_lossy(),
        "output": output_path.to_string_lossy(),
        "algorithm": "gzip",
        "decompressedSize": decompressed_size,
    }))
}

pub async fn compress_zstd(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let level = get_opt_i64(args, "level").unwrap_or(3);
    let level = level.clamp(1, 22) as i32;
    let output = get_opt_str(args, "output");

    let valid_path = config.sandbox().resolve_path(&path)?;
    let output_path = derive_output(&valid_path, output.as_deref(), Suffix::Add(".zst"), config)?;

    if output_path == valid_path {
        return Err(MCSError::InvalidParams(
            "Output path must differ from source".into(),
        ));
    }

    let src = valid_path.clone();
    let dst = output_path.clone();
    let lvl = level;
    let (original_size, compressed_size) = blocking(
        "Compression",
        move || -> std::result::Result<(u64, u64), String> {
            let meta =
                std::fs::metadata(&src).map_err(|e| format!("Cannot get source metadata: {e}"))?;
            let original_size = meta.len();
            let mut input =
                std::fs::File::open(&src).map_err(|e| format!("Cannot open file: {e}"))?;
            let output = std::fs::File::create(&dst)
                .map_err(|e| format!("Cannot create output file: {e}"))?;
            let mut encoder = zstd::stream::Encoder::new(output, lvl)
                .map_err(|e| format!("Cannot create zstd encoder: {e}"))?;
            std::io::copy(&mut input, &mut encoder)
                .map_err(|e| format!("zstd compression failed: {e}"))?;
            let output_file = encoder
                .finish()
                .map_err(|e| format!("zstd compression finalize failed: {e}"))?;
            let size = output_file
                .metadata()
                .map_err(|e| format!("Cannot get output metadata: {e}"))?
                .len();
            Ok((original_size, size))
        },
    )
    .await?;

    let ratio = compute_ratio(original_size, compressed_size);

    Ok(json!({
        "success": true,
        "source": valid_path.to_string_lossy(),
        "output": output_path.to_string_lossy(),
        "algorithm": "zstd",
        "level": level,
        "originalSize": original_size,
        "compressedSize": compressed_size,
        "ratio": ratio,
    }))
}

#[allow(clippy::cast_precision_loss)]
fn compute_ratio(original: u64, compressed: u64) -> Option<f64> {
    if original > 0 {
        let cs = compressed as f64;
        let os = original as f64;
        Some((cs / os * 100.0 * 100.0).round() / 100.0)
    } else {
        None
    }
}

pub async fn decompress_zstd(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let output = get_opt_str(args, "output");

    let valid_path = config.sandbox().resolve_path(&path)?;
    let output_path = derive_output(
        &valid_path,
        output.as_deref(),
        Suffix::Strip(".zst"),
        config,
    )?;

    if output_path == valid_path {
        return Err(MCSError::InvalidParams(
            "Output path must differ from source".into(),
        ));
    }

    let src = valid_path.clone();
    let dst = output_path.clone();
    let max_out = config.max_decompressed_size;

    let reader = tokio::io::BufReader::new(
        tokio::fs::File::open(&src)
            .await
            .map_err(|e| MCSError::FilesystemError(format!("Cannot open compressed file: {e}")))?,
    );
    let mut decoder = AsyncZstdDecoder::new(reader);
    let output = tokio::fs::File::create(&dst)
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Cannot create output file: {e}")))?;
    let mut writer = AsyncLimitedWriter::new(tokio::io::BufWriter::new(output), max_out);
    tokio::io::copy(&mut decoder, &mut writer)
        .await
        .map_err(|e| MCSError::FilesystemError(format!("zstd decompression failed: {e}")))?;
    let decompressed_size = writer.written;

    Ok(json!({
        "success": true,
        "source": valid_path.to_string_lossy(),
        "output": output_path.to_string_lossy(),
        "algorithm": "zstd",
        "decompressedSize": decompressed_size,
    }))
}

pub async fn compress_tar(args: Option<&Value>, config: &Config) -> Result<Value> {
    let source = get_str_arg(args, "source")?;
    let output = get_str_arg(args, "output")?;
    let compression = get_opt_str(args, "compression").unwrap_or_else(|| "none".to_string());

    let valid_source = config.sandbox().resolve_path(&source)?;
    let output_path = config.sandbox().resolve_destination_path(&output)?;

    if output_path == valid_source || output_path.starts_with(&valid_source) {
        return Err(MCSError::InvalidParams(
            "Output path must not be inside the source directory".into(),
        ));
    }

    let source_clone = valid_source.clone();
    let output_clone = output_path.clone();
    let comp_clone = compression.clone();
    let follow = config.server.follow_symlinks;
    let result = blocking("Tar", move || {
        let entries = collect_tar_entries(&source_clone, follow)?;
        create_tar_archive(&source_clone, &output_clone, &entries, &comp_clone)
    })
    .await?;

    Ok(json!({
        "success": true,
        "source": valid_source.to_string_lossy(),
        "output": output_path.to_string_lossy(),
        "compression": compression,
        "entries": result.entries,
        "totalSize": result.total_size,
    }))
}

pub async fn decompress_tar(args: Option<&Value>, config: &Config) -> Result<Value> {
    let path = get_str_arg(args, "path")?;
    let output_dir = get_str_arg(args, "outputDir")?;

    let valid_path = config.sandbox().resolve_path(&path)?;
    let output_path = config.sandbox().resolve_destination_path(&output_dir)?;

    let src = valid_path.clone();
    let dst = output_path.clone();
    let max_out = config.max_decompressed_size;
    let result = blocking("Extract", move || {
        extract_tar_archive_streaming(&src, &dst, max_out)
    })
    .await?;

    Ok(json!({
        "success": true,
        "source": valid_path.to_string_lossy(),
        "outputDir": output_path.to_string_lossy(),
        "extracted": result.extracted,
        "totalSize": result.total_size,
    }))
}

struct TarResult {
    entries: u64,
    total_size: u64,
}

struct ExtractResult {
    extracted: u64,
    total_size: u64,
}

fn collect_tar_entries(
    source: &std::path::Path,
    follow_symlinks: bool,
) -> std::result::Result<Vec<PathBuf>, String> {
    let mut entries = Vec::new();
    if source.is_dir() {
        let walker = WalkDir::new(source)
            .follow_links(follow_symlinks)
            .into_iter()
            .filter_entry(|e| !is_hidden(e));
        for entry in walker.filter_map(|e| e.ok()) {
            if entry.path() != source {
                entries.push(entry.path().to_path_buf());
            }
        }
    } else {
        entries.push(source.to_path_buf());
    }
    Ok(entries)
}

fn create_tar_archive(
    source: &std::path::Path,
    output: &std::path::Path,
    entries: &[PathBuf],
    compression: &str,
) -> std::result::Result<TarResult, String> {
    let file = std::fs::File::create(output).map_err(|e| format!("Cannot create tar file: {e}"))?;

    let write: Box<dyn std::io::Write> = match compression {
        "gzip" | "gz" => Box::new(GzEncoder::new(file, Compression::default())),
        "zstd" | "zst" => {
            let enc = zstd::stream::Encoder::new(file, 3)
                .map_err(|e| format!("Cannot create zstd encoder: {e}"))?;
            Box::new(enc)
        }
        _ => Box::new(file),
    };

    let mut archive = tar::Builder::new(write);
    let mut total_size = 0u64;

    for path in entries {
        let relative = path.strip_prefix(source).unwrap_or(path);
        if path.is_dir() {
            archive
                .append_dir(relative, path)
                .map_err(|e| format!("Cannot add directory to tar: {e}"))?;
        } else {
            let metadata =
                std::fs::metadata(path).map_err(|e| format!("Cannot read file metadata: {e}"))?;
            total_size += metadata.len();
            let mut file =
                std::fs::File::open(path).map_err(|e| format!("Cannot open file for tar: {e}"))?;
            let mut header = tar::Header::new_ustar();
            header
                .set_path(relative)
                .map_err(|e| format!("Invalid tar path: {e}"))?;
            header.set_size(metadata.len());
            header.set_mtime(
                metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            let archive_mode = {
                #[cfg(unix)]
                {
                    metadata.permissions().mode()
                }
                #[cfg(not(unix))]
                {
                    if metadata.permissions().readonly() {
                        0o444
                    } else {
                        0o644
                    }
                }
            };
            header.set_mode(archive_mode);
            header.set_cksum();
            archive
                .append(&header, &mut file)
                .map_err(|e| format!("Cannot add file to tar: {e}"))?;
        }
    }

    let entries_count = entries.len() as u64;
    let _ = archive
        .into_inner()
        .map_err(|e| format!("Cannot finalize tar: {e}"))?;

    Ok(TarResult {
        entries: entries_count,
        total_size,
    })
}

fn extract_tar_archive_streaming(
    src: &std::path::Path,
    output: &std::path::Path,
    max_total: u64,
) -> std::result::Result<ExtractResult, String> {
    std::fs::create_dir_all(output).map_err(|e| format!("Cannot create output directory: {e}"))?;

    let file = std::fs::File::open(src).map_err(|e| format!("Cannot open tar file: {e}"))?;
    let mut file = std::io::BufReader::new(file);

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|e| format!("Cannot read magic bytes: {e}"))?;

    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| format!("Cannot seek back: {e}"))?;
    let file = file.into_inner();

    let reader: Box<dyn std::io::Read> = if magic[..3] == [0x1f, 0x8b, 0x08] {
        Box::new(GzDecoder::new(file))
    } else if magic == [0x28, 0xb5, 0x2f, 0xfd] {
        Box::new(
            zstd::stream::Decoder::new(file)
                .map_err(|e| format!("Cannot create zstd decoder: {e}"))?,
        )
    } else {
        Box::new(file)
    };

    let mut archive = tar::Archive::new(reader);
    let mut extracted = 0u64;
    let mut total_size = 0u64;

    for entry in archive
        .entries()
        .map_err(|e| format!("Cannot read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("Cannot read tar entry: {e}"))?;

        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(
                "Tar archive contains symlink/hardlink entries, which are not allowed".to_string(),
            );
        }

        let path = entry
            .path()
            .map_err(|e| format!("Cannot read entry path: {e}"))?
            .to_path_buf();

        let target = if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("Unsafe tar path: {}", path.display()));
        } else {
            output.join(&path)
        };

        total_size = total_size.saturating_add(entry.size());
        if total_size > max_total {
            return Err(format!(
                "Tar extraction exceeds maximum allowed size of {max_total} bytes"
            ));
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create parent directory: {e}"))?;
        }

        entry
            .unpack(&target)
            .map_err(|e| format!("Cannot unpack tar entry: {e}"))?;
        extracted += 1;
    }

    Ok(ExtractResult {
        extracted,
        total_size,
    })
}
