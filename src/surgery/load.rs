use super::*;
use std::collections::HashMap;
use std::path::Path;

/// Reads the existing target files without modifying them.
///
/// Paths must be safe relative paths. Symlinks are rejected so a target cannot
/// redirect a read outside the supplied root.
pub(crate) const MAX_TARGET_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn load_originals(
    root: &Path,
    envelope: &Envelope,
) -> Result<HashMap<String, String>, LoadError> {
    load_originals_with_limits(root, envelope, MAX_TARGET_BYTES, MAX_AGGREGATE_BYTES)
}

pub(crate) fn load_originals_with_limit(
    root: &Path,
    envelope: &Envelope,
    max_target_bytes: u64,
) -> Result<HashMap<String, String>, LoadError> {
    load_originals_with_limits(root, envelope, max_target_bytes, MAX_AGGREGATE_BYTES)
}

pub(crate) fn load_originals_with_limits(
    root: &Path,
    envelope: &Envelope,
    max_target_bytes: u64,
    max_aggregate_bytes: u64,
) -> Result<HashMap<String, String>, LoadError> {
    use std::collections::HashSet;

    struct TargetFile {
        rel_path: String,
        fs_path: std::path::PathBuf,
    }

    let mut targets = Vec::with_capacity(envelope.files.len());
    let mut seen_paths = HashSet::new();
    let mut aggregate_bytes: u64 = 0;

    // Inspect target metadata and enforce size limits before reading file contents.
    for section in &envelope.files {
        if validate_relative_path(&section.path).is_err() {
            return Err(LoadError::UnsafePath(section.path.clone()));
        }
        if !seen_paths.insert(section.path.clone()) {
            return Err(LoadError::DuplicatePath(section.path.clone()));
        }

        let relative = Path::new(&section.path);
        let mut current = root.to_path_buf();

        for component in relative.components() {
            current.push(component.as_os_str());
            let metadata =
                std::fs::symlink_metadata(&current).map_err(|error| LoadError::Inspect {
                    path: current.display().to_string(),
                    message: error.to_string(),
                })?;

            if metadata.file_type().is_symlink() {
                return Err(LoadError::SymlinkTarget(current.display().to_string()));
            }
        }

        let metadata = std::fs::metadata(&current).map_err(|error| LoadError::Inspect {
            path: current.display().to_string(),
            message: error.to_string(),
        })?;

        if !metadata.is_file() {
            return Err(LoadError::NotRegularFile(current.display().to_string()));
        }

        let size = metadata.len();
        if size > max_target_bytes {
            return Err(LoadError::FileTooLarge {
                path: current.display().to_string(),
                size,
                limit: max_target_bytes,
            });
        }

        aggregate_bytes = aggregate_bytes.saturating_add(size);
        if aggregate_bytes > max_aggregate_bytes {
            return Err(LoadError::AggregateTooLarge {
                size: aggregate_bytes,
                limit: max_aggregate_bytes,
            });
        }

        targets.push(TargetFile {
            rel_path: section.path.clone(),
            fs_path: current,
        });
    }

    // All target sizes have been checked before any full-file read.
    let mut originals = HashMap::new();

    for target in targets {
        let bytes = std::fs::read(&target.fs_path).map_err(|error| LoadError::Read {
            path: target.fs_path.display().to_string(),
            message: error.to_string(),
        })?;

        let contents = String::from_utf8(bytes)
            .map_err(|_| LoadError::InvalidUtf8(target.fs_path.display().to_string()))?;

        if contents.contains('\0') {
            return Err(LoadError::NulByte(target.fs_path.display().to_string()));
        }

        if contents.starts_with('\u{FEFF}') {
            return Err(LoadError::Bom(target.fs_path.display().to_string()));
        }

        match detect_newline(&contents) {
            Err(NewlineError::LoneCr) => {
                return Err(LoadError::LoneCr(target.fs_path.display().to_string()));
            }
            Err(NewlineError::Mixed) => {
                return Err(LoadError::MixedLineEndings(
                    target.fs_path.display().to_string(),
                ));
            }
            Ok(_) => {}
        }

        originals.insert(target.rel_path, contents);
    }

    Ok(originals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn test_directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("cvb-load-originals-{name}-{}", std::process::id()));

        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn envelope(path: &str) -> Envelope {
        Envelope {
            token: "TOKEN123".to_owned(),
            id: "load-test".to_owned(),
            files: vec![FileSection {
                path: path.to_owned(),
                operations: vec![],
            }],
        }
    }

    #[test]
    fn load_originals_rejects_git_path() {
        let root = test_directory("git-path");
        let result = load_originals(&root, &envelope(".git/config"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsafe target path"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_existing_relative_file() {
        let root = test_directory("reads");
        fs::write(root.join("example.rs"), "fn example() {}\n").unwrap();

        let result = load_originals(&root, &envelope("example.rs")).unwrap();

        assert_eq!(
            result.get("example.rs").map(String::as_str),
            Some("fn example() {}\n")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_file() {
        let root = test_directory("missing");

        let result = load_originals(&root, &envelope("missing.rs"));

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unable to inspect"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_relative_path() {
        let root = test_directory("unsafe");

        let result = load_originals(&root, &envelope("../outside.rs"));

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsafe target path"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let root = test_directory("symlink");
        let outside = root.with_extension("outside");
        fs::write(&outside, "outside contents").unwrap();
        symlink(&outside, root.join("target.rs")).unwrap();

        let result = load_originals(&root, &envelope("target.rs"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("symlink target is not allowed")
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside contents");

        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn rejects_utf8_bom() {
        let root = test_directory("bom");
        let original: &[u8] = b"\xEF\xBB\xBFfn example() {}\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "UTF-8 BOM must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting a BOM must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_nul_byte() {
        let root = test_directory("nul");
        let original: &[u8] = b"fn example() {}\0\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "NUL bytes must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting NUL bytes must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_crlf_file() {
        let root = test_directory("crlf");
        let original = "first line\r\nsecond line\r\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs")).unwrap();

        assert_eq!(result.get("example.rs").map(String::as_str), Some(original));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_mixed_line_endings() {
        let root = test_directory("mixed-line-endings");
        let original: &[u8] = b"first line\nsecond line\r\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "mixed LF/CRLF endings must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting mixed line endings must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_lone_carriage_return() {
        let root = test_directory("lone-carriage-return");
        let original: &[u8] = b"first line\rsecond line\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "a lone CR must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting a lone CR must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn load_originals_accepts_file_at_size_limit() {
        let root = test_directory("size-limit-exact");
        let path = root.join("exact.rs");
        // Use a test limit constant or parameter to test without allocating 16 MiB:
        let test_limit: u64 = 100;
        let content = "a".repeat(test_limit as usize);
        fs::write(&path, &content).unwrap();

        let result = load_originals_with_limit(&root, &envelope("exact.rs"), test_limit);
        assert!(result.is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_originals_rejects_file_over_size_limit_without_reading() {
        let root = test_directory("size-limit-over");
        let path = root.join("oversize.rs");
        let test_limit: u64 = 100;
        let content = "a".repeat((test_limit + 1) as usize);
        fs::write(&path, &content).unwrap();

        let result = load_originals_with_limit(&root, &envelope("oversize.rs"), test_limit);
        match result {
            Err(LoadError::FileTooLarge { path, size, limit }) => {
                assert_eq!(size, 101);
                assert_eq!(limit, 100);
                assert!(path.contains("oversize.rs"));
            }
            other => panic!("expected FileTooLarge error, got {other:?}"),
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_originals_uses_eight_mebibytes_default_cap() {
        assert_eq!(MAX_TARGET_BYTES, 8 * 1024 * 1024);
        assert_eq!(MAX_AGGREGATE_BYTES, 32 * 1024 * 1024);
    }

    #[test]
    fn load_originals_accepts_aggregate_size_at_limit() {
        let root = test_directory("aggregate-exact");
        fs::write(root.join("file1.rs"), "a".repeat(50)).unwrap();
        fs::write(root.join("file2.rs"), "b".repeat(50)).unwrap();

        let env = Envelope {
            token: "TOKEN123".to_owned(),
            id: "load-test".to_owned(),
            files: vec![
                FileSection {
                    path: "file1.rs".to_owned(),
                    operations: vec![],
                },
                FileSection {
                    path: "file2.rs".to_owned(),
                    operations: vec![],
                },
            ],
        };

        let result = load_originals_with_limits(&root, &env, 80, 100);
        assert!(result.is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_originals_rejects_aggregate_size_over_limit_without_reading() {
        let root = test_directory("aggregate-over");
        fs::write(root.join("file1.rs"), "a".repeat(60)).unwrap();
        fs::write(root.join("file2.rs"), "b".repeat(60)).unwrap();

        let env = Envelope {
            token: "TOKEN123".to_owned(),
            id: "load-test".to_owned(),
            files: vec![
                FileSection {
                    path: "file1.rs".to_owned(),
                    operations: vec![],
                },
                FileSection {
                    path: "file2.rs".to_owned(),
                    operations: vec![],
                },
            ],
        };

        let result = load_originals_with_limits(&root, &env, 80, 100);
        match result {
            Err(LoadError::AggregateTooLarge { size, limit }) => {
                assert_eq!(size, 120);
                assert_eq!(limit, 100);
            }
            other => panic!("expected AggregateTooLarge error, got {other:?}"),
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_error_display_includes_protocol_codes() {
        assert!(
            LoadError::UnsafePath("test".into()).to_string().starts_with("CVB: E-PATH:"),
            "expected E-PATH"
        );
        assert!(
            LoadError::DuplicatePath("test".into()).to_string().starts_with("CVB: E-DUPFILE:"),
            "expected E-DUPFILE"
        );
        assert!(
            LoadError::Inspect { path: "test".into(), message: "not found".into() }
                .to_string()
                .starts_with("CVB: E-FILE:"),
            "expected E-FILE"
        );
        assert!(
            LoadError::FileTooLarge { path: "test".into(), size: 200, limit: 100 }
                .to_string()
                .starts_with("CVB: E-LIMIT:"),
            "expected E-LIMIT"
        );
    }
}
