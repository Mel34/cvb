use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::surgery::journal::{
    check_replay_id, find_unfinished_journal, transition_journal_state, write_journal_atomic,
    Journal, JournalFileEntry, JournalState, ProjectState,
};
use crate::surgery::lock::{LockError, ProjectLock};
use crate::surgery::validate_relative_path;
use crate::surgery::Plan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitCrashPoint {
    JournalPrepared,
    TempWrite(usize),
    Rename(usize),
    BeforeRename(usize),
    ReadBack,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileClassification {
    Original,
    Planned,
    Other { actual_digest: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileConflict {
    pub path: String,
    pub expected_original: String,
    pub expected_planned: String,
    pub found_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryOutcome {
    NothingToDo(JournalState),
    RolledForward,
    RolledBack,
    Conflict(Vec<FileConflict>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum SurgeryExitStatus {
    Applied = 0,
    RejectedBeforeCommit = 2,
    UserDeclined = 3,
    CommitFailure = 4,
}

impl SurgeryExitStatus {
    pub(crate) fn code(&self) -> i32 {
        *self as i32
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileRestorationStatus {
    Restored,
    NotRestorable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileReport {
    pub path: String,
    pub status: FileRestorationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitFailureReport {
    pub message: String,
    pub files: Vec<FileReport>,
}

impl std::fmt::Display for CommitFailureReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "CVB: surgery failed during commit (E-IO): {}", self.message)?;
        for file in &self.files {
            match &file.status {
                FileRestorationStatus::Restored => {
                    writeln!(f, "  {}: restored to original bytes", file.path)?;
                }
                FileRestorationStatus::NotRestorable { reason } => {
                    writeln!(f, "  {}: not restorable: {reason}", file.path)?;
                }
            }
        }
        Ok(())
    }
}

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
    CommitFailed(CommitFailureReport),
    FaultInjected(String),
}

impl ApplyError {
    pub(crate) fn exit_status(&self) -> SurgeryExitStatus {
        match self {
            ApplyError::Lock(_)
            | ApplyError::Recover { .. }
            | ApplyError::Replay { .. }
            | ApplyError::Stale { .. } => SurgeryExitStatus::RejectedBeforeCommit,
            ApplyError::Io { .. }
            | ApplyError::CommitFailed(_)
            | ApplyError::FaultInjected(_) => SurgeryExitStatus::CommitFailure,
        }
    }
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
            ApplyError::CommitFailed(report) => {
                write!(f, "{report}")
            }
            ApplyError::FaultInjected(point) => {
                write!(f, "CVB: simulated crash at '{point}'")
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

    if check_replay_id(&project_state.dir, &plan.id).map_err(|e| ApplyError::Io {
        path: None,
        message: e.to_string(),
    })? {
        return Err(ApplyError::Replay {
            id: plan.id.clone(),
        });
    }

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

/// Applies a planned surgery transaction under the advisory project lock.
pub(crate) fn apply_plan(
    project_state: &ProjectState,
    plan: &Plan,
) -> Result<Journal, ApplyError> {
    let _lock = ProjectLock::acquire(&project_state.dir).map_err(ApplyError::Lock)?;
    commit_plan(project_state, plan)
}

/// Applies a planned surgery transaction under the advisory lock with a fault-injection hook.
pub(crate) fn apply_plan_with_hook<F>(
    project_state: &ProjectState,
    plan: &Plan,
    hook: F,
) -> Result<Journal, ApplyError>
where
    F: FnMut(CommitCrashPoint) -> Result<(), ApplyError>,
{
    let _lock = ProjectLock::acquire(&project_state.dir).map_err(ApplyError::Lock)?;
    commit_plan_with_hook(project_state, plan, hook)
}

/// Executes the commit sequence for `plan` against `project_state`.
pub(crate) fn commit_plan(
    project_state: &ProjectState,
    plan: &Plan,
) -> Result<Journal, ApplyError> {
    commit_plan_with_hook(project_state, plan, |_| Ok(()))
}

/// Executes the commit sequence with a fault-injection hook called between steps.
pub(crate) fn commit_plan_with_hook<F>(
    project_state: &ProjectState,
    plan: &Plan,
    mut hook: F,
) -> Result<Journal, ApplyError>
where
    F: FnMut(CommitCrashPoint) -> Result<(), ApplyError>,
{
    check_pipeline_preconditions(project_state, plan)?;

    let mut journal = Journal {
        state: JournalState::Prepared,
        id: plan.id.clone(),
        canonical_root: project_state.canonical_root.clone(),
        dev: project_state.dev,
        ino: project_state.ino,
        files: plan
            .files
            .iter()
            .map(|f| JournalFileEntry {
                path: f.path.clone(),
                original_digest: f.original_digest.clone(),
                planned_digest: f.proposed_digest.clone(),
                original_bytes: f.original.as_bytes().to_vec(),
            })
            .collect(),
    };

    write_journal_atomic(&project_state.dir, &journal).map_err(|e| ApplyError::Io {
        path: None,
        message: e.to_string(),
    })?;
    hook(CommitCrashPoint::JournalPrepared)?;

    let mut temp_paths = Vec::with_capacity(plan.files.len());

    let commit_result: Result<(), ApplyError> = (|| {
        // Step 2: Write proposed content to temp files in each target's directory, preserving mode
        for (i, file) in plan.files.iter().enumerate() {
            let target_path = project_state.canonical_root.join(&file.path);
            let target_dir = target_path.parent().ok_or_else(|| ApplyError::Io {
                path: Some(file.path.clone()),
                message: "target has no parent directory".to_string(),
            })?;
            let meta = fs::metadata(&target_path).map_err(|e| ApplyError::Io {
                path: Some(file.path.clone()),
                message: e.to_string(),
            })?;

            let temp_path = target_dir.join(format!(
                ".surgery.{}.{}.{}.tmp",
                plan.id,
                i,
                std::process::id()
            ));
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                use std::os::unix::fs::PermissionsExt;
                options.mode(meta.permissions().mode() & 0o777);
            }

            let mut temp_file = options.open(&temp_path).map_err(|e| ApplyError::Io {
                path: Some(file.path.clone()),
                message: e.to_string(),
            })?;
            use std::io::Write;
            temp_file
                .write_all(file.proposed.as_bytes())
                .map_err(|e| ApplyError::Io {
                    path: Some(file.path.clone()),
                    message: e.to_string(),
                })?;
            temp_file.sync_all().map_err(|e| ApplyError::Io {
                path: Some(file.path.clone()),
                message: e.to_string(),
            })?;
            drop(temp_file);
            temp_paths.push(temp_path);
            hook(CommitCrashPoint::TempWrite(i))?;
        }

        // Recheck every target after preparation and before the first rename.
        for file in &plan.files {
            let target_path = project_state.canonical_root.join(&file.path);
            let current_bytes = fs::read(&target_path).map_err(|e| ApplyError::Io {
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

        // Step 3: Per-rename digest recheck and rename
        for (i, (file, temp_path)) in plan.files.iter().zip(&temp_paths).enumerate() {
            let target_path = project_state.canonical_root.join(&file.path);
            let current_bytes = fs::read(&target_path).map_err(|e| ApplyError::Io {
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

            hook(CommitCrashPoint::BeforeRename(i))?;
            fs::rename(temp_path, &target_path).map_err(|e| ApplyError::Io {
                path: Some(file.path.clone()),
                message: e.to_string(),
            })?;
            hook(CommitCrashPoint::Rename(i))?;
        }

        // Step 4: Fsync directories and re-read verification
        let mut parent_dirs = HashSet::new();
        for file in &plan.files {
            let target_path = project_state.canonical_root.join(&file.path);
            if let Some(parent) = target_path.parent() {
                parent_dirs.insert(parent.to_path_buf());
            }
        }
        for dir in parent_dirs {
            if let Ok(d) = fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }

        for file in &plan.files {
            let target_path = project_state.canonical_root.join(&file.path);
            let read_back = fs::read_to_string(&target_path).map_err(|e| ApplyError::Io {
                path: Some(file.path.clone()),
                message: e.to_string(),
            })?;
            if read_back != file.proposed {
                return Err(ApplyError::Io {
                    path: Some(file.path.clone()),
                    message: "read-back content mismatch after commit rename".to_string(),
                });
            }
        }
        hook(CommitCrashPoint::ReadBack)?;

        Ok(())
    })();

    if let Err(err) = commit_result {
        if matches!(err, ApplyError::FaultInjected(_)) {
            return Err(err);
        }

        // Clean up remaining temporary files
        for temp in &temp_paths {
            let _ = fs::remove_file(temp);
        }

        // Execute recovery to roll back any files already renamed
        let outcome = recover_journal(project_state, &mut journal);

        // Build structured per-file failure report
        let mut file_reports = Vec::new();
        match outcome {
            Ok(RecoveryOutcome::Conflict(conflicts)) => {
                let conflict_map: std::collections::HashMap<_, _> = conflicts
                    .into_iter()
                    .map(|c| (c.path, c.found_digest))
                    .collect();

                for f in &plan.files {
                    if let Some(actual) = conflict_map.get(&f.path) {
                        file_reports.push(FileReport {
                            path: f.path.clone(),
                            status: FileRestorationStatus::NotRestorable {
                                reason: format!(
                                    "conflict with unknown content (digest {actual}, untouched)"
                                ),
                            },
                        });
                    } else {
                        file_reports.push(FileReport {
                            path: f.path.clone(),
                            status: FileRestorationStatus::Restored,
                        });
                    }
                }
            }
            Ok(RecoveryOutcome::RolledBack) => {
                for f in &plan.files {
                    file_reports.push(FileReport {
                        path: f.path.clone(),
                        status: FileRestorationStatus::Restored,
                    });
                }
            }
            Ok(RecoveryOutcome::RolledForward) => {
                for f in &plan.files {
                    file_reports.push(FileReport {
                        path: f.path.clone(),
                        status: FileRestorationStatus::NotRestorable {
                            reason: "transaction was rolled forward to planned content".to_string(),
                        },
                    });
                }
            }
            Ok(RecoveryOutcome::NothingToDo(_)) => {
                for f in &plan.files {
                    file_reports.push(FileReport {
                        path: f.path.clone(),
                        status: FileRestorationStatus::Restored,
                    });
                }
            }
            Err(rec_err) => {
                for f in &plan.files {
                    file_reports.push(FileReport {
                        path: f.path.clone(),
                        status: FileRestorationStatus::NotRestorable {
                            reason: format!("recovery failed: {rec_err}"),
                        },
                    });
                }
            }
        }

        return Err(ApplyError::CommitFailed(CommitFailureReport {
            message: err.to_string(),
            files: file_reports,
        }));
    }

    // Step 5: Mark journal Committed
    transition_journal_state(&project_state.dir, &mut journal, JournalState::Committed)
        .map_err(|e| ApplyError::Io {
            path: None,
            message: e.to_string(),
        })?;
    hook(CommitCrashPoint::Committed)?;

    Ok(journal)
}

/// Restores a single target file back to its original bytes using atomic temp write + rename.
fn restore_file_to_original(
    project_state: &ProjectState,
    entry: &JournalFileEntry,
) -> Result<(), ApplyError> {
    let target_path = project_state.canonical_root.join(&entry.path);
    let target_dir = target_path.parent().ok_or_else(|| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: "target has no parent directory".to_string(),
    })?;

    let meta = fs::metadata(&target_path).map_err(|e| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: e.to_string(),
    })?;

    let temp_path = target_dir.join(format!(
        ".restore.{}.{}.tmp",
        entry.path.replace('/', "_"),
        std::process::id()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        options.mode(meta.permissions().mode() & 0o777);
    }

    let mut temp_file = options.open(&temp_path).map_err(|e| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: e.to_string(),
    })?;
    use std::io::Write;
    temp_file
        .write_all(&entry.original_bytes)
        .map_err(|e| ApplyError::Io {
            path: Some(entry.path.clone()),
            message: e.to_string(),
        })?;
    temp_file.sync_all().map_err(|e| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: e.to_string(),
    })?;
    drop(temp_file);

    let current_bytes = fs::read(&target_path).map_err(|e| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: e.to_string(),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&current_bytes);
    let current_digest = format!("{:x}", hasher.finalize());

    if current_digest != entry.planned_digest {
        let _ = fs::remove_file(&temp_path);
        return Err(ApplyError::Stale {
            path: entry.path.clone(),
            expected_digest: entry.planned_digest.clone(),
            actual_digest: current_digest,
        });
    }

    fs::rename(&temp_path, &target_path).map_err(|e| ApplyError::Io {
        path: Some(entry.path.clone()),
        message: e.to_string(),
    })?;

    if let Ok(dir) = fs::File::open(target_dir) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Safely removes temporary commit files belonging to this journal transaction.
/// Only checks each target's parent directory and only removes files
/// whose names match `.surgery.<journal.id>.<index>.*.tmp`.
fn cleanup_journal_temp_files(project_state: &ProjectState, journal: &Journal) {
    for (i, entry) in journal.files.iter().enumerate() {
        let target_path = project_state.canonical_root.join(&entry.path);
        let Some(target_dir) = target_path.parent() else {
            continue;
        };
        let Ok(read_dir) = fs::read_dir(target_dir) else {
            continue;
        };
        let prefix = format!(".surgery.{}.{}.", journal.id, i);
        for dir_entry in read_dir.flatten() {
            let file_name = dir_entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                let _ = fs::remove_file(dir_entry.path());
            }
        }
    }
}

/// Executes recovery on a journal per protocol 6.3.
pub(crate) fn recover_journal(
    project_state: &ProjectState,
    journal: &mut Journal,
) -> Result<RecoveryOutcome, ApplyError> {
    for f in &journal.files {
        if validate_relative_path(&f.path).is_err() {
            return Err(ApplyError::Io {
                path: Some(f.path.clone()),
                message: "journal contains invalid or tampered relative path".to_string(),
            });
        }
    }

    if journal.state == JournalState::Committed || journal.state == JournalState::RolledBack {
        cleanup_journal_temp_files(project_state, journal);
        return Ok(RecoveryOutcome::NothingToDo(journal.state));
    }

    let mut classifications = Vec::with_capacity(journal.files.len());
    for f in &journal.files {
        let target_path = project_state.canonical_root.join(&f.path);
        let actual_bytes = match fs::read(&target_path) {
            Ok(b) => b,
            Err(e) => {
                return Err(ApplyError::Io {
                    path: Some(f.path.clone()),
                    message: e.to_string(),
                });
            }
        };

        let mut hasher = Sha256::new();
        hasher.update(&actual_bytes);
        let actual_digest = format!("{:x}", hasher.finalize());

        if actual_digest == f.original_digest {
            classifications.push(FileClassification::Original);
        } else if actual_digest == f.planned_digest {
            classifications.push(FileClassification::Planned);
        } else {
            classifications.push(FileClassification::Other { actual_digest });
        }
    }

    let has_other = classifications
        .iter()
        .any(|c| matches!(c, FileClassification::Other { .. }));

    if has_other {
        let mut conflicts = Vec::new();
        for (entry, class) in journal.files.iter().zip(&classifications) {
            match class {
                FileClassification::Other { actual_digest } => {
                    conflicts.push(FileConflict {
                        path: entry.path.clone(),
                        expected_original: entry.original_digest.clone(),
                        expected_planned: entry.planned_digest.clone(),
                        found_digest: actual_digest.clone(),
                    });
                }
                FileClassification::Planned => {
                    restore_file_to_original(project_state, entry)?;
                }
                FileClassification::Original => {}
            }
        }
        return Ok(RecoveryOutcome::Conflict(conflicts));
    }

    let all_planned = classifications
        .iter()
        .all(|c| matches!(c, FileClassification::Planned));

    if all_planned {
        transition_journal_state(&project_state.dir, journal, JournalState::Committed).map_err(
            |e| ApplyError::Io {
                path: None,
                message: e.to_string(),
            },
        )?;
        cleanup_journal_temp_files(project_state, journal);
        return Ok(RecoveryOutcome::RolledForward);
    }

    let all_original = classifications
        .iter()
        .all(|c| matches!(c, FileClassification::Original));

    if all_original {
        transition_journal_state(&project_state.dir, journal, JournalState::RolledBack).map_err(
            |e| ApplyError::Io {
                path: None,
                message: e.to_string(),
            },
        )?;
        cleanup_journal_temp_files(project_state, journal);
        return Ok(RecoveryOutcome::RolledBack);
    }

    for (entry, class) in journal.files.iter().zip(&classifications) {
        if matches!(class, FileClassification::Planned) {
            restore_file_to_original(project_state, entry)?;
        }
    }

    transition_journal_state(&project_state.dir, journal, JournalState::RolledBack).map_err(
        |e| ApplyError::Io {
            path: None,
            message: e.to_string(),
        },
    )?;
    cleanup_journal_temp_files(project_state, journal);

    Ok(RecoveryOutcome::RolledBack)
}

/// Finds and recovers any unfinished transaction for `project_state`.
pub(crate) fn recover_project(
    project_state: &ProjectState,
) -> Result<Option<RecoveryOutcome>, ApplyError> {
    if let Some(mut journal) = find_unfinished_journal(&project_state.dir).map_err(|e| {
        ApplyError::Io {
            path: None,
            message: e.to_string(),
        }
    })? {
        let outcome = recover_journal(project_state, &mut journal)?;
        Ok(Some(outcome))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surgery::journal::{
        init_project_state, read_journal, transition_journal_state, write_journal_atomic,
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
                operations: Vec::new(),
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

    #[test]
    fn test_commit_plan_happy_path_single_file() {
        let base = std::env::temp_dir().join(format!("cvb-commit-single-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let target_file = project_dir.join("src").join("main.rs");
        fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        let orig = "fn main() {\n    println!(\"hello\");\n}\n";
        let prop = "fn main() {\n    println!(\"world\");\n}\n";
        fs::write(&target_file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("single-commit", vec![("src/main.rs", orig, prop)]);

        let journal = commit_plan(&state, &plan).unwrap();
        assert_eq!(journal.state, JournalState::Committed);

        let on_disk = fs::read_to_string(&target_file).unwrap();
        assert_eq!(on_disk, prop);

        let j_path = state.dir.join("single-commit.journal");
        let read_j = read_journal(&j_path).unwrap();
        assert_eq!(read_j.state, JournalState::Committed);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_commit_plan_happy_path_multi_file() {
        let base = std::env::temp_dir().join(format!("cvb-commit-multi-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("src").join("a.rs");
        let file2 = project_dir.join("b.txt");
        fs::create_dir_all(file1.parent().unwrap()).unwrap();

        fs::write(&file1, "orig a\n").unwrap();
        fs::write(&file2, "orig b\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "multi-commit",
            vec![
                ("src/a.rs", "orig a\n", "prop a\n"),
                ("b.txt", "orig b\n", "prop b\n"),
            ],
        );

        let journal = commit_plan(&state, &plan).unwrap();
        assert_eq!(journal.state, JournalState::Committed);

        assert_eq!(fs::read_to_string(&file1).unwrap(), "prop a\n");
        assert_eq!(fs::read_to_string(&file2).unwrap(), "prop b\n");

        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn test_commit_plan_preserves_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("cvb-commit-perms-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let script = project_dir.join("run.sh");
        fs::write(&script, "#!/bin/sh\necho hello\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "script-commit",
            vec![("run.sh", "#!/bin/sh\necho hello\n", "#!/bin/sh\necho world\n")],
        );

        commit_plan(&state, &plan).unwrap();

        let meta = fs::metadata(&script).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o755,
            "target file must preserve executable 0755 permissions"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_commit_plan_rechecks_digest_before_rename() {
        let base = std::env::temp_dir().join(format!("cvb-commit-recheck-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("file.txt");
        fs::write(&file, "orig content\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("recheck-commit", vec![("file.txt", "orig content\n", "new\n")]);

        let target_clone = file.clone();
        let res = commit_plan_with_hook(&state, &plan, move |point| {
            if matches!(point, CommitCrashPoint::TempWrite(0)) {
                fs::write(&target_clone, "interfering writer content\n").unwrap();
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(report.message.contains("E-STALE"));
            }
            other => panic!("expected ApplyError::CommitFailed with Stale, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_commit_plan_rechecks_all_targets_before_first_rename() {
        let base = std::env::temp_dir().join(format!(
            "cvb-commit-all-recheck-{}",
            std::process::id()
        ));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file_a = project_dir.join("a.rs");
        let file_b = project_dir.join("b.rs");
        let orig_a = "orig a\n";
        let orig_b = "orig b\n";
        let foreign_b = "foreign edit to b\n";
        fs::write(&file_a, orig_a).unwrap();
        fs::write(&file_b, orig_b).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "all-target-recheck",
            vec![
                ("a.rs", orig_a, "new a\n"),
                ("b.rs", orig_b, "new b\n"),
            ],
        );

        let target_b = file_b.clone();
        let res = commit_plan_with_hook(&state, &plan, move |point| {
            if point == CommitCrashPoint::TempWrite(1) {
                fs::write(&target_b, foreign_b).unwrap();
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(
                    report.message.contains("E-STALE"),
                    "expected stale-target failure, got: {}",
                    report.message
                );
            }
            other => panic!(
                "expected CommitFailed with stale-target error, got: {other:?}"
            ),
        }

        assert_eq!(
            fs::read_to_string(&file_a).unwrap(),
            orig_a,
            "first target must not be renamed before all-target verification"
        );
        assert_eq!(
            fs::read_to_string(&file_b).unwrap(),
            foreign_b,
            "foreign content must remain untouched"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_commit_plan_records_journal_with_original_bytes() {
        let base = std::env::temp_dir().join(format!("cvb-commit-journal-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "original code\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("journal-check", vec![("main.rs", orig, "new code\n")]);

        let committed = commit_plan(&state, &plan).unwrap();
        assert_eq!(committed.files.len(), 1);
        assert_eq!(committed.files[0].original_bytes, orig.as_bytes());

        let j_path = state.dir.join("journal-check.journal");
        let read_j = read_journal(&j_path).unwrap();
        assert_eq!(read_j.files[0].original_bytes, orig.as_bytes());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_journal_prepared() {
        let base = std::env::temp_dir().join(format!("cvb-crash-prep-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "orig\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("crash-prep", vec![("main.rs", orig, "new\n")]);

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::JournalPrepared {
                return Err(ApplyError::FaultInjected("crash after prepared".to_string()));
            }
            Ok(())
        });

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledBack));
        assert_eq!(fs::read_to_string(&file).unwrap(), orig);

        let j = read_journal(&fresh_state.dir.join("crash-prep.journal")).unwrap();
        assert_eq!(j.state, JournalState::RolledBack);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_temp_write() {
        let base = std::env::temp_dir().join(format!("cvb-crash-temp-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "orig\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("crash-temp", vec![("main.rs", orig, "new\n")]);

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::TempWrite(0) {
                return Err(ApplyError::FaultInjected("crash after temp write".to_string()));
            }
            Ok(())
        });

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledBack));
        assert_eq!(fs::read_to_string(&file).unwrap(), orig);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_partial_rename_mix() {
        let base = std::env::temp_dir().join(format!("cvb-crash-partial-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        let orig1 = "orig a\n";
        let orig2 = "orig b\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "crash-partial",
            vec![("a.rs", orig1, "new a\n"), ("b.rs", orig2, "new b\n")],
        );

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::Rename(0) {
                return Err(ApplyError::FaultInjected("crash after first rename".to_string()));
            }
            Ok(())
        });

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledBack));
        assert_eq!(
            fs::read_to_string(&file1).unwrap(),
            orig1,
            "modified file must be restored to original"
        );
        assert_eq!(fs::read_to_string(&file2).unwrap(), orig2);

        let j = read_journal(&fresh_state.dir.join("crash-partial.journal")).unwrap();
        assert_eq!(j.state, JournalState::RolledBack);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_all_renames_rolls_forward() {
        let base = std::env::temp_dir().join(format!("cvb-crash-renamed-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        let orig1 = "orig a\n";
        let orig2 = "orig b\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "crash-all-renames",
            vec![("a.rs", orig1, "new a\n"), ("b.rs", orig2, "new b\n")],
        );

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::Rename(1) {
                return Err(ApplyError::FaultInjected("crash after all renames".to_string()));
            }
            Ok(())
        });

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledForward));
        assert_eq!(fs::read_to_string(&file1).unwrap(), "new a\n");
        assert_eq!(fs::read_to_string(&file2).unwrap(), "new b\n");

        let j = read_journal(&fresh_state.dir.join("crash-all-renames.journal")).unwrap();
        assert_eq!(j.state, JournalState::Committed);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_read_back_rolls_forward() {
        let base = std::env::temp_dir().join(format!("cvb-crash-readback-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        fs::write(&file, "orig\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("crash-readback", vec![("main.rs", "orig\n", "new\n")]);

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::ReadBack {
                return Err(ApplyError::FaultInjected("crash after read back".to_string()));
            }
            Ok(())
        });

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledForward));
        assert_eq!(fs::read_to_string(&file).unwrap(), "new\n");

        let j = read_journal(&fresh_state.dir.join("crash-readback.journal")).unwrap();
        assert_eq!(j.state, JournalState::Committed);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_second_temp_write_rolls_back() {
        let base = std::env::temp_dir().join(format!("cvb-crash-temp2-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        let orig1 = "orig a\n";
        let orig2 = "orig b\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "crash-temp2",
            vec![("a.rs", orig1, "new a\n"), ("b.rs", orig2, "new b\n")],
        );

        let res = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::TempWrite(1) {
                return Err(ApplyError::FaultInjected("crash after second temp write".to_string()));
            }
            Ok(())
        });
        assert!(matches!(res, Err(ApplyError::FaultInjected(_))));

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        assert_eq!(outcome, Some(RecoveryOutcome::RolledBack));
        assert_eq!(fs::read_to_string(&file1).unwrap(), orig1);
        assert_eq!(fs::read_to_string(&file2).unwrap(), orig2);

        let j = read_journal(&fresh_state.dir.join("crash-temp2.journal")).unwrap();
        assert_eq!(j.state, JournalState::RolledBack);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_after_committed_transition_preserves_committed() {
        let base = std::env::temp_dir().join(format!("cvb-crash-committed-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "orig\n";
        let planned = "planned\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("crash-committed", vec![("main.rs", orig, planned)]);

        let res = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::Committed {
                return Err(ApplyError::FaultInjected(
                    "crash immediately after committed transition".to_string(),
                ));
            }
            Ok(())
        });
        assert!(matches!(res, Err(ApplyError::FaultInjected(_))));

        // General project recovery should find no unfinished journals
        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();
        assert_eq!(outcome, None);

        // Direct recovery on the committed journal is a no-op
        let mut journal = read_journal(&fresh_state.dir.join("crash-committed.journal")).unwrap();
        assert_eq!(journal.state, JournalState::Committed);
        let direct_outcome = recover_journal(&fresh_state, &mut journal).unwrap();
        assert_eq!(
            direct_outcome,
            RecoveryOutcome::NothingToDo(JournalState::Committed)
        );

        // Planned target bytes are preserved
        assert_eq!(fs::read_to_string(&file).unwrap(), planned);

        // Replay protection remains active
        let replay_res = apply_plan(&fresh_state, &plan);
        assert!(matches!(replay_res, Err(ApplyError::Replay { .. })));

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_cleans_up_stale_temp_files() {
        let base = std::env::temp_dir().join(format!("cvb-stale-temp-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        let orig1 = "orig a\n";
        let orig2 = "orig b\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        // Place an unrelated temp file belonging to a different transaction
        let other_temp = project_dir.join(".surgery.other-tx.0.99999.tmp");
        fs::write(&other_temp, "unrelated temp content\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "stale-temp-tx",
            vec![("a.rs", orig1, "new a\n"), ("b.rs", orig2, "new b\n")],
        );

        // Crash after both temp files are written
        let res = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::TempWrite(1) {
                return Err(ApplyError::FaultInjected("crash after temp writes".to_string()));
            }
            Ok(())
        });
        assert!(matches!(res, Err(ApplyError::FaultInjected(_))));

        // Confirm the transaction's temp files exist prior to recovery
        let temp_files_before: Vec<_> = fs::read_dir(&project_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".surgery.stale-temp-tx."))
            .collect();
        assert!(
            !temp_files_before.is_empty(),
            "expected temp files to exist before recovery"
        );

        // Run recovery on a fresh state
        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();
        assert_eq!(outcome, Some(RecoveryOutcome::RolledBack));

        // The transaction's temp files must be removed
        let temp_files_after: Vec<_> = fs::read_dir(&project_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".surgery.stale-temp-tx."))
            .collect();
        assert!(
            temp_files_after.is_empty(),
            "expected transaction temp files to be cleaned up after recovery, found: {temp_files_after:?}"
        );

        // The unrelated transaction's temp file must be preserved
        assert!(
            other_temp.exists(),
            "unrelated temp file was unexpectedly deleted"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_conflict_leaves_unknown_content_untouched() {
        let base = std::env::temp_dir().join(format!("cvb-crash-conflict-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        let orig1 = "orig a\n";
        let orig2 = "orig b\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "crash-conflict",
            vec![("a.rs", "orig a\n", "new a\n"), ("b.rs", "orig b\n", "new b\n")],
        );

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::Rename(0) {
                return Err(ApplyError::FaultInjected("crash after first rename".to_string()));
            }
            Ok(())
        });

        let alien_content = "unknown foreign edits by user/editor\n";
        fs::write(&file2, alien_content).unwrap();

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let outcome = recover_project(&fresh_state).unwrap();

        match outcome {
            Some(RecoveryOutcome::Conflict(conflicts)) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].path, "b.rs");
            }
            other => panic!("expected Conflict outcome, got: {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file2).unwrap(),
            alien_content,
            "unknown content must NEVER be overwritten on recovery"
        );

        assert_eq!(fs::read_to_string(&file1).unwrap(), "orig a\n");

        let j = read_journal(&fresh_state.dir.join("crash-conflict.journal")).unwrap();
        assert_eq!(j.state, JournalState::Prepared);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_recovery_already_committed_or_rolled_back_is_noop() {
        let base = std::env::temp_dir().join(format!("cvb-crash-noop-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let mut journal = Journal {
            state: JournalState::Committed,
            id: "noop-j".to_string(),
            canonical_root: state.canonical_root.clone(),
            dev: state.dev,
            ino: state.ino,
            files: vec![],
        };

        let outcome = recover_journal(&state, &mut journal).unwrap();
        assert_eq!(
            outcome,
            RecoveryOutcome::NothingToDo(JournalState::Committed)
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_crash_recovery_conflict_propagates_restore_failure() {
        let base = std::env::temp_dir().join(format!(
            "cvb-crash-conflict-restore-fail-{}",
            std::process::id()
        ));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.rs");
        let file2 = project_dir.join("b.rs");
        fs::write(&file1, "orig a\n").unwrap();
        fs::write(&file2, "orig b\n").unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "crash-conflict-restore-fail",
            vec![("a.rs", "orig a\n", "new a\n"), ("b.rs", "orig b\n", "new b\n")],
        );

        let _ = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::Rename(0) {
                return Err(ApplyError::FaultInjected("crash after first rename".to_string()));
            }
            Ok(())
        });

        let alien_content = "unknown foreign edits by user/editor\n";
        fs::write(&file2, alien_content).unwrap();

        // Force restoration of a.rs to fail when opening its temporary file.
        let restore_temp = project_dir.join(format!(
            ".restore.a.rs.{}.tmp",
            std::process::id()
        ));
        fs::create_dir(&restore_temp).unwrap();

        let fresh_state = init_project_state(&state_dir, &project_dir).unwrap();
        let recovery = recover_project(&fresh_state);

        assert!(
            matches!(recovery, Err(ApplyError::Io { .. })),
            "expected restore I/O failure to propagate, got: {recovery:?}"
        );

        assert_eq!(fs::read_to_string(&file1).unwrap(), "new a\n");
        assert_eq!(fs::read_to_string(&file2).unwrap(), alien_content);

        let journal = read_journal(
            &fresh_state.dir.join("crash-conflict-restore-fail.journal")
        )
        .unwrap();
        assert_eq!(journal.state, JournalState::Prepared);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_concurrency_simultaneous_runs_lock_or_replay() {
        let base =
            std::env::temp_dir().join(format!("cvb-concurrency-lock-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "fn main() {}\n";
        let prop = "fn main() { println!(\"applied\"); }\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan("conc-tx-01", vec![("main.rs", orig, prop)]);

        let lock = ProjectLock::acquire(&state.dir).unwrap();

        let res = apply_plan(&state, &plan);
        match res {
            Err(ApplyError::Lock(LockError::Locked)) => {}
            other => panic!("expected ApplyError::Lock(Locked), got: {other:?}"),
        }

        drop(lock);

        let res_first = apply_plan(&state, &plan);
        assert!(res_first.is_ok());

        let res_second = apply_plan(&state, &plan);
        match res_second {
            Err(ApplyError::Replay { id }) => {
                assert_eq!(id, "conc-tx-01");
            }
            other => panic!("expected ApplyError::Replay, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_ordinary_rename_failure_rolls_back_earlier_renames() {
        let base = std::env::temp_dir().join(format!(
            "cvb-ordinary-rename-fail-{}",
            std::process::id()
        ));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file_a = project_dir.join("a.rs");
        let file_b = project_dir.join("b.rs");
        let orig_a = "orig a\n";
        let orig_b = "orig b\n";
        fs::write(&file_a, orig_a).unwrap();
        fs::write(&file_b, orig_b).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "ordinary-rename-fail",
            vec![
                ("a.rs", orig_a, "new a\n"),
                ("b.rs", orig_b, "new b\n"),
            ],
        );

        let res = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::BeforeRename(1) {
                return Err(ApplyError::Io {
                    path: Some("b.rs".to_string()),
                    message: "injected ordinary rename failure".to_string(),
                });
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(
                    report.message.contains("injected ordinary rename failure"),
                    "expected injected rename failure, got: {}",
                    report.message
                );
                assert_eq!(report.files.len(), 2);
                assert_eq!(report.files[0].path, "a.rs");
                assert_eq!(report.files[0].status, FileRestorationStatus::Restored);
                assert_eq!(report.files[1].path, "b.rs");
                assert_eq!(report.files[1].status, FileRestorationStatus::Restored);
            }
            other => panic!("expected CommitFailed after injected rename failure, got: {other:?}"),
        }

        assert_eq!(fs::read_to_string(&file_a).unwrap(), orig_a);
        assert_eq!(fs::read_to_string(&file_b).unwrap(), orig_b);

        let journal = crate::surgery::journal::read_journal(
            &state.dir.join("ordinary-rename-fail.journal"),
        )
        .unwrap();
        assert_eq!(journal.state, JournalState::RolledBack);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_ordinary_rename_failure_reports_restore_failure() {
        let base = std::env::temp_dir().join(format!(
            "cvb-ordinary-restore-fail-{}",
            std::process::id()
        ));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file_a = project_dir.join("a.rs");
        let file_b = project_dir.join("b.rs");
        let orig_a = "orig a\n";
        let orig_b = "orig b\n";
        fs::write(&file_a, orig_a).unwrap();
        fs::write(&file_b, orig_b).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "ordinary-restore-fail",
            vec![
                ("a.rs", orig_a, "new a\n"),
                ("b.rs", orig_b, "new b\n"),
            ],
        );

        let res = commit_plan_with_hook(&state, &plan, |point| {
            if point == CommitCrashPoint::BeforeRename(1) {
                fs::create_dir(project_dir.join(format!(
                    ".restore.a.rs.{}.tmp",
                    std::process::id()
                )))
                .unwrap();
                return Err(ApplyError::Io {
                    path: Some("b.rs".to_string()),
                    message: "injected ordinary rename failure".to_string(),
                });
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(
                    report.message.contains("injected ordinary rename failure"),
                    "expected injected rename failure, got: {}",
                    report.message
                );
                assert_eq!(report.files.len(), 2);
                for file in &report.files {
                    assert!(
                        matches!(
                            &file.status,
                            FileRestorationStatus::NotRestorable { reason }
                                if reason.contains("recovery failed")
                        ),
                        "expected restoration failure for {}, got: {:?}",
                        file.path,
                        file.status
                    );
                }
            }
            other => panic!(
                "expected CommitFailed after restoration failure, got: {other:?}"
            ),
        }

        assert_eq!(fs::read_to_string(&file_a).unwrap(), "new a\n");
        assert_eq!(fs::read_to_string(&file_b).unwrap(), orig_b);

        let journal = crate::surgery::journal::read_journal(
            &state.dir.join("ordinary-restore-fail.journal"),
        )
        .unwrap();
        assert_eq!(journal.state, JournalState::Prepared);
        assert_eq!(journal.files.len(), 2);
        assert_eq!(journal.files[0].original_bytes, orig_a.as_bytes());
        assert_eq!(journal.files[1].original_bytes, orig_b.as_bytes());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_concurrency_file_modified_before_rename_rolls_back_earlier_renames() {
    #[test]
    fn test_concurrency_file_modified_before_rename_rolls_back_earlier_renames() {
        let base = std::env::temp_dir().join(format!(
            "cvb-concurrency-rollback-{}",
            std::process::id()
        ));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file1 = project_dir.join("a.txt");
        let file2 = project_dir.join("b.txt");
        let orig1 = "first original\n";
        let orig2 = "second original\n";
        fs::write(&file1, orig1).unwrap();
        fs::write(&file2, orig2).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "rollback-per-rename",
            vec![("a.txt", orig1, "first new\n"), ("b.txt", orig2, "second new\n")],
        );

        let f2_clone = file2.clone();
        let res = apply_plan_with_hook(&state, &plan, move |point| {
            if point == CommitCrashPoint::Rename(0) {
                fs::write(&f2_clone, "interfering modified content\n").unwrap();
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(report.message.contains("E-STALE"));
                assert_eq!(report.files.len(), 2);
                assert_eq!(report.files[0].path, "a.txt");
                assert_eq!(report.files[0].status, FileRestorationStatus::Restored);
                assert_eq!(report.files[1].path, "b.txt");
                assert!(matches!(
                    report.files[1].status,
                    FileRestorationStatus::NotRestorable { .. }
                ));
            }
            other => panic!("expected ApplyError::CommitFailed, got: {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file1).unwrap(),
            orig1,
            "earlier renamed file must be rolled back to original"
        );

        assert_eq!(
            fs::read_to_string(&file2).unwrap(),
            "interfering modified content\n",
            "modified file must never be overwritten"
        );

        let j = read_journal(&state.dir.join("rollback-per-rename.journal")).unwrap();
        assert_eq!(j.state, JournalState::Prepared);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_unknown_content_modified_before_temp_write_never_overwritten() {
        let base =
            std::env::temp_dir().join(format!("cvb-unknown-before-temp-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let modified = "user modified content before surgery\n";
        fs::write(&file, modified).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "stale-before-temp",
            vec![("main.rs", "planned old\n", "planned new\n")],
        );

        let res = apply_plan(&state, &plan);
        assert!(matches!(res, Err(ApplyError::Stale { .. })));

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            modified,
            "modified content before temp write must never be overwritten"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_concurrency_true_simultaneous_runs_race_lock_or_replay() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let base =
            std::env::temp_dir().join(format!("cvb-concurrency-race-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "fn main() {}\n";
        let prop = "fn main() { println!(\"applied\"); }\n";
        fs::write(&file, orig).unwrap();

        let state = Arc::new(init_project_state(&state_dir, &project_dir).unwrap());
        let plan = Arc::new(make_test_plan("race-tx-01", vec![("main.rs", orig, prop)]));

        let barrier = Arc::new(Barrier::new(2));

        let state1 = Arc::clone(&state);
        let plan1 = Arc::clone(&plan);
        let barrier1 = Arc::clone(&barrier);
        let h1 = thread::spawn(move || {
            barrier1.wait();
            apply_plan(&state1, &plan1)
        });

        let state2 = Arc::clone(&state);
        let plan2 = Arc::clone(&plan);
        let barrier2 = Arc::clone(&barrier);
        let h2 = thread::spawn(move || {
            barrier2.wait();
            apply_plan(&state2, &plan2)
        });

        let res1 = h1.join().expect("thread 1 panicked");
        let res2 = h2.join().expect("thread 2 panicked");

        let outcomes = [res1, res2];
        let successes = outcomes.iter().filter(|r| r.is_ok()).count();
        let failures: Vec<_> = outcomes.into_iter().filter_map(|r| r.err()).collect();

        assert_eq!(successes, 1, "exactly one concurrent apply must succeed");
        assert_eq!(failures.len(), 1, "competing apply must fail");

        match &failures[0] {
            ApplyError::Lock(LockError::Locked) => {}
            ApplyError::Replay { id } => {
                assert_eq!(id, "race-tx-01");
            }
            other => panic!("expected ApplyError::Lock(Locked) or Replay, got: {other:?}"),
        }

        assert_eq!(fs::read_to_string(&file).unwrap(), prop);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_unknown_content_modified_before_rename_never_overwritten() {
        let base =
            std::env::temp_dir().join(format!("cvb-unknown-before-rename-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "original\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "stale-before-rename",
            vec![("main.rs", orig, "planned new\n")],
        );

        let file_clone = file.clone();
        let foreign = "foreign edit right before rename\n";
        let res = apply_plan_with_hook(&state, &plan, move |point| {
            if point == CommitCrashPoint::TempWrite(0) {
                fs::write(&file_clone, foreign).unwrap();
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(report.message.contains("E-STALE"));
            }
            other => panic!("expected ApplyError::CommitFailed, got: {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            foreign,
            "foreign content before rename must never be overwritten"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_unknown_content_modified_after_rename_never_overwritten() {
        let base =
            std::env::temp_dir().join(format!("cvb-unknown-after-rename-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let file = project_dir.join("main.rs");
        let orig = "original\n";
        fs::write(&file, orig).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let plan = make_test_plan(
            "stale-after-rename",
            vec![("main.rs", orig, "planned new\n")],
        );

        let file_clone = file.clone();
        let foreign = "foreign edit right after rename\n";
        let res = apply_plan_with_hook(&state, &plan, move |point| {
            if point == CommitCrashPoint::Rename(0) {
                fs::write(&file_clone, foreign).unwrap();
            }
            Ok(())
        });

        match res {
            Err(ApplyError::CommitFailed(report)) => {
                assert!(report.files[0].path == "main.rs");
                assert!(matches!(
                    report.files[0].status,
                    FileRestorationStatus::NotRestorable { .. }
                ));
            }
            other => panic!("expected ApplyError::CommitFailed, got: {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            foreign,
            "foreign content after rename must never be overwritten"
        );

        let outcome = recover_project(&state).unwrap();
        assert!(matches!(outcome, Some(RecoveryOutcome::Conflict(_))));

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            foreign,
            "recovery must preserve unknown content untouched"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_exit_status_codes() {
        assert_eq!(SurgeryExitStatus::Applied.code(), 0);
        assert_eq!(SurgeryExitStatus::RejectedBeforeCommit.code(), 2);
        assert_eq!(SurgeryExitStatus::UserDeclined.code(), 3);
        assert_eq!(SurgeryExitStatus::CommitFailure.code(), 4);
    }

    #[test]
    fn test_apply_error_exit_status_mapping() {
        let lock_err = ApplyError::Lock(LockError::Locked);
        assert_eq!(
            lock_err.exit_status(),
            SurgeryExitStatus::RejectedBeforeCommit
        );

        let recover_err = ApplyError::Recover {
            journal_path: PathBuf::from("/tmp/x"),
            id: "tx-1".to_string(),
        };
        assert_eq!(
            recover_err.exit_status(),
            SurgeryExitStatus::RejectedBeforeCommit
        );

        let replay_err = ApplyError::Replay {
            id: "tx-2".to_string(),
        };
        assert_eq!(
            replay_err.exit_status(),
            SurgeryExitStatus::RejectedBeforeCommit
        );

        let stale_err = ApplyError::Stale {
            path: "f.rs".to_string(),
            expected_digest: "a".to_string(),
            actual_digest: "b".to_string(),
        };
        assert_eq!(
            stale_err.exit_status(),
            SurgeryExitStatus::RejectedBeforeCommit
        );

        let fail_err = ApplyError::CommitFailed(CommitFailureReport {
            message: "I/O error".to_string(),
            files: vec![],
        });
        assert_eq!(fail_err.exit_status(), SurgeryExitStatus::CommitFailure);
    }

    #[test]
    fn test_commit_failure_report_all_restored() {
        let report = CommitFailureReport {
            message: "write failed on second target".to_string(),
            files: vec![
                FileReport {
                    path: "src/a.rs".to_string(),
                    status: FileRestorationStatus::Restored,
                },
                FileReport {
                    path: "src/b.rs".to_string(),
                    status: FileRestorationStatus::Restored,
                },
            ],
        };

        let formatted = report.to_string();
        assert!(formatted.contains("CVB: surgery failed during commit (E-IO): write failed on second target"));
        assert!(formatted.contains("src/a.rs: restored to original bytes"));
        assert!(formatted.contains("src/b.rs: restored to original bytes"));
    }

    #[test]
    fn test_commit_failure_report_conflict_not_restorable() {
        let report = CommitFailureReport {
            message: "disk content mismatch".to_string(),
            files: vec![
                FileReport {
                    path: "src/good.rs".to_string(),
                    status: FileRestorationStatus::Restored,
                },
                FileReport {
                    path: "src/alien.rs".to_string(),
                    status: FileRestorationStatus::NotRestorable {
                        reason: "conflict with unknown content (digest deadbeef, untouched)"
                            .to_string(),
                    },
                },
            ],
        };

        let formatted = report.to_string();
        assert!(formatted.contains("src/good.rs: restored to original bytes"));
        assert!(formatted.contains("src/alien.rs: not restorable: conflict with unknown content (digest deadbeef, untouched)"));
    }
}
