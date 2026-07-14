use cap_std::ambient_authority;
use cap_std::fs::Dir;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::errors::{MCSError, Result};
use crate::structures::PathTrie;

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => {
                result.push(component);
            }
        }
    }
    result
}

fn canonicalize_or_parent(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(c) => Ok(c),
        Err(_) => {
            let parent = path.parent().ok_or_else(|| {
                MCSError::PathNotAllowed(format!("Cannot resolve path: {}", path.display()))
            })?;
            let filename = path.file_name().ok_or_else(|| {
                MCSError::PathNotAllowed(format!("Cannot resolve path: {}", path.display()))
            })?;
            let parent_canon = parent.canonicalize().map_err(|_| {
                MCSError::PathNotAllowed(format!("Cannot resolve path: {}", path.display()))
            })?;
            Ok(parent_canon.join(filename))
        }
    }
}

fn to_abs(path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

/// OS-level capability-backed sandbox. Each allowed directory gets a `cap_std::fs::Dir`
/// handle that enforces path containment via `openat2(RESOLVE_BENEATH)` on Linux ≥5.6,
/// `openat(O_RESOLVE_BENEATH)` on FreeBSD ≥13, and manual component-walking elsewhere.
pub struct Sandbox {
    roots: HashMap<PathBuf, Dir>,
    trie: Arc<PathTrie>,
    follow_symlinks: bool,
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sandbox")
            .field("roots", &self.roots.keys())
            .field("follow_symlinks", &self.follow_symlinks)
            .finish()
    }
}

impl Sandbox {
    pub fn new(config: &Config) -> Result<Self> {
        let mut roots = HashMap::new();
        for dir_str in &config.allowed_directories {
            let abs = to_abs(dir_str);
            let canonical = abs.canonicalize().map_err(|e| {
                MCSError::FilesystemError(format!(
                    "Cannot canonicalize allowed directory {}: {}",
                    dir_str, e,
                ))
            })?;
            let dir = Dir::open_ambient_dir(&canonical, ambient_authority()).map_err(|e| {
                MCSError::FilesystemError(format!(
                    "Failed to open sandboxed directory {}: {}",
                    dir_str, e,
                ))
            })?;
            roots.insert(canonical, dir);
        }
        Ok(Self {
            roots,
            trie: Arc::clone(config.allowed_trie_ref()),
            follow_symlinks: config.server.follow_symlinks,
        })
    }

    /// Validate `path` and resolve it to a reusable [`Resolved`] handle. This is
    /// the single choke point for the sandbox's security checks — symlink
    /// rejection, canonicalization, and allowed-root containment. A handler that
    /// touches the same path more than once resolves it here a single time and
    /// reuses the handle, paying the (blocking) canonicalization cost only once.
    ///
    /// Non-existent paths are permitted (their parent is canonicalized), so the
    /// same method serves both source and destination resolution.
    pub fn resolve(&self, path: &str) -> Result<Resolved> {
        let abs = to_abs(path);
        let normalized = normalize_path(&abs);

        if !self.follow_symlinks {
            check_symlinks_in_path(&normalized).map_err(|_| {
                MCSError::PathNotAllowed(format!(
                    "Symlink component in path is not allowed: {path}"
                ))
            })?;
        }

        let canonical = canonicalize_or_parent(&normalized)?;

        let root_path = self
            .trie
            .longest_prefix(&canonical)
            .ok_or_else(|| MCSError::PathNotAllowed(path.to_string()))?;

        let dir = self
            .roots
            .get(&root_path)
            .ok_or_else(|| MCSError::PathNotAllowed(path.to_string()))?;
        let dir = dir
            .try_clone()
            .map_err(|e| MCSError::FilesystemError(format!("Failed to clone Dir handle: {e}")))?;

        let rel = canonical
            .strip_prefix(&root_path)
            .map_err(|_| MCSError::PathNotAllowed(path.to_string()))?;
        // When the requested path *is* the allowed root, `strip_prefix` yields an
        // empty path, which cap-std rejects ("No such file or directory"). Use "."
        // so capability-handle operations target the root directory itself.
        let rel = if rel.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            rel.to_path_buf()
        };

        Ok(Resolved {
            dir,
            rel,
            requested: path.to_string(),
            canonical,
        })
    }

    /// Validate `path` and return only its canonical form, for handlers that
    /// operate through `std::fs` (compression, tar, mmap) rather than the
    /// capability handle.
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        Ok(self.resolve(path)?.canonical)
    }

    /// Destination variant of [`resolve_path`]. Validation is identical — a
    /// non-existent path canonicalizes against its parent — but the distinct
    /// name documents intent at write-side call sites.
    pub fn resolve_destination_path(&self, path: &str) -> Result<PathBuf> {
        Ok(self.resolve(path)?.canonical)
    }

    // ── Two-path operations. These resolve both endpoints (with cross-root
    //    handling) so they stay on `Sandbox` rather than a single-path handle. ──

    /// Rename (move) a file or directory.
    pub async fn rename(&self, src: &str, dst: &str) -> Result<()> {
        let src_abs = canonicalize_or_parent(&normalize_path(&to_abs(src)))?;
        let dst_abs = canonicalize_or_parent(&normalize_path(&to_abs(dst)))?;

        let src_root = self
            .trie
            .longest_prefix(&src_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(src.to_string()))?;
        let dst_root = self
            .trie
            .longest_prefix(&dst_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(dst.to_string()))?;

        let follow = self.follow_symlinks;
        let src_owned = src.to_string();
        let dst_owned = dst.to_string();

        if dst_abs.exists() {
            return Err(MCSError::FilesystemError(format!(
                "Destination already exists: {}",
                dst_abs.display(),
            )));
        }

        if src_root == dst_root {
            let dir = self
                .roots
                .get(&src_root)
                .ok_or_else(|| MCSError::PathNotAllowed(src.to_string()))?;
            let dir = dir.try_clone().map_err(|e| {
                MCSError::FilesystemError(format!("Failed to clone Dir handle: {e}"))
            })?;
            let dst_dir = dir.try_clone().map_err(|e| {
                MCSError::FilesystemError(format!("Failed to clone Dir handle: {e}"))
            })?;
            let rel_src = src_abs.strip_prefix(&src_root).unwrap().to_path_buf();
            let rel_dst = dst_abs.strip_prefix(&dst_root).unwrap().to_path_buf();
            tokio::task::spawn_blocking(move || {
                dir.rename(&rel_src, &dst_dir, &rel_dst)
                    .map_err(|e| MCSError::FilesystemError(format!("Cannot rename: {e}")))
            })
            .await
            .map_err(|e| MCSError::FilesystemError(format!("Rename task failed: {e}")))?
            .map_err(|e| MCSError::FilesystemError(format!("Cannot rename: {e}")))?;
        } else {
            if !follow {
                check_symlinks_in_path(&src_abs).map_err(|_| {
                    MCSError::PathNotAllowed(format!("Symlink in source path: {src_owned}"))
                })?;
                check_symlinks_in_path(&dst_abs).map_err(|_| {
                    MCSError::PathNotAllowed(format!("Symlink in destination path: {dst_owned}"))
                })?;
            }
            tokio::task::spawn_blocking(move || {
                std::fs::rename(&src_abs, &dst_abs)
                    .map_err(|e| MCSError::FilesystemError(format!("Cannot rename: {e}")))
            })
            .await
            .map_err(|e| MCSError::FilesystemError(format!("Rename task failed: {e}")))?
            .map_err(|e| MCSError::FilesystemError(format!("Cannot rename: {e}")))?;
        }
        Ok(())
    }

    /// Copy a file.
    pub async fn copy(&self, src: &str, dst: &str) -> Result<u64> {
        let src_abs = canonicalize_or_parent(&normalize_path(&to_abs(src)))?;
        let dst_abs = canonicalize_or_parent(&normalize_path(&to_abs(dst)))?;

        let _src_root = self
            .trie
            .longest_prefix(&src_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(src.to_string()))?;
        let _dst_root = self
            .trie
            .longest_prefix(&dst_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(dst.to_string()))?;

        let follow = self.follow_symlinks;
        if !follow {
            check_symlinks_in_path(&src_abs)
                .map_err(|_| MCSError::PathNotAllowed(format!("Symlink in source path: {src}")))?;
            check_symlinks_in_path(&dst_abs).map_err(|_| {
                MCSError::PathNotAllowed(format!("Symlink in destination path: {dst}"))
            })?;
        }

        tokio::task::spawn_blocking(move || {
            std::fs::copy(&src_abs, &dst_abs)
                .map_err(|e| MCSError::FilesystemError(format!("Cannot copy: {e}")))
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Copy task failed: {e}")))?
    }

    /// Create a symlink (Unix only, within sandbox).
    pub async fn create_symlink(&self, src: &str, link: &str) -> Result<()> {
        let src_abs = canonicalize_or_parent(&normalize_path(&to_abs(src)))?;
        let link_abs = canonicalize_or_parent(&normalize_path(&to_abs(link)))?;

        let _src_root = self
            .trie
            .longest_prefix(&src_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(src.to_string()))?;
        let _link_root = self
            .trie
            .longest_prefix(&link_abs)
            .ok_or_else(|| MCSError::PathNotAllowed(link.to_string()))?;

        #[cfg(unix)]
        {
            tokio::task::spawn_blocking(move || {
                std::os::unix::fs::symlink(&src_abs, &link_abs)
                    .map_err(|e| MCSError::FilesystemError(format!("Cannot create symlink: {e}")))
            })
            .await
            .map_err(|e| MCSError::FilesystemError(format!("Symlink task failed: {e}")))?
            .map_err(|e| MCSError::FilesystemError(format!("Cannot create symlink: {e}")))?;
        }

        #[cfg(windows)]
        {
            let _ = (src_abs, link_abs);
            return Err(MCSError::FilesystemError(
                "Symlinks not supported through sandbox on Windows yet".into(),
            ));
        }

        Ok(())
    }
}

/// A path that has passed the sandbox's security checks, bundled with the
/// capability handle for its allowed root. Produced once by [`Sandbox::resolve`];
/// every IO method reuses the captured handle and relative path, so repeated
/// operations on the same path do not re-canonicalize.
pub struct Resolved {
    dir: Dir,
    /// Path relative to the allowed root, for the capability handle.
    rel: PathBuf,
    /// The path exactly as the caller requested it, used in error messages.
    requested: String,
    /// Canonical absolute path within an allowed root.
    pub canonical: PathBuf,
}

impl Resolved {
    /// Duplicate the capability handle so it can be moved into a blocking task.
    fn clone_dir(&self) -> Result<Dir> {
        self.dir
            .try_clone()
            .map_err(|e| MCSError::FilesystemError(format!("Failed to clone Dir handle: {e}")))
    }

    /// Read the entire file into a byte vector.
    pub async fn read(&self) -> Result<Vec<u8>> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = dir.open(&rel).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    MCSError::PathNotFound(requested)
                } else {
                    MCSError::FilesystemError(format!("Cannot read file: {e}"))
                }
            })?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| MCSError::FilesystemError(format!("Cannot read file: {e}")))?;
            Ok(buf)
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Read task failed: {e}")))?
    }

    /// Read the entire file into a String (lossy UTF-8).
    pub async fn read_to_string(&self) -> Result<String> {
        let data = self.read().await?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    /// Write bytes to the file (creates or overwrites).
    pub async fn write(&self, content: &[u8]) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        let content = content.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut file = dir.create(&rel).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot create file '{}': {e}", requested))
            })?;
            file.write_all(&content)
                .map_err(|e| MCSError::FilesystemError(format!("Cannot write file: {e}")))?;
            file.flush()
                .map_err(|e| MCSError::FilesystemError(format!("Cannot flush file: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Write task failed: {e}")))?
    }

    /// Fetch file metadata.
    pub async fn metadata(&self) -> Result<cap_std::fs::Metadata> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.metadata(&rel).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    MCSError::PathNotFound(requested)
                } else {
                    MCSError::FilesystemError(format!("Cannot read metadata: {e}"))
                }
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Metadata task failed: {e}")))?
    }

    /// Create the directory and all parents.
    pub async fn create_dir_all(&self) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.create_dir_all(&rel).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot create directory '{}': {e}", requested))
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Create dir task failed: {e}")))?
    }

    /// Read directory entries as `(name, is_dir)` pairs.
    pub async fn read_dir(&self) -> Result<Vec<(String, bool)>> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            let read_dir = dir.read_dir(&rel).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot read directory '{}': {e}", requested))
            })?;
            let mut entries = Vec::new();
            for entry in read_dir {
                let entry = entry.map_err(|e| {
                    MCSError::FilesystemError(format!("Cannot read directory entry: {e}"))
                })?;
                let file_type = entry.file_type().map_err(|e| {
                    MCSError::FilesystemError(format!("Cannot read entry type: {e}"))
                })?;
                let name = entry.file_name().to_string_lossy().to_string();
                entries.push((name, file_type.is_dir()));
            }
            Ok(entries)
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Read dir task failed: {e}")))?
    }

    /// Remove a file.
    pub async fn remove_file(&self) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.remove_file(&rel).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot remove file '{}': {e}", requested))
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Remove file task failed: {e}")))?
    }

    /// Remove an empty directory.
    pub async fn remove_dir(&self) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.remove_dir(&rel).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot remove directory '{}': {e}", requested))
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Remove dir task failed: {e}")))?
    }

    /// Remove a directory and all its contents.
    pub async fn remove_dir_all(&self) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.remove_dir_all(&rel).map_err(|e| {
                MCSError::FilesystemError(format!(
                    "Cannot remove directory tree '{}': {e}",
                    requested
                ))
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Remove dir all task failed: {e}")))?
    }

    /// Set file permissions (Unix).
    pub async fn set_permissions(&self, perm: cap_std::fs::Permissions) -> Result<()> {
        let dir = self.clone_dir()?;
        let rel = self.rel.clone();
        let requested = self.requested.clone();
        tokio::task::spawn_blocking(move || {
            dir.set_permissions(&rel, perm).map_err(|e| {
                MCSError::FilesystemError(format!("Cannot set permissions on '{}': {e}", requested))
            })
        })
        .await
        .map_err(|e| MCSError::FilesystemError(format!("Set permissions task failed: {e}")))?
    }
}

/// Check that no component of the path is a symlink.
fn check_symlinks_in_path(path: &Path) -> std::result::Result<(), ()> {
    let mut current = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        return Err(());
    };

    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Prefix(_) => continue,
            Component::CurDir | Component::ParentDir => return Err(()),
            Component::Normal(name) => {
                current.push(name);
                if current.is_symlink() {
                    return Err(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.allowed_directories = vec![
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        ];
        c.rebuild_trie();
        c
    }

    #[test]
    fn test_sandbox_new() {
        let c = cfg();
        let sb = Sandbox::new(&c).unwrap();
        assert!(!sb.roots.is_empty());
    }

    #[tokio::test]
    async fn test_sandbox_read_write() {
        let c = cfg();
        let sb = Sandbox::new(&c).unwrap();
        let path = "target/sandbox_test_write.txt";
        sb.resolve(path)
            .unwrap()
            .write(b"hello sandbox")
            .await
            .unwrap();
        let content = sb.resolve(path).unwrap().read().await.unwrap();
        assert_eq!(&content, b"hello sandbox");
        let _ = sb.resolve(path).unwrap().remove_file().await;
    }

    #[test]
    fn test_resolve_rejects_escape() {
        let c = cfg();
        let sb = Sandbox::new(&c).unwrap();
        // A path climbing above every allowed root must be rejected.
        let res = sb.resolve("../../../../../../etc/passwd");
        assert!(matches!(res, Err(MCSError::PathNotAllowed(_))));
    }

    #[tokio::test]
    async fn test_resolve_root_operations() {
        let c = cfg();
        let sb = Sandbox::new(&c).unwrap();
        let root = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        // Operating on the allowed root itself must work (empty relative path
        // maps to "."), covering the former "Cannot read directory" bug.
        let r = sb.resolve(&root).unwrap();
        assert!(r.metadata().await.unwrap().is_dir());
        let entries = r.read_dir().await.unwrap();
        assert!(entries.iter().any(|(name, _)| name == "Cargo.toml"));
    }

    #[tokio::test]
    async fn test_resolve_handle_reuse() {
        let c = cfg();
        let sb = Sandbox::new(&c).unwrap();
        let path = "target/sandbox_handle_reuse.txt";
        // One resolve, several operations through the same handle.
        let r = sb.resolve(path).unwrap();
        r.write(b"reuse").await.unwrap();
        assert!(r.metadata().await.unwrap().is_file());
        assert_eq!(r.read().await.unwrap(), b"reuse");
        let _ = r.remove_file().await;
    }

    #[test]
    fn test_normalize_path() {
        let p = Path::new("/a/b/../c/./d");
        let n = normalize_path(p);
        assert_eq!(n, PathBuf::from("/a/c/d"));
    }

    #[test]
    fn test_check_symlinks_clean_path() {
        // Use a non-existent path under root to avoid macOS /tmp → /private/tmp symlink.
        let p = Path::new("/nonexistent_dir_xyzabc/nonexistent_file");
        assert!(check_symlinks_in_path(p).is_ok());
    }
}
