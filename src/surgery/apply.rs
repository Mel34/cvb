use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::surgery::journal::{
    check_replay_id, find_unfinished_journal, ProjectState,
};
use crate::surgery::lock::LockError;
use crate::surgery::Plan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplyError {
    Lock(LockError),
    Recover {
        journal_path: PathBuf,
        id: String,
    },
    Replay {
        id: String,
    },
    Stale {
        path: String,
        expected_digest: String,
        actual_digest: String,
    },
    Io {
        path: Option<String>,
        message: String,
    },
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Lock(err) => write!(f, "{err}"),
            ApplyError::Recover { journal_path, id } => {
                write!(
                    f,
                    "CVB: unfinished surgery transaction '{id}' found in '{}' (E-RECOVER); run recovery or resolve before continuing",
                    journal_path.display()
                )
            }
            ApplyError::Replay { id } => {
                write!(
                    f,
                    "CVB: surgery id '{id}' has already been applied to this project (E-REPLAY)"
                )
            }
            ApplyError::Stale {
                path,
                expected_digest,
                actual_digest,
            } => {
                write!(
                    f,
                    "CVB: target file '{path}' has changed since the plan was created (E-STALE): expected digest {expected_digest}, actual {actual_digest}"
                )
            }
            ApplyError::Io {
                path: Some(p),
                message,
            } => {
                write!(f, "CVB: target file '{p}' I/O error: {message}")
            }
            ApplyError::Io {
                path: None,
                message,
            } => {
                write!(f, "CVB: surgery I/O error: {message}")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

/// Checks pipeline Step 4 preconditions before any mutations occur:
/// 1. Unfinished earlier transaction (E-RECOVER)
/// 2. Replay of previously applied surgery id (E-REPLAY)
/// 3. Target file freshness against original plan digests (E-STALE)
pub(crate) fn check_pipeline_preconditions(
    project_state: &ProjectState,
    plan: &Plan,
) -> Result<(), ApplyError> {
    // 1. Check for any unfinished transaction (E-RECOVER)
    if let Some(unfinished) = find_unfinished_journal(&project_state.dir).map_err(|e| {
        ApplyError::Io {
            path: None,
            message: e.to_string(),
        }
    })? {
        let journal_path = project_state.dir.join(format!("{}.journal", unfinished.id));
        return Err(ApplyError::Recover {
            journal_path,
            id: unfinished.id,
        });
    }

    // 2. Check if the surgery id was already applied (E-REPLAY)
    if check_replay_id(&project_state.dir, &plan.id).map_err(|e| ApplyError::Io {
        path: None,
        message: e.to_string(),
    })? {
        return Err(ApplyError::Replay {
            id: plan.id.clone(),
        });
    }

    // 3. Re-check file digests against the plan's original digests (E-STALE)
    for file in &plan.files {
        let full_path = project_state.canonical_root.join(&file.path);
        let current_bytes = fs::read(&full_path).map_err(|e| ApplyError::Io {
            path: Some(file.path.clone()),
            message: e.to_string(),
        })?;

        let mut hasher = Sha256::new();
        hasher.update(&current_bytes);
        let current_digest = format!("{:x}", hasher.finalize());

        if current_digest != file.original_digest {
            return Err(ApplyError::Stale {
                path: file.path.clone(),
                expected_digest: file.original_digest.clone(),
                actual_digest: current_digest,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surgery::journal::{
        init_project_state, transition_journal_state, write_journal_atomic, Journal,
        JournalFileEntry, JournalState,
    };
    use crate::surgery::PlannedFile;
    use std::fs;

    fn make_test_plan(id: &str, files: Vec<(&str, &str, &str)>) -> Plan {
        let mut planned_files = Vec::new();
        for (path, orig, prop) in files {
            let mut h1 = Sha256::new();
            h1.update(orig.as_bytes());
            let orig_dig = format!("{:x}", h1.finalize());

            let mut h2 = Sha256::new();
            h2.update(prop.as_bytes());
            let prop_dig = format!("{:x}", h2.finalize());

            planned_files.push(PlannedFile {
                path: path.to_string(),
                original: orig.to_string(),
                original_digest: orig_dig,
                proposed: prop.to_string(),
                proposed_digest: prop_dig,
            });
        }

        Plan {
            token: "tok".to_string(),
            id: id.to_string(),
            digest: "plan-digest".to_string(),
            files: planned_files,
        }
    }

    #[test]
    fn test_pipeline_preconditions_happy_path() {
        let base = std::env::temp_dir().join(format!("cvb-precond-happy-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("src").join("main.rs");
        fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        let contents = "fn main() {\n    println!(\"hello\");\n}\n";
        fs::write(&target_file, contents).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("happy-id", vec![("src/main.rs", contents, "new content")]);

        let res = check_pipeline_preconditions(&state, &plan);
        assert!(res.is_ok(), "preconditions should pass on fresh matching state");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_pipeline_preconditions_rejects_unfinished_prepared_journal() {
        let base =
            std::env::temp_dir().join(format!("cvb-precond-recover-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("file.txt");
        fs::write(&target_file, "orig\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();

        // Write an unfinished (prepared) journal from a previous crash/attempt
        let unfinished = Journal {
            state: JournalState::Prepared,
            id: "unfinished-surgery".to_string(),
            canonical_root: state.canonical_root.clone(),
            dev: state.dev,
            ino: state.ino,
            files: vec![],
        };
        write_journal_atomic(&state.dir, &unfinished).unwrap();

        let plan = make_test_plan("fresh-id", vec![("file.txt", "orig\n", "prop\n")]);

        match check_pipeline_preconditions(&state, &plan) {
            Err(ApplyError::Recover { id, journal_path }) => {
                assert_eq!(id, "unfinished-surgery");
                assert!(journal_path.ends_with("unfinished-surgery.journal"));
            }
            other => panic!("expected ApplyError::Recover, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_pipeline_preconditions_rejects_replayed_id_committed() {
        let base = std::env::temp_dir().join(format!("cvb-precond-replay-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("file.txt");
        fs::write(&target_file, "orig\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();

        let journal = Journal {
            state: JournalState::Committed,
            id: "replayed-id".to_string(),
            canonical_root: state.canonical_root.clone(),
            dev: state.dev,
            ino: state.ino,
            files: vec![],
        };
        write_journal_atomic(&state.dir, &journal).unwrap();

        let plan = make_test_plan("replayed-id", vec![("file.txt", "orig\n", "prop\n")]);

        match check_pipeline_preconditions(&state, &plan) {
            Err(ApplyError::Replay { id }) => {
                assert_eq!(id, "replayed-id");
            }
            other => panic!("expected ApplyError::Replay, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_pipeline_preconditions_allows_replayed_id_rolled_back() {
        let base =
            std::env::temp_dir().join(format!("cvb-precond-rollback-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("file.txt");
        fs::write(&target_file, "orig\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();

        let mut journal = Journal {
            state: JournalState::Prepared,
            id: "reused-id".to_string(),
            canonical_root: state.canonical_root.clone(),
            dev: state.dev,
            ino: state.ino,
            files: vec![],
        };
        write_journal_atomic(&state.dir, &journal).unwrap();
        transition_journal_state(&state.dir, &mut journal, JournalState::RolledBack).unwrap();

        let plan = make_test_plan("reused-id", vec![("file.txt", "orig\n", "prop\n")]);

        let res = check_pipeline_preconditions(&state, &plan);
        assert!(
            res.is_ok(),
            "rolled-back journal must not block reuse of its id"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_pipeline_preconditions_rejects_stale_file_digest() {
        let base = std::env::temp_dir().join(format!("cvb-precond-stale-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("file.txt");
        fs::write(&target_file, "disk modified content\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();

        // Plan expects "expected original content"
        let plan = make_test_plan(
            "stale-id",
            vec![("file.txt", "expected original content\n", "new content")],
        );

        match check_pipeline_preconditions(&state, &plan) {
            Err(ApplyError::Stale { path, .. }) => {
                assert_eq!(path, "file.txt");
            }
            other => panic!("expected ApplyError::Stale, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_pipeline_preconditions_rejects_missing_file() {
        let base =
            std::env::temp_dir().join(format!("cvb-precond-missing-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();

        let plan = make_test_plan(
            "missing-id",
            vec![("nonexistent.txt", "expected\n", "new\n")],
        );

        match check_pipeline_preconditions(&state, &plan) {
            Err(ApplyError::Io {
                path: Some(ref p), ..
            }) => {
                assert_eq!(p, "nonexistent.txt");
            }
            other => panic!("expected ApplyError::Io for missing file, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }
}
