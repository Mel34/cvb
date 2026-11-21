//! Checks a parsed patch against the real tree before anything runs.
//!
//! The parser already guarantees every path is relative, clean and unique. This
//! step looks at the filesystem under the root (the shell's directory):
//!
//! * no path may pass through a symlink (CVB's rule),
//! * files to update, move or delete must exist and be regular files,
//! * files to add, and move destinations, must not exist (decision D8: the LLM
//!   meant "new file"; to replace a file it must use Update),
//! * nothing may lie inside CVB's own state directory.
//!
//! Nothing is modified.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::patch::parse::{Change, ParsedPatch};

/// Why a patch cannot be applied to this tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuardError {
    Missing { path: String, action: &'static str },
    NotRegularFile { path: String },
    Symlink { path: String },
    AddTargetExists { path: String },
    MoveTargetExists { path: String },
    ParentNotDirectory { path: String, parent: String },
    InsideStateDir { path: String },
    Inspect { path: String, message: String },
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::Missing { path, action } => {
                write!(f, "{action} target does not exist: {path}")
            }
            GuardError::NotRegularFile { path } => {
                write!(f, "target is not a regular file: {path}")
            }
            GuardError::Symlink { path } => {
                write!(f, "path passes through a symlink, which is not allowed: {path}")
            }
            GuardError::AddTargetExists { path } => {
                write!(f, "Add File target already exists: {path}")
            }
            GuardError::MoveTargetExists { path } => {
                write!(f, "Move to target already exists: {path}")
            }
            GuardError::ParentNotDirectory { path, parent } => {
                write!(f, "cannot create {path}: {parent} is not a directory")
            }
            GuardError::InsideStateDir { path } => {
                write!(f, "path is inside CVB's state directory: {path}")
            }
            GuardError::Inspect { path, message } => {
                write!(f, "cannot inspect {path}: {message}")
            }
        }
    }
}

impl std::error::Error for GuardError {}

/// What the walk along a path found.
enum Walk {
    /// Every component exists; the last one is this kind.
    Exists(fs::Metadata),
    /// Components up to (not including) `missing` exist as directories.
    Absent,
}

/// Walks `relative` under `root` with `symlink_metadata`, refusing symlinks.
fn walk(root: &Path, relative: &str) -> Result<Walk, GuardError> {
    let mut current = root.to_path_buf();
    let mut components = Path::new(relative).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            // The parser rejects these; refuse rather than trust.
            return Err(GuardError::Inspect {
                path: relative.to_string(),
                message: "path is not normalized".to_string(),
            });
        };
        current.push(name);
        let last = components.peek().is_none();
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GuardError::Symlink { path: relative.to_string() });
                }
                if last {
                    return Ok(Walk::Exists(meta));
                }
                if !meta.is_dir() {
                    return Err(GuardError::ParentNotDirectory {
                        path: relative.to_string(),
                        parent: current
                            .strip_prefix(root)
                            .unwrap_or(&current)
                            .display()
                            .to_string(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Everything below a missing directory is missing too.
                return Ok(Walk::Absent);
            }
            Err(error) => {
                return Err(GuardError::Inspect {
                    path: relative.to_string(),
                    message: error.to_string(),
                })
            }
        }
    }
    Ok(Walk::Absent)
}

fn require_regular(root: &Path, path: &str, action: &'static str) -> Result<(), GuardError> {
    match walk(root, path)? {
        Walk::Absent => Err(GuardError::Missing { path: path.to_string(), action }),
        Walk::Exists(meta) if meta.is_file() => Ok(()),
        Walk::Exists(_) => Err(GuardError::NotRegularFile { path: path.to_string() }),
    }
}

fn require_absent(
    root: &Path,
    path: &str,
    exists: impl FnOnce(String) -> GuardError,
) -> Result<(), GuardError> {
    match walk(root, path)? {
        Walk::Absent => Ok(()),
        Walk::Exists(_) => Err(exists(path.to_string())),
    }
}

/// For a patch path that does not exist under `root`: a line saying where the
/// file does exist, when it is somewhere obvious. Either in a parent directory
/// (the patch was written for the project root and the shell is in a
/// subdirectory, or the path starts with the project folder's name) or here
/// without the path's first component (the patch names a folder that is not
/// part of this tree). Nothing is guessed beyond that and nothing is applied.
pub(crate) fn missing_hint(root: &Path, path: &str) -> Option<String> {
    let is_file_at = |base: &Path, relative: &str| {
        matches!(walk(base, relative), Ok(Walk::Exists(meta)) if meta.is_file())
    };
    for ancestor in root.ancestors().skip(1).take(8) {
        if is_file_at(ancestor, path) {
            return Some(format!(
                "Note: {path} exists in {}, a parent of this directory ({}). \
                 Change to that directory and paste the patch again.",
                ancestor.display(),
                root.display()
            ));
        }
    }
    let (first, rest) = path.split_once('/')?;
    if is_file_at(root, rest) {
        return Some(format!(
            "Note: {rest} exists in this directory. The patch path starts with {first}/, \
             which is not a folder here; it was probably written for another directory."
        ));
    }
    None
}

/// Checks every operation of `parsed` against the tree under `root`, in patch
/// order, and reports the first problem. `root` should be an absolute path; the
/// optional `state_dir` is CVB's state directory (also absolute).
pub(crate) fn check_tree(
    parsed: &ParsedPatch,
    root: &Path,
    state_dir: Option<&Path>,
) -> Result<(), GuardError> {
    for op in &parsed.ops {
        for path in std::iter::once(op.path.as_str()).chain(match &op.change {
            Change::Move { to } => Some(to.as_str()),
            _ => None,
        }) {
            if let Some(state) = state_dir {
                let full: PathBuf = root.join(path);
                if full.starts_with(state) {
                    return Err(GuardError::InsideStateDir { path: path.to_string() });
                }
            }
        }

        match &op.change {
            Change::Add => require_absent(root, &op.path, |path| {
                GuardError::AddTargetExists { path }
            })?,
            Change::Update => require_regular(root, &op.path, "Update File")?,
            Change::Delete => require_regular(root, &op.path, "Delete File")?,
            Change::Move { to } => {
                require_regular(root, &op.path, "Update File")?;
                require_absent(root, to, |path| GuardError::MoveTargetExists { path })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::parse::parse_patch;
    use std::os::unix::fs::symlink;

    fn tree(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("cvb-guard-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "a\n").unwrap();
        fs::write(root.join("b.txt"), "b\n").unwrap();
        root
    }

    fn check(root: &Path, body: &str) -> Result<(), GuardError> {
        let parsed = parse_patch(&format!("*** Begin Patch\n{body}*** End Patch\n")).unwrap();
        check_tree(&parsed, root, None)
    }

    #[test]
    fn a_valid_mixed_patch_passes() {
        let root = tree("valid");
        check(
            &root,
            "*** Update File: src/a.rs\n@@\n-a\n+z\n\
             *** Add File: new/deep/c.txt\n+c\n\
             *** Delete File: b.txt\n",
        )
        .unwrap();
        check(&root, "*** Update File: src/a.rs\n*** Move to: src/moved/a.rs\n").unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn add_over_an_existing_file_is_rejected() {
        let root = tree("add-exists");
        assert_eq!(
            check(&root, "*** Add File: b.txt\n+x\n"),
            Err(GuardError::AddTargetExists { path: "b.txt".into() })
        );
        // Also when it is a directory, or a dangling symlink.
        assert!(matches!(
            check(&root, "*** Add File: src\n+x\n"),
            Err(GuardError::AddTargetExists { .. })
        ));
        symlink("nowhere", root.join("dangling")).unwrap();
        assert!(matches!(
            check(&root, "*** Add File: dangling\n+x\n"),
            Err(GuardError::Symlink { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_file_gets_a_hint_when_it_exists_in_a_parent() {
        let root = tree("hint-parent");
        let sub = root.join("src");
        // The shell is in src/ but the patch names src/a.rs: it exists one level up.
        let hint = missing_hint(&sub, "src/a.rs").expect("hint");
        assert!(hint.contains("src/a.rs exists in"), "{hint}");
        assert!(hint.contains(&root.display().to_string()), "{hint}");
        assert!(hint.contains("parent of this directory"), "{hint}");
        // Nothing found for a file that exists nowhere.
        assert_eq!(missing_hint(&sub, "src/nowhere.rs"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_file_gets_a_hint_when_the_first_component_is_foreign() {
        let root = tree("hint-prefix");
        let hint = missing_hint(&root, "project/src/a.rs").expect("hint");
        assert!(hint.contains("src/a.rs exists in this directory"), "{hint}");
        assert!(hint.contains("starts with project/"), "{hint}");
        assert_eq!(missing_hint(&root, "src/nowhere.rs"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_and_delete_need_an_existing_regular_file() {
        let root = tree("missing");
        assert_eq!(
            check(&root, "*** Update File: nope.txt\n@@\n-x\n"),
            Err(GuardError::Missing { path: "nope.txt".into(), action: "Update File" })
        );
        assert_eq!(
            check(&root, "*** Delete File: no/such/dir/x\n"),
            Err(GuardError::Missing { path: "no/such/dir/x".into(), action: "Delete File" })
        );
        assert_eq!(
            check(&root, "*** Delete File: src\n"),
            Err(GuardError::NotRegularFile { path: "src".into() })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn move_needs_a_free_destination() {
        let root = tree("move");
        assert_eq!(
            check(&root, "*** Update File: src/a.rs\n*** Move to: b.txt\n"),
            Err(GuardError::MoveTargetExists { path: "b.txt".into() })
        );
        assert_eq!(
            check(&root, "*** Update File: nope\n*** Move to: free\n"),
            Err(GuardError::Missing { path: "nope".into(), action: "Update File" })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlinks_are_refused_anywhere_on_the_path() {
        let root = tree("symlink");
        let outside = std::env::temp_dir().join(format!("cvb-guard-out-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("t.txt"), "t\n").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        symlink(root.join("b.txt"), root.join("file-link")).unwrap();

        for body in [
            "*** Update File: link/t.txt\n@@\n-t\n",
            "*** Add File: link/new.txt\n+x\n",
            "*** Delete File: link/t.txt\n",
            "*** Update File: file-link\n@@\n-b\n",
            "*** Delete File: file-link\n",
            "*** Update File: b.txt\n*** Move to: link/moved.txt\n",
        ] {
            assert!(
                matches!(check(&root, body), Err(GuardError::Symlink { .. })),
                "{body:?}"
            );
        }
        assert!(outside.join("t.txt").exists());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn a_file_where_a_directory_is_needed_is_rejected() {
        let root = tree("parent-file");
        assert_eq!(
            check(&root, "*** Add File: b.txt/inner.txt\n+x\n"),
            Err(GuardError::ParentNotDirectory {
                path: "b.txt/inner.txt".into(),
                parent: "b.txt".into()
            })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cvb_state_directory_is_off_limits() {
        let root = tree("state");
        let state = root.join(".cvb-state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("x"), "x\n").unwrap();
        let parsed = parse_patch(
            "*** Begin Patch\n*** Delete File: .cvb-state/x\n*** End Patch\n",
        )
        .unwrap();
        assert_eq!(
            check_tree(&parsed, &root, Some(&state)),
            Err(GuardError::InsideStateDir { path: ".cvb-state/x".into() })
        );
        // Without a state directory the same patch is fine.
        assert!(check_tree(&parsed, &root, None).is_ok());

        // A root that is itself inside the state directory: everything is refused.
        let inner = state.join("scratch");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("f"), "f\n").unwrap();
        let parsed =
            parse_patch("*** Begin Patch\n*** Delete File: f\n*** End Patch\n").unwrap();
        assert!(matches!(
            check_tree(&parsed, &inner, Some(&state)),
            Err(GuardError::InsideStateDir { .. })
        ));
        // A sibling that merely shares the name prefix is not inside it.
        fs::write(root.join(".cvb-state-notes"), "n\n").unwrap();
        let parsed =
            parse_patch("*** Begin Patch\n*** Delete File: .cvb-state-notes\n*** End Patch\n")
                .unwrap();
        assert!(check_tree(&parsed, &root, Some(&state)).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_first_problem_in_patch_order_is_reported_and_nothing_is_touched() {
        let root = tree("order");
        let before = fs::read_to_string(root.join("b.txt")).unwrap();
        let result = check(
            &root,
            "*** Delete File: b.txt\n*** Update File: missing\n@@\n-x\n*** Add File: src/a.rs\n+x\n",
        );
        assert_eq!(
            result,
            Err(GuardError::Missing { path: "missing".into(), action: "Update File" })
        );
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), before);
        assert!(root.join("src/a.rs").exists());
        let _ = fs::remove_dir_all(root);
    }
}
