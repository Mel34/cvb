use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateError {
    NoHomeDir,
    Io(String),
    CorruptMeta(String),
    Canonicalize {
        path: String,
        message: String,
    },
    IdentityMismatch {
        key: String,
        recorded_root: String,
        current_root: String,
        recorded_dev: u64,
        current_dev: u64,
        recorded_ino: u64,
        current_ino: u64,
    },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::NoHomeDir => write!(
                f,
                "CVB: unable to resolve user state directory (neither XDG_STATE_HOME nor HOME is set)"
            ),
            StateError::Io(msg) => write!(f, "CVB: state directory I/O error: {msg}"),
            StateError::CorruptMeta(path) => {
                write!(f, "CVB: corrupt project metadata in '{path}'")
            }
            StateError::Canonicalize { path, message } => {
                write!(f, "CVB: unable to resolve project root '{path}': {message}")
            }
            StateError::IdentityMismatch {
                key,
                recorded_root,
                current_root,
                recorded_dev,
                current_dev,
                recorded_ino,
                current_ino,
            } => {
                write!(
                    f,
                    "CVB: project identity mismatch in state directory '{key}': recorded (root='{recorded_root}', dev={recorded_dev}, ino={recorded_ino}), current (root='{current_root}', dev={current_dev}, ino={current_ino})"
                )
            }
        }
    }
}

impl std::error::Error for StateError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectState {
    pub key: String,
    pub dir: PathBuf,
    pub canonical_root: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// Resolves the user state directory (named `surgery` for historical reasons;
/// existing journals live there):
/// $XDG_STATE_HOME/cvb/surgery/ falling back to $HOME/.local/state/cvb/surgery/
pub(crate) fn resolve_state_dir() -> Result<PathBuf, StateError> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("cvb").join("surgery"));
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("cvb")
            .join("surgery"));
    }
    Err(StateError::NoHomeDir)
}

/// Derives the SHA-256 project key from canonical root path, device, and inode.
pub(crate) fn compute_project_key(canonical_root: &Path, dev: u64, ino: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_root.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(dev.to_le_bytes());
    hasher.update(b":");
    hasher.update(ino.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

fn read_project_meta(meta_path: &Path) -> Result<(String, u64, u64), StateError> {
    let content = fs::read_to_string(meta_path).map_err(|e| StateError::Io(e.to_string()))?;
    let mut path = None;
    let mut dev = None;
    let mut ino = None;

    for line in content.lines() {
        if let Some(val) = line.strip_prefix("path=") {
            path = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("dev=") {
            dev = val.parse::<u64>().ok();
        } else if let Some(val) = line.strip_prefix("ino=") {
            ino = val.parse::<u64>().ok();
        }
    }

    match (path, dev, ino) {
        (Some(p), Some(d), Some(i)) => Ok((p, d, i)),
        _ => Err(StateError::CorruptMeta(meta_path.display().to_string())),
    }
}

fn write_project_meta(
    meta_path: &Path,
    canonical_root: &Path,
    dev: u64,
    ino: u64,
) -> Result<(), StateError> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let content = format!("path={}\ndev={dev}\nino={ino}\n", canonical_root.display());

    let parent = meta_path
        .parent()
        .ok_or_else(|| StateError::Io("metadata path has no parent".to_string()))?;
    let temp_path = parent.join(format!(".meta.tmp.{}", std::process::id()));

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(&temp_path)
        .map_err(|e| StateError::Io(e.to_string()))?;
    use std::io::Write;
    file.write_all(content.as_bytes())
        .map_err(|e| StateError::Io(e.to_string()))?;
    file.sync_all()
        .map_err(|e| StateError::Io(e.to_string()))?;
    drop(file);

    fs::rename(&temp_path, meta_path).map_err(|e| StateError::Io(e.to_string()))?;

    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Initializes or validates the project state directory under `state_base` for `project_root`.
/// Enforces directory mode 0700, file mode 0600, and refuses on root/dev/ino mismatch.
pub(crate) fn init_project_state(
    state_base: &Path,
    project_root: &Path,
) -> Result<ProjectState, StateError> {
    let canonical_root = fs::canonicalize(project_root).map_err(|e| StateError::Canonicalize {
        path: project_root.display().to_string(),
        message: e.to_string(),
    })?;

    let meta = fs::metadata(&canonical_root).map_err(|e| StateError::Io(e.to_string()))?;

    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0u64, 0u64);

    let key = compute_project_key(&canonical_root, dev, ino);
    let project_dir = state_base.join(&key);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&project_dir)
            .map_err(|e| StateError::Io(e.to_string()))?;

        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&project_dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&project_dir).map_err(|e| StateError::Io(e.to_string()))?;
    }

    let meta_path = project_dir.join("project.meta");
    let current_root_str = canonical_root.to_string_lossy().into_owned();

    if meta_path.exists() {
        let (recorded_root, recorded_dev, recorded_ino) = read_project_meta(&meta_path)?;
        if recorded_root != current_root_str || recorded_dev != dev || recorded_ino != ino {
            return Err(StateError::IdentityMismatch {
                key,
                recorded_root,
                current_root: current_root_str,
                recorded_dev,
                current_dev: dev,
                recorded_ino,
                current_ino: ino,
            });
        }
    } else {
        write_project_meta(&meta_path, &canonical_root, dev, ino)?;
    }

    Ok(ProjectState {
        key,
        dir: project_dir,
        canonical_root,
        dev,
        ino,
    })
}

use crate::commit::plan::validate_relative_path;

const JOURNAL_MAGIC: &[u8; 8] = b"CVBJRNL1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum JournalState {
    Prepared = 1,
    Committed = 2,
    RolledBack = 3,
}

impl TryFrom<u8> for JournalState {
    type Error = JournalError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(JournalState::Prepared),
            2 => Ok(JournalState::Committed),
            3 => Ok(JournalState::RolledBack),
            other => Err(JournalError::InvalidState(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalFileEntry {
    pub path: String,
    pub original_digest: String,
    pub planned_digest: String,
    pub original_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Journal {
    pub state: JournalState,
    pub id: String,
    pub canonical_root: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub files: Vec<JournalFileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalError {
    Io(String),
    Truncated(String),
    BadMagic,
    InvalidState(u8),
    InvalidUtf8(String),
    TamperedPath(String),
    IdentityMismatch {
        journal_root: String,
        current_root: String,
        journal_dev: u64,
        current_dev: u64,
        journal_ino: u64,
        current_ino: u64,
    },
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Io(msg) => write!(f, "CVB: journal I/O error: {msg}"),
            JournalError::Truncated(msg) => write!(f, "CVB: truncated journal: {msg}"),
            JournalError::BadMagic => write!(f, "CVB: corrupt journal header (bad magic bytes)"),
            JournalError::InvalidState(s) => write!(f, "CVB: invalid journal state code: {s}"),
            JournalError::InvalidUtf8(field) => {
                write!(f, "CVB: invalid UTF-8 in journal field '{field}'")
            }
            JournalError::TamperedPath(path) => {
                write!(f, "CVB: refused journal containing invalid or tampered relative path: '{path}'")
            }
            JournalError::IdentityMismatch {
                journal_root,
                current_root,
                journal_dev,
                current_dev,
                journal_ino,
                current_ino,
            } => {
                write!(
                    f,
                    "CVB: journal project identity mismatch: recorded (root='{journal_root}', dev={journal_dev}, ino={journal_ino}), current (root='{current_root}', dev={current_dev}, ino={current_ino})"
                )
            }
        }
    }
}

impl std::error::Error for JournalError {}

pub(crate) fn encode_journal(journal: &Journal) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(JOURNAL_MAGIC);
    buf.push(journal.state as u8);

    let id_bytes = journal.id.as_bytes();
    buf.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(id_bytes);

    let root_bytes = journal.canonical_root.to_string_lossy().as_bytes().to_vec();
    buf.extend_from_slice(&(root_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&root_bytes);

    buf.extend_from_slice(&journal.dev.to_le_bytes());
    buf.extend_from_slice(&journal.ino.to_le_bytes());

    buf.extend_from_slice(&(journal.files.len() as u32).to_le_bytes());
    for f in &journal.files {
        let p_bytes = f.path.as_bytes();
        buf.extend_from_slice(&(p_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(p_bytes);

        let orig_dig = f.original_digest.as_bytes();
        buf.extend_from_slice(&(orig_dig.len() as u32).to_le_bytes());
        buf.extend_from_slice(orig_dig);

        let plan_dig = f.planned_digest.as_bytes();
        buf.extend_from_slice(&(plan_dig.len() as u32).to_le_bytes());
        buf.extend_from_slice(plan_dig);

        buf.extend_from_slice(&(f.original_bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&f.original_bytes);
    }

    buf
}

pub(crate) fn decode_journal(data: &[u8]) -> Result<Journal, JournalError> {
    decode_journal_prefix(data).map(|(journal, _consumed)| journal)
}

/// Magic that introduces the optional extension block after the last file entry.
const EXT_MAGIC: &[u8; 8] = b"CVBXTRA1";

/// Extra per-file data a journal can carry for transactions that create or
/// delete files. Plain edits never write it, so their journals
/// stay byte-for-byte what they have always been.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct JournalExt {
    /// Empty (no block is written) or exactly one entry per journal file.
    pub entries: Vec<EntryExt>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EntryExt {
    /// Permission bits of a file that is about to be deleted, so that a
    /// rollback can recreate it with the same mode.
    pub mode: u32,
    /// Directories this transaction may create for an added file, shallowest
    /// first. A rollback removes them again, deepest first, if they are empty.
    pub created_dirs: Vec<String>,
}

impl JournalExt {
    /// The extension of file `index`, or the default when the journal has none.
    pub(crate) fn entry(&self, index: usize) -> EntryExt {
        self.entries.get(index).cloned().unwrap_or_default()
    }
}

/// Encodes `journal`, followed by the extension block when `ext` has entries.
pub(crate) fn encode_journal_with_ext(journal: &Journal, ext: &JournalExt) -> Vec<u8> {
    let mut buf = encode_journal(journal);
    if ext.entries.is_empty() {
        return buf;
    }

    buf.extend_from_slice(EXT_MAGIC);
    buf.extend_from_slice(&(ext.entries.len() as u32).to_le_bytes());
    for entry in &ext.entries {
        buf.extend_from_slice(&entry.mode.to_le_bytes());
        buf.extend_from_slice(&(entry.created_dirs.len() as u32).to_le_bytes());
        for dir in &entry.created_dirs {
            let bytes = dir.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
    }
    buf
}

/// Decodes a journal and its optional extension block. Bytes after the last
/// file entry that do not start with the extension magic are ignored, exactly
/// as `decode_journal` has always done.
pub(crate) fn decode_journal_with_ext(data: &[u8]) -> Result<(Journal, JournalExt), JournalError> {
    let (journal, consumed) = decode_journal_prefix(data)?;
    let rest = &data[consumed..];
    if !rest.starts_with(EXT_MAGIC) {
        return Ok((journal, JournalExt::default()));
    }

    let mut cursor = EXT_MAGIC.len();
    let take = |cursor: &mut usize, len: usize, desc: &'static str| -> Result<&[u8], JournalError> {
        if *cursor + len > rest.len() {
            return Err(JournalError::Truncated(desc.to_string()));
        }
        let slice = &rest[*cursor..*cursor + len];
        *cursor += len;
        Ok(slice)
    };
    let read_u32 = |cursor: &mut usize, desc: &'static str| -> Result<u32, JournalError> {
        Ok(u32::from_le_bytes(take(cursor, 4, desc)?.try_into().unwrap()))
    };

    let count = read_u32(&mut cursor, "extension entry count")? as usize;
    if count != journal.files.len() {
        return Err(JournalError::Truncated(
            "extension entry count does not match the file count".to_string(),
        ));
    }

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let mode = read_u32(&mut cursor, "extension mode")?;
        let dir_count = read_u32(&mut cursor, "extension directory count")? as usize;
        let mut created_dirs = Vec::new();
        for _ in 0..dir_count {
            let len = read_u32(&mut cursor, "extension directory length")? as usize;
            let bytes = take(&mut cursor, len, "extension directory")?;
            let dir = String::from_utf8(bytes.to_vec())
                .map_err(|_| JournalError::InvalidUtf8("created directory".to_string()))?;
            if validate_relative_path(&dir).is_err() {
                return Err(JournalError::TamperedPath(dir));
            }
            created_dirs.push(dir);
        }
        entries.push(EntryExt { mode, created_dirs });
    }

    Ok((journal, JournalExt { entries }))
}

/// Parses a journal and reports how many bytes it occupied.
fn decode_journal_prefix(data: &[u8]) -> Result<(Journal, usize), JournalError> {
    let mut cursor = 0;

    let take = |cursor: &mut usize, len: usize, desc: &'static str| -> Result<&[u8], JournalError> {
        if *cursor + len > data.len() {
            return Err(JournalError::Truncated(desc.to_string()));
        }
        let slice = &data[*cursor..*cursor + len];
        *cursor += len;
        Ok(slice)
    };

    let magic = take(&mut cursor, JOURNAL_MAGIC.len(), "magic bytes")?;
    if magic != JOURNAL_MAGIC {
        return Err(JournalError::BadMagic);
    }

    let state_byte = take(&mut cursor, 1, "state")?[0];
    let state = JournalState::try_from(state_byte)?;

    let id_len = u32::from_le_bytes(take(&mut cursor, 4, "id length")?.try_into().unwrap()) as usize;
    let id_bytes = take(&mut cursor, id_len, "id")?;
    let id = String::from_utf8(id_bytes.to_vec())
        .map_err(|_| JournalError::InvalidUtf8("id".to_string()))?;

    let root_len =
        u32::from_le_bytes(take(&mut cursor, 4, "root length")?.try_into().unwrap()) as usize;
    let root_bytes = take(&mut cursor, root_len, "root")?;
    let root_str = String::from_utf8(root_bytes.to_vec())
        .map_err(|_| JournalError::InvalidUtf8("canonical_root".to_string()))?;
    let canonical_root = PathBuf::from(root_str);

    let dev = u64::from_le_bytes(take(&mut cursor, 8, "dev")?.try_into().unwrap());
    let ino = u64::from_le_bytes(take(&mut cursor, 8, "ino")?.try_into().unwrap());

    let file_count =
        u32::from_le_bytes(take(&mut cursor, 4, "file count")?.try_into().unwrap()) as usize;
    let mut files = Vec::with_capacity(file_count);

    for _ in 0..file_count {
        let p_len =
            u32::from_le_bytes(take(&mut cursor, 4, "path length")?.try_into().unwrap()) as usize;
        let p_bytes = take(&mut cursor, p_len, "path")?;
        let path = String::from_utf8(p_bytes.to_vec())
            .map_err(|_| JournalError::InvalidUtf8("file path".to_string()))?;

        // Revalidate relative path against Section 4 rules on every read
        if validate_relative_path(&path).is_err() {
            return Err(JournalError::TamperedPath(path));
        }

        let orig_dig_len =
            u32::from_le_bytes(take(&mut cursor, 4, "orig digest length")?.try_into().unwrap())
                as usize;
        let orig_dig_bytes = take(&mut cursor, orig_dig_len, "orig digest")?;
        let original_digest = String::from_utf8(orig_dig_bytes.to_vec())
            .map_err(|_| JournalError::InvalidUtf8("original_digest".to_string()))?;

        let plan_dig_len =
            u32::from_le_bytes(take(&mut cursor, 4, "planned digest length")?.try_into().unwrap())
                as usize;
        let plan_dig_bytes = take(&mut cursor, plan_dig_len, "planned digest")?;
        let planned_digest = String::from_utf8(plan_dig_bytes.to_vec())
            .map_err(|_| JournalError::InvalidUtf8("planned_digest".to_string()))?;

        let orig_bytes_len =
            u64::from_le_bytes(take(&mut cursor, 8, "orig bytes length")?.try_into().unwrap())
                as usize;
        let original_bytes = take(&mut cursor, orig_bytes_len, "original bytes")?.to_vec();

        files.push(JournalFileEntry {
            path,
            original_digest,
            planned_digest,
            original_bytes,
        });
    }

    Ok((
        Journal {
            state,
            id,
            canonical_root,
            dev,
            ino,
            files,
        },
        cursor,
    ))
}

/// Atomically writes a journal into `project_dir` under `<id>.journal` with mode 0600.
pub(crate) fn write_journal_atomic(
    project_dir: &Path,
    journal: &Journal,
) -> Result<PathBuf, JournalError> {
    write_journal_with_ext_atomic(project_dir, journal, &JournalExt::default())
}

/// Like `write_journal_atomic`, and also records the extension block when `ext`
/// has entries.
pub(crate) fn write_journal_with_ext_atomic(
    project_dir: &Path,
    journal: &Journal,
    ext: &JournalExt,
) -> Result<PathBuf, JournalError> {
    #[cfg(unix)]
    let sync_directory = |path: &Path| fs::File::open(path)?.sync_all();
    #[cfg(not(unix))]
    let sync_directory = |_path: &Path| Ok(());

    write_journal_bytes_atomic_with_dir_sync(
        project_dir,
        &journal.id,
        encode_journal_with_ext(journal, ext),
        sync_directory,
    )
}

fn write_journal_atomic_with_dir_sync<F>(
    project_dir: &Path,
    journal: &Journal,
    sync_directory: F,
) -> Result<PathBuf, JournalError>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    write_journal_bytes_atomic_with_dir_sync(
        project_dir,
        &journal.id,
        encode_journal(journal),
        sync_directory,
    )
}

fn write_journal_bytes_atomic_with_dir_sync<F>(
    project_dir: &Path,
    id: &str,
    encoded: Vec<u8>,
    sync_directory: F,
) -> Result<PathBuf, JournalError>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let target_path = project_dir.join(format!("{id}.journal"));
    let temp_path = project_dir.join(format!(".journal.{id}.tmp.{}", std::process::id()));

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(&temp_path)
        .map_err(|e| JournalError::Io(e.to_string()))?;
    use std::io::Write;
    file.write_all(&encoded)
        .map_err(|e| JournalError::Io(e.to_string()))?;
    file.sync_all()
        .map_err(|e| JournalError::Io(e.to_string()))?;
    drop(file);

    fs::rename(&temp_path, &target_path).map_err(|e| JournalError::Io(e.to_string()))?;

    sync_directory(project_dir).map_err(|e| JournalError::Io(e.to_string()))?;

    Ok(target_path)
}

/// Atomically transitions the journal's state and writes it to disk.
pub(crate) fn transition_journal_state(
    project_dir: &Path,
    journal: &mut Journal,
    new_state: JournalState,
) -> Result<PathBuf, JournalError> {
    journal.state = new_state;
    write_journal_atomic(project_dir, journal)
}

/// Reads a journal from disk and verifies its integrity and path safety.
pub(crate) fn read_journal(path: &Path) -> Result<Journal, JournalError> {
    let bytes = fs::read(path).map_err(|e| JournalError::Io(e.to_string()))?;
    decode_journal(&bytes)
}

/// Reads the extension block of journal `id` in `project_dir`. A journal that
/// does not exist, or has no extension block, yields an empty extension.
pub(crate) fn read_journal_ext(project_dir: &Path, id: &str) -> Result<JournalExt, JournalError> {
    let path = project_dir.join(format!("{id}.journal"));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(JournalExt::default()),
        Err(e) => return Err(JournalError::Io(e.to_string())),
    };
    decode_journal_with_ext(&bytes).map(|(_journal, ext)| ext)
}

/// Lists all journals found in `project_dir`.
pub(crate) fn list_project_journals(project_dir: &Path) -> Result<Vec<Journal>, JournalError> {
    let mut journals = Vec::new();
    let entries = fs::read_dir(project_dir).map_err(|e| JournalError::Io(e.to_string()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "journal") {
            let journal = read_journal(&path)?;
            journals.push(journal);
        }
    }
    Ok(journals)
}

/// Returns true if `id` is currently applied (`Prepared` or `Committed`).
/// Rolled-back journals do not count as applied.
pub(crate) fn check_replay_id(project_dir: &Path, id: &str) -> Result<bool, JournalError> {
    let journals = list_project_journals(project_dir)?;
    for j in journals {
        if j.id == id && (j.state == JournalState::Prepared || j.state == JournalState::Committed) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Finds any unfinished (`Prepared`) transaction in `project_dir`.
pub(crate) fn find_unfinished_journal(project_dir: &Path) -> Result<Option<Journal>, JournalError> {
    let journals = list_project_journals(project_dir)?;
    for j in journals {
        if j.state == JournalState::Prepared {
            return Ok(Some(j));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_resolve_state_dir_xdg_env() {
        let temp = std::env::temp_dir().join(format!("cvb-state-env-{}", std::process::id()));
        unsafe {
            std::env::set_var("XDG_STATE_HOME", &temp);
        }

        let resolved = resolve_state_dir().unwrap();
        assert_eq!(resolved, temp.join("cvb").join("surgery"));

        unsafe {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    #[test]
    fn test_init_project_state_modes_and_meta() {
        let base = std::env::temp_dir().join(format!("cvb-init-state-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        assert!(state.dir.exists());

        #[cfg(unix)]
        {
            let dir_meta = fs::metadata(&state.dir).unwrap();
            assert_eq!(
                dir_meta.permissions().mode() & 0o777,
                0o700,
                "state directory must have mode 0700"
            );

            let meta_file = state.dir.join("project.meta");
            assert!(meta_file.exists());
            let file_meta = fs::metadata(&meta_file).unwrap();
            assert_eq!(
                file_meta.permissions().mode() & 0o777,
                0o600,
                "project.meta file must have mode 0600"
            );
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_init_project_state_reopens_identical_project() {
        let base = std::env::temp_dir().join(format!("cvb-reopen-state-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state1 = init_project_state(&state_dir, &project_dir).unwrap();
        let state2 = init_project_state(&state_dir, &project_dir).unwrap();

        assert_eq!(state1.key, state2.key);
        assert_eq!(state1.dir, state2.dir);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_init_project_state_refuses_mismatched_root_path() {
        let base = std::env::temp_dir().join(format!("cvb-mismatch-path-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let meta_file = state.dir.join("project.meta");

        // Tamper with recorded path
        let tampered = format!("path=/tampered/nonexistent\ndev={}\nino={}\n", state.dev, state.ino);
        fs::write(&meta_file, tampered).unwrap();

        match init_project_state(&state_dir, &project_dir) {
            Err(StateError::IdentityMismatch { recorded_root, .. }) => {
                assert_eq!(recorded_root, "/tampered/nonexistent");
            }
            other => panic!("expected IdentityMismatch, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_init_project_state_refuses_mismatched_dev_or_ino() {
        let base = std::env::temp_dir().join(format!("cvb-mismatch-ino-{}", std::process::id()));
        let state_dir = base.join("state");
        let project_dir = base.join("project");

        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&project_dir).unwrap();

        let state = init_project_state(&state_dir, &project_dir).unwrap();
        let meta_file = state.dir.join("project.meta");

        // Tamper with recorded inode
        let tampered = format!(
            "path={}\ndev={}\nino={}\n",
            state.canonical_root.display(),
            state.dev,
            state.ino + 9999
        );
        fs::write(&meta_file, tampered).unwrap();

        match init_project_state(&state_dir, &project_dir) {
            Err(StateError::IdentityMismatch { recorded_ino, current_ino, .. }) => {
                assert_eq!(recorded_ino, state.ino + 9999);
                assert_eq!(current_ino, state.ino);
            }
            other => panic!("expected IdentityMismatch on inode, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(base);
    }

#[test]
    fn test_journal_roundtrip_encode_decode() {
        let journal = Journal {
            state: JournalState::Prepared,
            id: "tx-test-01".to_string(),
            canonical_root: PathBuf::from("/canonical/path/to/project"),
            dev: 42,
            ino: 1001,
            files: vec![
                JournalFileEntry {
                    path: "src/main.rs".to_string(),
                    original_digest: "abcd1234abcd1234abcd1234abcd1234".to_string(),
                    planned_digest: "ef015678ef015678ef015678ef015678".to_string(),
                    original_bytes: b"fn main() {\n    println!(\"hello\");\n}\n".to_vec(),
                },
                JournalFileEntry {
                    path: "Cargo.toml".to_string(),
                    original_digest: "1111222233334444".to_string(),
                    planned_digest: "5555666677778888".to_string(),
                    original_bytes: b"[package]\nname = \"cvb\"\n".to_vec(),
                },
            ],
        };

        let encoded = encode_journal(&journal);
        let decoded = decode_journal(&encoded).unwrap();

        assert_eq!(decoded, journal);
    }

    /// Characterization test: pins the exact on-disk journal layout so that
    /// later extensions cannot change the bytes written for ordinary edits.
    /// The expected bytes are assembled by hand from the documented layout:
    /// magic, state, then length-prefixed id and root, dev, ino, the file
    /// count and, per file, length-prefixed path and two digests followed by
    /// a u64-prefixed original byte string. Nothing follows the last entry.
    #[test]
    fn test_journal_encoding_is_byte_stable() {
        let journal = Journal {
            state: JournalState::Prepared,
            id: "tx1".to_string(),
            canonical_root: PathBuf::from("/r"),
            dev: 7,
            ino: 9,
            files: vec![JournalFileEntry {
                path: "a.rs".to_string(),
                original_digest: "aa".to_string(),
                planned_digest: "bb".to_string(),
                original_bytes: b"old\n".to_vec(),
            }],
        };

        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"CVBJRNL1");
        expected.push(1);
        expected.extend_from_slice(&3u32.to_le_bytes());
        expected.extend_from_slice(b"tx1");
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(b"/r");
        expected.extend_from_slice(&7u64.to_le_bytes());
        expected.extend_from_slice(&9u64.to_le_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(b"a.rs");
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(b"aa");
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(b"bb");
        expected.extend_from_slice(&4u64.to_le_bytes());
        expected.extend_from_slice(b"old\n");

        assert_eq!(encode_journal(&journal), expected);
        assert_eq!(decode_journal(&expected).unwrap(), journal);
    }

    /// Characterization test: the decoder ignores bytes after the last entry.
    /// A later journal extension relies on this to append optional data that
    /// older readers skip.
    #[test]
    fn test_journal_decoder_ignores_trailing_bytes() {
        let journal = Journal {
            state: JournalState::Committed,
            id: "tx2".to_string(),
            canonical_root: PathBuf::from("/r"),
            dev: 1,
            ino: 2,
            files: vec![JournalFileEntry {
                path: "b.rs".to_string(),
                original_digest: "cc".to_string(),
                planned_digest: "dd".to_string(),
                original_bytes: b"x".to_vec(),
            }],
        };

        let mut encoded = encode_journal(&journal);
        encoded.extend_from_slice(b"\x01\x02optional trailing data");

        assert_eq!(decode_journal(&encoded).unwrap(), journal);
    }

    #[test]
    fn test_journal_directory_sync_failure_propagates_after_rename() {
        let base = std::env::temp_dir().join(format!(
            "cvb-journal-sync-fail-{}",
            std::process::id()
        ));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();

        let mut journal = Journal {
            state: JournalState::Prepared,
            id: "tx-sync-fail".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![],
        };

        let j_path = write_journal_atomic(&base, &journal).unwrap();
        journal.state = JournalState::Committed;
        let result = write_journal_atomic_with_dir_sync(&base, &journal, |_path| {
            Err(std::io::Error::other(
                "injected directory sync failure",
            ))
        });

        assert!(matches!(
            result,
            Err(JournalError::Io(ref message)) if message.contains("injected directory sync failure")
        ));
        assert_eq!(
            read_journal(&j_path).unwrap().state,
            JournalState::Committed
        );
        let _ = fs::remove_dir_all(base);
    }


    #[test]
    fn test_journal_atomic_state_transitions() {
        let base = std::env::temp_dir().join(format!("cvb-journal-trans-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();

        let mut journal = Journal {
            state: JournalState::Prepared,
            id: "tx-transition".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![JournalFileEntry {
                path: "src/lib.rs".to_string(),
                original_digest: "orig1".to_string(),
                planned_digest: "plan1".to_string(),
                original_bytes: b"original content\n".to_vec(),
            }],
        };

        let j_path = write_journal_atomic(&base, &journal).unwrap();
        assert!(j_path.exists());

        let read_prep = read_journal(&j_path).unwrap();
        assert_eq!(read_prep.state, JournalState::Prepared);

        // Transition to Committed
        transition_journal_state(&base, &mut journal, JournalState::Committed).unwrap();
        let read_comm = read_journal(&j_path).unwrap();
        assert_eq!(read_comm.state, JournalState::Committed);

        // Transition to RolledBack
        transition_journal_state(&base, &mut journal, JournalState::RolledBack).unwrap();
        let read_roll = read_journal(&j_path).unwrap();
        assert_eq!(read_roll.state, JournalState::RolledBack);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_journal_rejects_tampered_path_traversal() {
        let journal = Journal {
            state: JournalState::Prepared,
            id: "tx-tamper-traversal".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![JournalFileEntry {
                path: "src/safe.rs".to_string(),
                original_digest: "orig".to_string(),
                planned_digest: "plan".to_string(),
                original_bytes: b"safe\n".to_vec(),
            }],
        };

        let mut encoded = encode_journal(&journal);
        let safe_needle = b"src/safe.rs";
        let bad_path = b"../outside.rs";

        let pos = encoded
            .windows(safe_needle.len())
            .position(|w| w == safe_needle)
            .unwrap();
        encoded.splice(pos..pos + safe_needle.len(), bad_path.iter().cloned());

        // Adjust path length prefix immediately preceding the bytes
        let len_pos = pos - 4;
        let new_len = (bad_path.len() as u32).to_le_bytes();
        encoded[len_pos..len_pos + 4].copy_from_slice(&new_len);

        match decode_journal(&encoded) {
            Err(JournalError::TamperedPath(ref p)) => {
                assert_eq!(p, "../outside.rs");
            }
            other => panic!("expected TamperedPath error for traversal, got: {other:?}"),
        }
    }

    #[test]
    fn test_journal_rejects_tampered_path_git() {
        let mut journal = Journal {
            state: JournalState::Prepared,
            id: "tx-tamper-git".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![JournalFileEntry {
                path: "src/safe.rs".to_string(),
                original_digest: "orig".to_string(),
                planned_digest: "plan".to_string(),
                original_bytes: b"safe\n".to_vec(),
            }],
        };

        journal.files[0].path = ".git/hooks/pre-commit".to_string();
        let encoded = encode_journal(&journal);

        match decode_journal(&encoded) {
            Err(JournalError::TamperedPath(ref p)) => {
                assert!(p.contains(".git"));
            }
            other => panic!("expected TamperedPath error for .git path, got: {other:?}"),
        }
    }

    #[test]
    fn test_journal_replay_check() {
        let base = std::env::temp_dir().join(format!("cvb-journal-replay-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();

        let mut journal = Journal {
            state: JournalState::Prepared,
            id: "applied-id-01".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![],
        };

        write_journal_atomic(&base, &journal).unwrap();
        assert!(check_replay_id(&base, "applied-id-01").unwrap());
        assert!(!check_replay_id(&base, "non-existent-id").unwrap());

        // Committed still blocks
        transition_journal_state(&base, &mut journal, JournalState::Committed).unwrap();
        assert!(check_replay_id(&base, "applied-id-01").unwrap());

        // RolledBack allows reuse
        transition_journal_state(&base, &mut journal, JournalState::RolledBack).unwrap();
        assert!(!check_replay_id(&base, "applied-id-01").unwrap());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_journal_find_unfinished() {
        let base = std::env::temp_dir().join(format!("cvb-journal-unfinished-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();

        let mut journal = Journal {
            state: JournalState::Committed,
            id: "comm-id".to_string(),
            canonical_root: PathBuf::from("/canonical/project"),
            dev: 1,
            ino: 2,
            files: vec![],
        };
        write_journal_atomic(&base, &journal).unwrap();

        assert!(find_unfinished_journal(&base).unwrap().is_none());

        // Now add a prepared journal
        journal.id = "prep-id".to_string();
        journal.state = JournalState::Prepared;
        write_journal_atomic(&base, &journal).unwrap();

        let unfinished = find_unfinished_journal(&base).unwrap();
        assert!(unfinished.is_some());
        assert_eq!(unfinished.unwrap().id, "prep-id");

        let _ = fs::remove_dir_all(base);
    }

    fn sample_journal_with_two_files() -> Journal {
        let entry = |path: &str| JournalFileEntry {
            path: path.to_string(),
            original_digest: "aa".to_string(),
            planned_digest: "bb".to_string(),
            original_bytes: b"old\n".to_vec(),
        };
        Journal {
            state: JournalState::Prepared,
            id: "ext-test".to_string(),
            canonical_root: PathBuf::from("/r"),
            dev: 3,
            ino: 4,
            files: vec![entry("new/added.txt"), entry("gone.sh")],
        }
    }

    fn sample_ext() -> JournalExt {
        JournalExt {
            entries: vec![
                EntryExt {
                    mode: 0,
                    created_dirs: vec!["new".to_string(), "new/sub".to_string()],
                },
                EntryExt {
                    mode: 0o750,
                    created_dirs: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn test_journal_extension_roundtrips() {
        let journal = sample_journal_with_two_files();
        let ext = sample_ext();

        let encoded = encode_journal_with_ext(&journal, &ext);
        let (decoded, decoded_ext) = decode_journal_with_ext(&encoded).unwrap();

        assert_eq!(decoded, journal);
        assert_eq!(decoded_ext, ext);
        // The plain decoder still reads the same journal and ignores the block.
        assert_eq!(decode_journal(&encoded).unwrap(), journal);
    }

    #[test]
    fn test_empty_extension_leaves_the_journal_bytes_unchanged() {
        let journal = sample_journal_with_two_files();
        assert_eq!(
            encode_journal_with_ext(&journal, &JournalExt::default()),
            encode_journal(&journal)
        );
    }

    #[test]
    fn test_journal_without_extension_decodes_to_an_empty_one() {
        let journal = sample_journal_with_two_files();
        let (decoded, ext) = decode_journal_with_ext(&encode_journal(&journal)).unwrap();
        assert_eq!(decoded, journal);
        assert_eq!(ext, JournalExt::default());
        assert_eq!(ext.entry(0), EntryExt::default());
    }

    #[test]
    fn test_trailing_bytes_without_the_magic_are_not_an_extension() {
        let journal = sample_journal_with_two_files();
        let mut bytes = encode_journal(&journal);
        bytes.extend_from_slice(b"some other trailing data");
        let (decoded, ext) = decode_journal_with_ext(&bytes).unwrap();
        assert_eq!(decoded, journal);
        assert_eq!(ext, JournalExt::default());
    }

    #[test]
    fn test_bytes_after_the_extension_are_ignored() {
        let journal = sample_journal_with_two_files();
        let mut bytes = encode_journal_with_ext(&journal, &sample_ext());
        bytes.extend_from_slice(b"future data");
        let (_, ext) = decode_journal_with_ext(&bytes).unwrap();
        assert_eq!(ext, sample_ext());
    }

    /// Cutting the block anywhere after its magic must be an error, never a
    /// silently shorter extension.
    #[test]
    fn test_truncated_extension_is_an_error_at_every_cut() {
        let journal = sample_journal_with_two_files();
        let plain_len = encode_journal(&journal).len();
        let full = encode_journal_with_ext(&journal, &sample_ext());

        for cut in (plain_len + EXT_MAGIC.len())..full.len() {
            assert!(
                decode_journal_with_ext(&full[..cut]).is_err(),
                "a block cut at byte {cut} of {} decoded",
                full.len()
            );
        }
    }

    #[test]
    fn test_extension_with_the_wrong_entry_count_is_an_error() {
        let journal = sample_journal_with_two_files();
        let one_entry = JournalExt {
            entries: vec![EntryExt::default()],
        };
        let bytes = encode_journal_with_ext(&journal, &one_entry);
        assert!(matches!(
            decode_journal_with_ext(&bytes),
            Err(JournalError::Truncated(_))
        ));
    }

    #[test]
    fn test_extension_with_an_unsafe_directory_is_refused() {
        let journal = sample_journal_with_two_files();
        for bad in [".git/hooks", "../escape", "/abs", "a//b", ""] {
            let ext = JournalExt {
                entries: vec![
                    EntryExt {
                        mode: 0,
                        created_dirs: vec![bad.to_string()],
                    },
                    EntryExt::default(),
                ],
            };
            let bytes = encode_journal_with_ext(&journal, &ext);
            assert!(
                matches!(
                    decode_journal_with_ext(&bytes),
                    Err(JournalError::TamperedPath(_))
                ),
                "directory {bad:?} was accepted"
            );
        }
    }

    #[test]
    fn test_read_journal_ext_from_disk() {
        let base = std::env::temp_dir().join(format!("cvb-journal-ext-{}", std::process::id()));
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();

        // No journal at all: an empty extension, not an error.
        assert_eq!(read_journal_ext(&base, "missing").unwrap(), JournalExt::default());

        let journal = sample_journal_with_two_files();
        write_journal_with_ext_atomic(&base, &journal, &sample_ext()).unwrap();
        assert_eq!(read_journal_ext(&base, "ext-test").unwrap(), sample_ext());
        assert_eq!(read_journal(&base.join("ext-test.journal")).unwrap(), journal);

        // Rewriting without an extension (as a state transition does) drops it.
        write_journal_atomic(&base, &journal).unwrap();
        assert_eq!(read_journal_ext(&base, "ext-test").unwrap(), JournalExt::default());

        let _ = fs::remove_dir_all(base);
    }
}
