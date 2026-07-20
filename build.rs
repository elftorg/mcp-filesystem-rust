use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/actions/files.rs");
    println!("cargo:rerun-if-changed=src/actions/compress.rs");
    println!("cargo:rerun-if-changed=src/validation.rs");

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));

    let files = patch_files(read_source("src/actions/files.rs"));
    write_generated(&out_dir, "files_windows.rs", &files);

    let compress = patch_compress(read_source("src/actions/compress.rs"));
    write_generated(&out_dir, "compress_windows.rs", &compress);

    let validation = patch_validation(read_source("src/validation.rs"));
    write_generated(&out_dir, "validation_windows.rs", &validation);
}

fn read_source(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
        .replace("\r\n", "\n")
}

fn write_generated(out_dir: &Path, name: &str, source: &str) {
    let path = out_dir.join(name);
    fs::write(&path, strip_inner_doc_comments(source))
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
}

fn strip_inner_doc_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            line.strip_prefix("//!")
                .map_or_else(|| line.to_owned(), |rest| format!("//{rest}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn patch_files(mut source: String) -> String {
    replace_once(
        &mut source,
        "    let permissions = format!(\"{:o}\", cap_meta.permissions().mode() & 0o777);",
        "    #[cfg(unix)]\n    let permissions = Some(format!(\"{:o}\", cap_meta.permissions().mode() & 0o777));\n    #[cfg(not(unix))]\n    let permissions: Option<String> = None;",
        "Unix permission metadata",
    );

    replace_section(
        &mut source,
        "pub async fn set_permissions",
        "\npub async fn create_symlink",
        r#"pub async fn set_permissions(args: Option<&Value>, config: &Config) -> Result<Value> {
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
"#,
        "set_permissions",
    );

    source
}

fn patch_compress(mut source: String) -> String {
    replace_once(
        &mut source,
        "            header.set_mode(metadata.permissions().mode());",
        r#"            let archive_mode = {
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
            header.set_mode(archive_mode);"#,
        "tar permission mode",
    );
    source
}

fn patch_validation(mut source: String) -> String {
    replace_section(
        &mut source,
        "    pub async fn create_symlink",
        "\n}\n\n/// A path",
        r#"    pub async fn create_symlink(&self, src: &str, link: &str) -> Result<()> {
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
                    .map_err(|error| MCSError::FilesystemError(format!("Cannot create symlink: {error}")))
            })
            .await
            .map_err(|error| MCSError::FilesystemError(format!("Symlink task failed: {error}")))?
            .map_err(|error| MCSError::FilesystemError(format!("Cannot create symlink: {error}")))?;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = (src_abs, link_abs);
            Err(MCSError::FilesystemError(
                "Symlinks are not supported through the sandbox on this platform".into(),
            ))
        }
    }
"#,
        "create_symlink",
    );

    replace_section(
        &mut source,
        "fn check_symlinks_in_path",
        "\n#[cfg(test)]",
        r#"fn check_symlinks_in_path(path: &Path) -> std::result::Result<(), ()> {
    if !path.is_absolute() {
        return Err(());
    }

    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
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
"#,
        "Windows symlink path traversal",
    );

    replace_once(
        &mut source,
        "        let p = Path::new(\"/nonexistent_dir_xyzabc/nonexistent_file\");\n        assert!(check_symlinks_in_path(p).is_ok());",
        "        let path = std::env::current_dir()\n            .unwrap()\n            .join(\"nonexistent_dir_xyzabc\")\n            .join(\"nonexistent_file\");\n        assert!(check_symlinks_in_path(&path).is_ok());",
        "platform-neutral clean path test",
    );

    source
}

fn replace_once(source: &mut String, old: &str, new: &str, label: &str) {
    let position = source
        .find(old)
        .unwrap_or_else(|| panic!("cannot find source fragment for {label}"));
    source.replace_range(position..position + old.len(), new);
}

fn replace_section(source: &mut String, start: &str, end: &str, replacement: &str, label: &str) {
    let start_position = source
        .find(start)
        .unwrap_or_else(|| panic!("cannot find start of {label}"));
    let relative_end = source[start_position..]
        .find(end)
        .unwrap_or_else(|| panic!("cannot find end of {label}"));
    let end_position = start_position + relative_end;
    source.replace_range(start_position..end_position, replacement);
}
