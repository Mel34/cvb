//! Applies a parsed patch all-or-nothing.
//!
//! The installed `apply_patch` is not atomic: given a patch whose second file does
//! not match, it writes the first file and then fails. So CVB never lets it touch
//! the real tree:
//!
//! 1. copy the files the patch names into a scratch directory,
//! 2. run `apply_patch` there, patch on stdin, scratch directory as its cwd,
//! 3. on failure report its output and stop: nothing real was touched,
//! 4. on success read the scratch results back and commit them as one surgery
//!    transaction (journal, project lock, digest re-check, rollback), which adds,
//!    updates and deletes files together or not at all.
//!
//! Output is rewritten so that no scratch path is ever shown: the LLM sees the
//! paths of the real tree.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::patch::guard::check_tree;
use crate::patch::parse::{Change, ParsedPatch, parse_patch};
use crate::surgery::apply::{ABSENT_DIGEST, ApplyError, apply_plan_with_modes, sha256_hex};
use crate::surgery::journal::{ProjectState, init_project_state};
use crate::surgery::load::{MAX_AGGREGATE_BYTES, MAX_TARGET_BYTES};
use crate::surgery::{Plan, PlannedFile, detect_newline};

/// How long `apply_patch` may run before it is killed.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Output longer than this is cut (and says so).
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Scratch directories older than this are leftovers of a crashed run.
const STALE_SCRATCH: Duration = Duration::from_secs(60 * 60);
const SCRATCH_PREFIX: &str = "patch-scratch-";

/// What the runner needs to know about its surroundings.
pub(crate) struct Env<'a> {
    /// The shell's current directory (the patch root).
    pub root: &'a Path,
    /// CVB's surgery state directory (journals and scratch live under it).
    pub state_base: &'a Path,
    /// The `apply_patch` program: a bare name is looked up in `PATH`.
    pub program: &'a OsStr,
    pub timeout: Duration,
}

/// The result of one pasted patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Report {
    /// Every change was committed.
    Applied { files: usize, output: String },
    /// `apply_patch` failed in the scratch copy; the real tree is untouched.
    Failed { exit: Option<i32>, output: String },
    /// CVB refused or could not finish; the reason says what happened.
    Rejected(String),
}

impl Report {
    /// The text for the screen and the clipboard (decision D6).
    pub(crate) fn to_text(&self) -> String {
        match self {
            Report::Applied { files, output } => {
                format!("CVB patch APPLIED files={files}\n{}", with_newline(output))
            }
            Report::Failed { exit, output } => {
                let status = match exit {
                    Some(code) => format!("apply_patch exit {code}"),
                    None => "apply_patch was terminated by a signal".to_string(),
                };
                format!(
                    "CVB patch FAILED ({status})\n{}No files were modified.\n",
                    with_newline(output)
                )
            }
            Report::Rejected(reason) => format!("CVB patch REJECTED: {reason}\n"),
        }
    }
}

fn with_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

fn rejected(reason: impl Into<String>) -> Report {
    Report::Rejected(reason.into())
}

/// Removes the scratch directory and the output capture file when dropped.
struct Scratch {
    dir: PathBuf,
    out: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        let _ = fs::remove_file(&self.out);
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("patch-{nanos}-{}-{count}", std::process::id())
}

/// Parses, checks and applies one pasted patch (`text` is a complete envelope).
pub(crate) fn run_patch(text: &str, env: &Env) -> Report {
    let parsed = match parse_patch(text) {
        Ok(parsed) => parsed,
        Err(error) => return rejected(error.to_string()),
    };
    let state = match init_project_state(env.state_base, env.root) {
        Ok(state) => state,
        Err(error) => return rejected(format!("cannot prepare CVB state: {error}")),
    };
    let state_base = fs::canonicalize(env.state_base).unwrap_or_else(|_| env.state_base.to_path_buf());
    if let Err(error) = check_tree(&parsed, &state.canonical_root, Some(&state_base)) {
        return rejected(error.to_string());
    }

    let originals = match read_originals(&parsed, &state.canonical_root) {
        Ok(originals) => originals,
        Err(reason) => return rejected(reason),
    };

    remove_stale_scratch(&state.dir);
    let id = new_id();
    let scratch = match make_scratch(&state.dir, &id) {
        Ok(scratch) => scratch,
        Err(reason) => return rejected(reason),
    };
    if let Err(reason) = fill_scratch(&scratch.dir, &originals, &parsed) {
        return rejected(reason);
    }

    let (status, raw_output) = match run_program(env, &scratch, text) {
        Ok(result) => result,
        Err(reason) => return rejected(reason),
    };
    let output = rewrite_paths(&raw_output, &scratch.dir, &state.canonical_root);

    if !status.success() {
        // Only an annotation: a patch pasted twice fails correctly, but reads as broken.
        let output = match crate::patch::already::note(text, &originals, &state.canonical_root) {
            Some(note) => format!("{}{note}", with_newline(&output)),
            None => output,
        };
        return Report::Failed { exit: status.code(), output };
    }

    let (plan, modes, files) = match build_plan(&parsed, &originals, &scratch.dir, &state, text, id) {
        Ok(result) => result,
        Err(reason) => return rejected(reason),
    };
    match apply_plan_with_modes(&state, &plan, &modes) {
        Ok(_) => Report::Applied { files, output },
        Err(ApplyError::Stale { path, .. }) => rejected(format!(
            "{path} changed while the patch was being applied; no files were modified"
        )),
        Err(error) => rejected(error.to_string()),
    }
}

/// UTF-8 text of every file the patch reads, keyed by path.
fn read_originals(
    parsed: &ParsedPatch,
    root: &Path,
) -> Result<HashMap<String, String>, String> {
    let mut originals = HashMap::new();
    let mut total: u64 = 0;
    for op in &parsed.ops {
        if matches!(op.change, Change::Add) {
            continue;
        }
        let full = root.join(&op.path);
        let size = fs::metadata(&full)
            .map_err(|e| format!("cannot inspect {}: {e}", op.path))?
            .len();
        if size > MAX_TARGET_BYTES {
            return Err(format!(
                "{} is {size} bytes; the limit for a patched file is {MAX_TARGET_BYTES}",
                op.path
            ));
        }
        total += size;
        if total > MAX_AGGREGATE_BYTES {
            return Err(format!(
                "the files this patch touches exceed {MAX_AGGREGATE_BYTES} bytes together"
            ));
        }
        let bytes = fs::read(&full).map_err(|e| format!("cannot read {}: {e}", op.path))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            format!("{} is not valid UTF-8; patching such a file is not supported", op.path)
        })?;
        if op.change == Change::Delete && detect_newline(&text).is_err() {
            return Err(format!(
                "{} has mixed or lone-CR line endings; deleting such a file is not supported",
                op.path
            ));
        }
        originals.insert(op.path.clone(), text);
    }
    Ok(originals)
}

fn remove_stale_scratch(project_dir: &Path) {
    let Ok(entries) = fs::read_dir(project_dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(SCRATCH_PREFIX) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_SCRATCH);
        if old {
            let path = entry.path();
            let _ = fs::remove_dir_all(&path);
            let _ = fs::remove_file(&path);
        }
    }
}

fn make_scratch(project_dir: &Path, id: &str) -> Result<Scratch, String> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = project_dir.join(format!("{SCRATCH_PREFIX}{id}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("cannot create scratch directory: {e}"))?;
    let dir = fs::canonicalize(&dir).unwrap_or(dir);
    let out = project_dir.join(format!("{SCRATCH_PREFIX}{id}.out"));
    Ok(Scratch { dir, out })
}

fn fill_scratch(
    dir: &Path,
    originals: &HashMap<String, String>,
    parsed: &ParsedPatch,
) -> Result<(), String> {
    // Directories for new files. A program that does not create them itself would
    // otherwise fail where the user's own run would have worked; empty
    // directories are ignored when the results are read back.
    for op in &parsed.ops {
        let destination = match &op.change {
            Change::Add => Some(op.path.as_str()),
            Change::Move { to } => Some(to.as_str()),
            _ => None,
        };
        if let Some(parent) = destination.and_then(|d| dir.join(d).parent().map(Path::to_path_buf)) {
            fs::create_dir_all(parent).map_err(|e| format!("cannot prepare scratch copy: {e}"))?;
        }
    }
    for (path, text) in originals {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot prepare scratch copy: {e}"))?;
        }
        fs::write(&target, text).map_err(|e| format!("cannot prepare scratch copy: {e}"))?;
    }
    Ok(())
}

/// Runs the program in the scratch directory. Returns its status and its
/// combined stdout and stderr, in the order it wrote them.
fn run_program(
    env: &Env,
    scratch: &Scratch,
    text: &str,
) -> Result<(std::process::ExitStatus, String), String> {
    let out_file = fs::File::create(&scratch.out)
        .map_err(|e| format!("cannot capture apply_patch output: {e}"))?;
    let err_file = out_file
        .try_clone()
        .map_err(|e| format!("cannot capture apply_patch output: {e}"))?;

    let mut child = Command::new(env.program)
        .current_dir(&scratch.dir)
        // A program that trusts $PWD must not be sent back to the real tree.
        .env("PWD", &scratch.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "apply_patch not found in PATH".to_string(),
            _ => format!("cannot run apply_patch: {e}"),
        })?;

    // The patch always goes on stdin, with a final line feed. A separate thread
    // writes it so a program that stops reading cannot block CVB.
    let mut stdin = child.stdin.take().ok_or("cannot open apply_patch stdin")?;
    let mut bytes = text.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= env.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                return Err(format!(
                    "apply_patch did not finish within {} seconds and was stopped; no files were modified",
                    env.timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("cannot wait for apply_patch: {e}")),
        }
    };
    let _ = writer.join();

    let mut captured = Vec::new();
    fs::File::open(&scratch.out)
        .and_then(|file| file.take(MAX_OUTPUT_BYTES as u64 + 1).read_to_end(&mut captured))
        .map_err(|e| format!("cannot read apply_patch output: {e}"))?;
    let truncated = captured.len() > MAX_OUTPUT_BYTES;
    captured.truncate(MAX_OUTPUT_BYTES);
    let mut output = String::from_utf8_lossy(&captured).into_owned();
    if truncated {
        output.push_str("\n[output truncated]\n");
    }
    Ok((status, output))
}

/// Replaces the scratch directory with the real root in `output`.
fn rewrite_paths(output: &str, scratch: &Path, root: &Path) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    let scratch = scratch.to_string_lossy();
    let scratch = scratch.trim_end_matches('/');
    let mut text = output.replace(scratch, root);
    // Anything that names the scratch directory in another form must not leak.
    if let Some(name) = Path::new(scratch).file_name().and_then(OsStr::to_str) {
        text = text.replace(name, ".");
    }
    text
}

/// Every regular file under `dir`, as a relative path. Anything else (a symlink,
/// a device) is an error: `apply_patch` must not produce it.
fn scratch_files(dir: &Path) -> Result<BTreeSet<String>, String> {
    fn walk(base: &Path, current: &Path, found: &mut BTreeSet<String>) -> Result<(), String> {
        let entries = fs::read_dir(current).map_err(|e| format!("cannot read scratch copy: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read scratch copy: {e}"))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("cannot inspect scratch copy: {e}"))?;
            if meta.is_dir() {
                walk(base, &path, found)?;
            } else if meta.is_file() {
                found.insert(relative);
            } else {
                return Err(format!(
                    "apply_patch produced something that is not a regular file: {relative}"
                ));
            }
        }
        Ok(())
    }
    let mut found = BTreeSet::new();
    walk(dir, dir, &mut found)?;
    Ok(found)
}

fn read_result(scratch: &Path, path: &str) -> Result<String, String> {
    let bytes = fs::read(scratch.join(path))
        .map_err(|e| format!("cannot read the patched {path}: {e}"))?;
    if bytes.len() as u64 > MAX_TARGET_BYTES {
        return Err(format!(
            "the patched {path} is {} bytes; the limit is {MAX_TARGET_BYTES}",
            bytes.len()
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("the patched {path} is not valid UTF-8"))
}

type Built = (Plan, HashMap<String, u32>, usize);

/// Turns the scratch results into one transaction.
fn build_plan(
    parsed: &ParsedPatch,
    originals: &HashMap<String, String>,
    scratch: &Path,
    state: &ProjectState,
    patch_text: &str,
    id: String,
) -> Result<Built, String> {
    let found = scratch_files(scratch)?;
    let mut expected_present: HashSet<&str> = HashSet::new();
    let mut expected_absent: HashSet<&str> = HashSet::new();
    let mut files: Vec<PlannedFile> = Vec::new();
    let mut modes: HashMap<String, u32> = HashMap::new();
    let mut changed = 0;
    let mut total: u64 = 0;

    let updated = |path: &str, proposed: String| -> Option<PlannedFile> {
        let original = &originals[path];
        (original != &proposed).then(|| PlannedFile {
            path: path.to_string(),
            original: original.clone(),
            original_digest: sha256_hex(original.as_bytes()),
            proposed_digest: sha256_hex(proposed.as_bytes()),
            proposed,
            operations: Vec::new(),
        })
    };
    let deleted = |path: &str| -> PlannedFile {
        let original = originals[path].clone();
        PlannedFile {
            path: path.to_string(),
            original_digest: sha256_hex(original.as_bytes()),
            original,
            proposed: String::new(),
            proposed_digest: ABSENT_DIGEST.to_string(),
            operations: Vec::new(),
        }
    };
    let added = |path: &str, proposed: String| PlannedFile {
        path: path.to_string(),
        original: String::new(),
        original_digest: ABSENT_DIGEST.to_string(),
        proposed_digest: sha256_hex(proposed.as_bytes()),
        proposed,
        operations: Vec::new(),
    };

    for op in &parsed.ops {
        match &op.change {
            Change::Add => {
                expected_present.insert(&op.path);
                if !found.contains(&op.path) {
                    return Err(format!(
                        "apply_patch reported success but did not create {}",
                        op.path
                    ));
                }
                let proposed = read_result(scratch, &op.path)?;
                total += proposed.len() as u64;
                files.push(added(&op.path, proposed));
                changed += 1;
            }
            Change::Update => {
                expected_present.insert(&op.path);
                if !found.contains(&op.path) {
                    return Err(format!("apply_patch removed {} unexpectedly", op.path));
                }
                let proposed = read_result(scratch, &op.path)?;
                if let Some(file) = updated(&op.path, proposed) {
                    total += file.proposed.len() as u64;
                    files.push(file);
                    changed += 1;
                }
            }
            Change::Delete => {
                expected_absent.insert(&op.path);
                if found.contains(&op.path) {
                    return Err(format!(
                        "apply_patch reported success but did not delete {}",
                        op.path
                    ));
                }
                files.push(deleted(&op.path));
                changed += 1;
            }
            Change::Move { to } => {
                expected_absent.insert(&op.path);
                expected_present.insert(to);
                if found.contains(&op.path) {
                    return Err(format!(
                        "apply_patch reported success but did not move {}",
                        op.path
                    ));
                }
                if !found.contains(to) {
                    return Err(format!("apply_patch did not create {to}"));
                }
                let proposed = read_result(scratch, to)?;
                total += proposed.len() as u64;
                // A move is a delete plus an add; the new file keeps the mode of
                // the old one.
                let source = state.canonical_root.join(&op.path);
                let meta = fs::metadata(&source)
                    .map_err(|e| format!("cannot inspect {}: {e}", op.path))?;
                {
                    use std::os::unix::fs::PermissionsExt;
                    modes.insert(to.clone(), meta.permissions().mode() & 0o777);
                }
                files.push(deleted(&op.path));
                files.push(added(to, proposed));
                changed += 1;
            }
        }
    }

    if let Some(extra) = found
        .iter()
        .find(|path| !expected_present.contains(path.as_str()) && !originals.contains_key(*path))
        .or_else(|| {
            found
                .iter()
                .find(|path| !expected_present.contains(path.as_str()) && !expected_absent.contains(path.as_str()))
        })
    {
        return Err(format!("apply_patch created an unexpected file: {extra}"));
    }
    if total > MAX_AGGREGATE_BYTES {
        return Err(format!(
            "the patched files exceed {MAX_AGGREGATE_BYTES} bytes together"
        ));
    }
    if files.is_empty() {
        return Err("the patch makes no changes".to_string());
    }

    let plan = Plan {
        token: "patch".to_string(),
        id,
        digest: sha256_hex(patch_text.as_bytes()),
        files,
    };
    Ok((plan, modes, changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for the real `apply_patch`, enough of it for these tests. Like
    /// the real one it writes each file as it goes (so it is not atomic), reads the
    /// patch on stdin, and reports absolute paths.
    const FAKE: &str = r#"#!/usr/bin/env bash
LOG="__LOG__"
cat > "$LOG.stdin"
{ echo "cwd=$(pwd -P)"; echo "PWD=$PWD"; echo "argc=$#"; } > "$LOG.env"
mapfile -t L < "$LOG.stdin"
n=${#L[@]}
i=0
changed=()
fail() { printf '%s\n' "$@" >&2; exit 1; }
while (( i < n )); do
  line=${L[i]}
  case "$line" in
    "*** Begin Patch"|"*** End Patch") i=$((i+1));;
    "*** Add File: "*)
      p=${line#"*** Add File: "}; i=$((i+1))
      mkdir -p "$(dirname "$p")"; : > "$p"
      while (( i < n )) && [[ ${L[i]} == +* ]]; do printf '%s\n' "${L[i]:1}" >> "$p"; i=$((i+1)); done
      changed+=("A $PWD/$p");;
    "*** Delete File: "*)
      p=${line#"*** Delete File: "}; i=$((i+1))
      rm "$p" 2>/dev/null || fail "Failed to delete file $PWD/$p"
      changed+=("D $PWD/$p");;
    "*** Update File: "*)
      p=${line#"*** Update File: "}; i=$((i+1)); dest=$p
      if (( i < n )) && [[ ${L[i]} == "*** Move to: "* ]]; then dest=${L[i]#"*** Move to: "}; i=$((i+1)); fi
      mapfile -t F < "$p" || fail "Failed to read $PWD/$p"
      while (( i < n )) && [[ ${L[i]} != "*** "* ]]; do
        old=(); new=()
        while (( i < n )) && [[ ${L[i]} != "*** "* ]]; do
          case "${L[i]}" in
            "@@"*) if (( ${#old[@]} + ${#new[@]} > 0 )); then break; fi;;
            -*) old+=("${L[i]:1}");;
            +*) new+=("${L[i]:1}");;
          esac
          i=$((i+1))
        done
        found=-1
        for (( s=0; s + ${#old[@]} <= ${#F[@]}; s++ )); do
          ok=1
          for (( k=0; k<${#old[@]}; k++ )); do [[ "${F[s+k]}" == "${old[k]}" ]] || { ok=0; break; }; done
          if (( ok )); then found=$s; break; fi
        done
        if (( found < 0 )); then fail "Failed to find expected lines in $PWD/$p:" "${old[@]}"; fi
        F=("${F[@]:0:found}" "${new[@]}" "${F[@]:found+${#old[@]}}")
      done
      mkdir -p "$(dirname "$dest")"
      printf '%s\n' "${F[@]}" > "$dest"
      if [[ $dest != "$p" ]]; then rm "$p"; fi
      changed+=("M $PWD/$dest");;
    *) i=$((i+1));;
  esac
done
echo "Success. Updated the following files:"
printf '%s\n' "${changed[@]}"
"#;

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        state: PathBuf,
        program: PathBuf,
        log: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!("cvb-run-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let root = base.join("root");
            fs::create_dir_all(&root).unwrap();
            fs::create_dir_all(base.join("bin")).unwrap();
            let root = fs::canonicalize(&root).unwrap();
            let base = fs::canonicalize(&base).unwrap();
            let log = base.join("log");
            let program = base.join("bin/apply_patch");
            let fixture = Fixture { state: base.join("state"), base, root, program, log };
            fixture.install(&FAKE.replace("__LOG__", &fixture.log.display().to_string()));
            fixture
        }

        fn install(&self, script: &str) {
            fs::write(&self.program, script).unwrap();
            fs::set_permissions(&self.program, fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.root.join(rel)).unwrap()
        }

        fn mode(&self, rel: &str) -> u32 {
            fs::metadata(self.root.join(rel)).unwrap().permissions().mode() & 0o777
        }

        fn env(&self, timeout: Duration) -> Env<'_> {
            Env { root: &self.root, state_base: &self.state, program: self.program.as_os_str(), timeout }
        }

        fn run(&self, body: &str) -> Report {
            self.run_with(body, Duration::from_secs(30))
        }

        fn run_with(&self, body: &str, timeout: Duration) -> Report {
            run_patch(&format!("*** Begin Patch\n{body}*** End Patch\n"), &self.env(timeout))
        }

        /// Every file under the root with its bytes and mode.
        fn snapshot(&self) -> BTreeMap<String, (Vec<u8>, u32)> {
            fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, u32)>) {
                for entry in fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    let meta = fs::symlink_metadata(&path).unwrap();
                    let rel = path.strip_prefix(base).unwrap().display().to_string();
                    if meta.is_dir() {
                        out.insert(format!("{rel}/"), (Vec::new(), 0));
                        walk(base, &path, out);
                    } else {
                        out.insert(rel, (fs::read(&path).unwrap(), meta.permissions().mode() & 0o777));
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(&self.root, &self.root, &mut out);
            out
        }

        /// Names in the project state directory that belong to the runner.
        fn leftovers(&self) -> Vec<String> {
            let mut found = Vec::new();
            for project in fs::read_dir(&self.state).into_iter().flatten().flatten() {
                for entry in fs::read_dir(project.path()).unwrap().flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(SCRATCH_PREFIX) || name.contains(".tmp") {
                        found.push(name);
                    }
                }
            }
            found
        }

        fn log(&self, suffix: &str) -> String {
            fs::read_to_string(format!("{}.{suffix}", self.log.display())).unwrap_or_default()
        }

        fn project_dir(&self) -> PathBuf {
            init_project_state(&self.state, &self.root).unwrap().dir
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn applied(report: &Report) -> (usize, &str) {
        match report {
            Report::Applied { files, output } => (*files, output),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn an_add_update_delete_patch_is_applied_together() {
        let f = Fixture::new("mixed");
        f.write("src/a.rs", "one\ntwo\nthree\n");
        f.write("gone.txt", "bye\n");

        let report = f.run(
            "*** Update File: src/a.rs\n@@\n-two\n+TWO\n\
             *** Add File: new/dir/c.txt\n+hello\n\
             *** Delete File: gone.txt\n",
        );

        let (files, output) = applied(&report);
        assert_eq!(files, 3);
        assert_eq!(f.read("src/a.rs"), "one\nTWO\nthree\n");
        assert_eq!(f.read("new/dir/c.txt"), "hello\n");
        assert!(!f.root.join("gone.txt").exists());
        // apply_patch's own summary, with real paths.
        let root = f.root.display();
        assert!(output.contains("Success. Updated the following files:"), "{output}");
        assert!(output.contains(&format!("M {root}/src/a.rs")), "{output}");
        assert!(output.contains(&format!("A {root}/new/dir/c.txt")), "{output}");
        assert!(output.contains(&format!("D {root}/gone.txt")), "{output}");
        assert!(!output.contains(SCRATCH_PREFIX), "{output}");
        assert!(report.to_text().starts_with("CVB patch APPLIED files=3\nSuccess."));
        assert_eq!(f.leftovers(), Vec::<String>::new());
    }

    /// The spike A case: the second file does not match. The installed binary
    /// would have left the first file changed; the real tree must stay identical.
    #[test]
    fn a_failure_in_the_second_file_leaves_the_real_tree_untouched() {
        let f = Fixture::new("atomic");
        f.write("a.txt", "one\n");
        f.write("b.txt", "two\n");
        f.write("keep.sh", "#!/bin/sh\n");
        fs::set_permissions(f.root.join("keep.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        let before = f.snapshot();

        let report = f.run(
            "*** Update File: a.txt\n@@\n-one\n+ONE\n\
             *** Update File: b.txt\n@@\n-does not exist\n+x\n\
             *** Add File: c.txt\n+c\n",
        );

        let Report::Failed { exit: Some(1), output } = &report else {
            panic!("expected a failure, got {report:?}");
        };
        let root = f.root.display();
        assert!(output.contains(&format!("Failed to find expected lines in {root}/b.txt:")), "{output}");
        assert!(output.contains("does not exist"), "{output}");
        assert!(!output.contains(SCRATCH_PREFIX), "{output}");
        assert_eq!(f.snapshot(), before, "the real tree changed");
        assert_eq!(f.leftovers(), Vec::<String>::new());

        let text = report.to_text();
        assert!(text.starts_with("CVB patch FAILED (apply_patch exit 1)\n"), "{text}");
        assert!(text.ends_with("No files were modified.\n"), "{text}");
    }

    /// The reported case end to end: pasting a patch whose changes are already
    /// in the file fails as before (nothing modified) and says why.
    #[test]
    fn a_patch_that_was_already_applied_fails_with_an_explanation() {
        let f = Fixture::new("twice");
        f.write("a.txt", "head\nNEW\ntail\n");
        let before = f.snapshot();

        let report = f.run("*** Update File: a.txt\n@@\n head\n-OLD\n+NEW\n tail\n");

        let Report::Failed { exit: Some(1), output } = &report else {
            panic!("expected a failure, got {report:?}");
        };
        let root = f.root.display();
        assert!(output.contains("Failed to find expected lines"), "{output}");
        assert!(output.contains("Note: every hunk of this patch appears to be already present"), "{output}");
        assert!(output.contains(&format!("{root}/a.txt (1 of 1 hunks)")), "{output}");
        assert_eq!(f.snapshot(), before);
        let text = report.to_text();
        let note_at = text.find("Note: every hunk").unwrap();
        let end_at = text.find("No files were modified.").unwrap();
        assert!(note_at < end_at && text.ends_with("No files were modified.\n"), "{text}");
    }

    #[test]
    fn an_ordinary_failure_gets_no_note() {
        let f = Fixture::new("nonote");
        f.write("a.txt", "one\n");
        let report = f.run("*** Update File: a.txt\n@@\n-absent\n+x\n");
        let Report::Failed { output, .. } = &report else { panic!("{report:?}") };
        assert!(!output.contains("Note:"), "{output}");
    }

    #[test]
    fn the_patch_goes_on_stdin_and_the_program_runs_in_the_scratch_copy() {
        let f = Fixture::new("stdin");
        f.write("a.txt", "x\n");
        let body = "*** Update File: a.txt\n@@\n-x\n+y\n";
        let text = format!("*** Begin Patch\n{body}*** End Patch");
        // No trailing newline, as pasted by the terminal.
        let report = run_patch(&text, &f.env(Duration::from_secs(30)));
        applied(&report);

        assert_eq!(f.log("stdin"), format!("{text}\n"));
        let env = f.log("env");
        assert!(env.contains("argc=0"), "{env}");
        let cwd = env.lines().find_map(|l| l.strip_prefix("cwd=")).unwrap();
        let pwd = env.lines().find_map(|l| l.strip_prefix("PWD=")).unwrap();
        assert!(cwd.contains(SCRATCH_PREFIX) && !cwd.starts_with(&f.root.display().to_string()), "{cwd}");
        assert_eq!(pwd, cwd, "$PWD must not point at the real tree");
    }

    /// bash quietly repairs a wrong $PWD, so read the environment the program was
    /// started with instead. A program that trusts $PWD (many do) must be pointed
    /// at the scratch copy, never at the real tree.
    #[test]
    fn the_programs_environment_names_the_scratch_directory() {
        let f = Fixture::new("pwdenv");
        f.write("a.txt", "x\n");
        f.install(&format!(
            "#!/usr/bin/env bash\ncat >/dev/null\ntr '\\0' '\\n' < /proc/$$/environ | grep '^PWD=' > '{}.rawpwd'\necho Success.\n",
            f.log.display()
        ));
        let _ = f.run("*** Add File: n.txt\n+n\n");
        let raw = f.log("rawpwd");
        let pwd = raw.trim().strip_prefix("PWD=").unwrap_or("");
        assert!(pwd.contains(SCRATCH_PREFIX), "{raw:?}");
        assert!(!pwd.starts_with(&f.root.display().to_string()), "{raw:?}");
    }

    #[test]
    fn a_move_keeps_the_mode_of_the_source() {
        let f = Fixture::new("move");
        f.write("bin/tool.sh", "#!/bin/sh\necho old\n");
        fs::set_permissions(f.root.join("bin/tool.sh"), fs::Permissions::from_mode(0o775)).unwrap();

        let report = f.run(
            "*** Update File: bin/tool.sh\n*** Move to: scripts/tool.sh\n@@\n-echo old\n+echo new\n",
        );

        assert_eq!(applied(&report).0, 1);
        assert!(!f.root.join("bin/tool.sh").exists());
        assert_eq!(f.read("scripts/tool.sh"), "#!/bin/sh\necho new\n");
        assert_eq!(f.mode("scripts/tool.sh"), 0o775);
        assert_eq!(f.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn an_update_that_changes_nothing_is_dropped_and_a_patch_of_only_those_is_refused() {
        let f = Fixture::new("noop");
        f.write("a.txt", "x\n");
        f.write("b.txt", "p\n");

        let report = f.run(
            "*** Update File: a.txt\n@@\n-x\n+x\n*** Update File: b.txt\n@@\n-p\n+q\n",
        );
        assert_eq!(applied(&report).0, 1);
        assert_eq!(f.read("b.txt"), "q\n");

        let before = f.snapshot();
        let report = f.run("*** Update File: a.txt\n@@\n-x\n+x\n");
        assert_eq!(report, Report::Rejected("the patch makes no changes".into()));
        assert_eq!(f.snapshot(), before);
    }

    #[test]
    fn checks_that_fail_run_nothing_and_leave_the_tree_alone() {
        let f = Fixture::new("refused");
        f.write("exists.txt", "x\n");
        let before = f.snapshot();

        for (body, needle) in [
            ("*** Add File: exists.txt\n+y\n", "Add File target already exists: exists.txt"),
            ("*** Update File: nope.txt\n@@\n-a\n+b\n", "Update File target does not exist: nope.txt"),
            ("*** Delete File: ../outside\n", "unsafe path"),
            ("*** Update File: exists.txt\n*** Move to: exists.txt\n", "moved onto itself"),
            ("stray\n*** Delete File: exists.txt\n", "unexpected line"),
        ] {
            let report = f.run(body);
            let Report::Rejected(reason) = &report else { panic!("{body:?}: {report:?}") };
            assert!(reason.contains(needle), "{body:?}: {reason}");
        }
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.log("stdin"), "", "apply_patch must not have been started");
    }

    #[test]
    fn files_that_cannot_be_carried_in_a_plan_are_refused() {
        let f = Fixture::new("limits");
        fs::write(f.root.join("binary.dat"), [0xff, 0xfe, 0x00]).unwrap();
        f.write("mixed.txt", "a\r\nb\n");
        fs::write(f.root.join("huge.txt"), vec![b'x'; MAX_TARGET_BYTES as usize + 1]).unwrap();
        let before = f.snapshot();

        for (body, needle) in [
            ("*** Update File: binary.dat\n@@\n-a\n+b\n", "not valid UTF-8"),
            ("*** Delete File: mixed.txt\n", "mixed or lone-CR line endings"),
            ("*** Update File: huge.txt\n@@\n-a\n+b\n", "the limit for a patched file"),
        ] {
            let report = f.run(body);
            let Report::Rejected(reason) = &report else { panic!("{body:?}: {report:?}") };
            assert!(reason.contains(needle), "{body:?}: {reason}");
        }
        assert_eq!(f.snapshot(), before);
    }

    /// A program that expects the directories of new files to exist (the runner
    /// creates them in the scratch copy) still works, and the real tree gets them
    /// created by the commit.
    #[test]
    fn directories_for_new_files_exist_in_the_scratch_copy() {
        let f = Fixture::new("newdirs");
        f.write("a.txt", "x\n");
        f.install(
            "#!/usr/bin/env bash\ncat >/dev/null\necho hi > deep/er/f.txt\necho \"A $PWD/deep/er/f.txt\"\n",
        );
        let report = f.run("*** Add File: deep/er/f.txt\n+hi\n");
        assert_eq!(applied(&report).0, 1);
        assert_eq!(f.read("deep/er/f.txt"), "hi\n");
    }

    #[test]
    fn a_missing_program_is_reported_clearly() {
        let f = Fixture::new("noprog");
        f.write("a.txt", "x\n");
        let before = f.snapshot();
        let env = Env {
            root: &f.root,
            state_base: &f.state,
            program: OsStr::new("definitely-not-an-apply-patch-binary"),
            timeout: Duration::from_secs(5),
        };
        let report = run_patch(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-x\n+y\n*** End Patch\n",
            &env,
        );
        assert_eq!(report, Report::Rejected("apply_patch not found in PATH".into()));
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn a_program_that_hangs_is_stopped() {
        let f = Fixture::new("hang");
        f.write("a.txt", "x\n");
        f.install("#!/usr/bin/env bash\nexec sleep 30\n");
        let before = f.snapshot();
        let started = Instant::now();

        let report = f.run_with("*** Update File: a.txt\n@@\n-x\n+y\n", Duration::from_millis(300));

        let Report::Rejected(reason) = &report else { panic!("{report:?}") };
        assert!(reason.contains("did not finish within 0 seconds"), "{reason}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn a_program_that_never_reads_stdin_does_not_block() {
        let f = Fixture::new("noread");
        f.write("a.txt", "x\n");
        f.install("#!/usr/bin/env bash\necho nope >&2\nexit 3\n");
        let mut text = String::from("*** Begin Patch\n*** Update File: a.txt\n@@\n-x\n");
        text.push_str(&"+y\n".repeat(200_000));
        text.push_str("*** End Patch\n");
        let report = run_patch(&text, &f.env(Duration::from_secs(30)));
        assert_eq!(report, Report::Failed { exit: Some(3), output: "nope\n".into() });
    }

    #[test]
    fn a_program_killed_by_a_signal_is_a_failure() {
        let f = Fixture::new("signal");
        f.write("a.txt", "x\n");
        f.install("#!/usr/bin/env bash\ncat >/dev/null\nkill -9 $$\n");
        let report = f.run("*** Update File: a.txt\n@@\n-x\n+y\n");
        assert!(matches!(report, Report::Failed { exit: None, .. }), "{report:?}");
        assert!(report.to_text().starts_with("CVB patch FAILED (apply_patch was terminated by a signal)\n"));
    }

    #[test]
    fn very_long_output_is_cut() {
        let f = Fixture::new("long");
        f.write("a.txt", "x\n");
        f.install("#!/usr/bin/env bash\ncat >/dev/null\nhead -c 2500000 /dev/zero | tr '\\0' 'x'\nexit 1\n");
        let Report::Failed { output, .. } = f.run("*** Update File: a.txt\n@@\n-x\n+y\n") else {
            panic!("expected a failure");
        };
        assert!(output.ends_with("\n[output truncated]\n"));
        assert!(output.len() < MAX_OUTPUT_BYTES + 100);
    }

    #[test]
    fn success_that_did_not_happen_is_not_believed() {
        let f = Fixture::new("liar");
        f.write("a.txt", "x\n");
        f.write("b.txt", "y\n");
        f.install("#!/usr/bin/env bash\ncat >/dev/null\necho Success.\n");
        let before = f.snapshot();

        for (body, needle) in [
            ("*** Add File: new.txt\n+n\n", "did not create new.txt"),
            ("*** Delete File: b.txt\n", "did not delete b.txt"),
            ("*** Update File: a.txt\n*** Move to: c.txt\n", "did not move a.txt"),
        ] {
            let report = f.run(body);
            let Report::Rejected(reason) = &report else { panic!("{body:?}: {report:?}") };
            assert!(reason.contains(needle), "{body:?}: {reason}");
        }
        assert_eq!(f.snapshot(), before);
    }

    #[test]
    fn files_the_patch_did_not_name_are_refused() {
        let f = Fixture::new("extra");
        f.write("a.txt", "x\n");
        let fake = f.program.display().to_string();
        let real = format!("{fake}.real");
        fs::rename(&f.program, &real).unwrap();
        f.install(&format!(
            "#!/usr/bin/env bash\n{real}\nrc=$?\necho stray > extra.txt\nexit $rc\n"
        ));
        let before = f.snapshot();

        let report = f.run("*** Update File: a.txt\n@@\n-x\n+y\n");

        let Report::Rejected(reason) = &report else { panic!("{report:?}") };
        assert!(reason.contains("unexpected file: extra.txt"), "{reason}");
        assert_eq!(f.snapshot(), before);
    }

    /// A file edited by someone else while apply_patch runs must not be
    /// overwritten: the commit re-checks every original under the project lock.
    #[test]
    fn a_file_changed_during_the_run_is_not_overwritten() {
        let f = Fixture::new("race");
        f.write("a.txt", "x\n");
        f.write("b.txt", "p\n");
        let real = format!("{}.real", f.program.display());
        fs::rename(&f.program, &real).unwrap();
        f.install(&format!(
            "#!/usr/bin/env bash\n{real}\nrc=$?\necho 'edited by someone' > '{}/b.txt'\nexit $rc\n",
            f.root.display()
        ));

        let report = f.run(
            "*** Update File: a.txt\n@@\n-x\n+y\n*** Update File: b.txt\n@@\n-p\n+q\n",
        );

        let Report::Rejected(reason) = &report else { panic!("{report:?}") };
        assert!(reason.contains("b.txt changed while the patch was being applied"), "{reason}");
        assert!(reason.contains("no files were modified"), "{reason}");
        assert_eq!(f.read("a.txt"), "x\n");
        assert_eq!(f.read("b.txt"), "edited by someone\n");
        assert_eq!(f.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn a_locked_project_is_refused_without_changes() {
        let f = Fixture::new("locked");
        f.write("a.txt", "x\n");
        let _lock = crate::surgery::lock::ProjectLock::acquire(&f.project_dir()).unwrap();
        let before = f.snapshot();

        let report = f.run("*** Update File: a.txt\n@@\n-x\n+y\n");

        let Report::Rejected(reason) = &report else { panic!("{report:?}") };
        assert!(reason.contains("locked"), "{reason}");
        assert_eq!(f.snapshot(), before);
    }

    #[test]
    fn leftover_scratch_from_a_crashed_run_is_cleaned_up_when_old() {
        let f = Fixture::new("stale");
        f.write("a.txt", "x\n");
        let project = f.project_dir();
        let stale = project.join(format!("{SCRATCH_PREFIX}stale"));
        let fresh = project.join(format!("{SCRATCH_PREFIX}fresh"));
        for dir in [&stale, &fresh] {
            fs::create_dir_all(dir.join("inner")).unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        fs::File::open(&stale).unwrap().set_modified(old).unwrap();

        applied(&f.run("*** Update File: a.txt\n@@\n-x\n+y\n"));

        assert!(!stale.exists(), "an old scratch directory should be removed");
        assert!(fresh.exists(), "a recent one may belong to a running patch");
    }

    #[test]
    fn scratch_paths_are_rewritten_in_every_form() {
        let scratch = Path::new("/state/p/patch-scratch-1-2-3");
        let root = Path::new("/home/u/proj");
        let text = "A /state/p/patch-scratch-1-2-3/x.rs\nin /state/p/patch-scratch-1-2-3\nodd: ../patch-scratch-1-2-3/y\n";
        let out = rewrite_paths(text, scratch, root);
        assert_eq!(out, "A /home/u/proj/x.rs\nin /home/u/proj\nodd: .././y\n");
        assert!(!out.contains("patch-scratch"));
    }

    #[test]
    fn report_text_follows_the_output_contract() {
        assert_eq!(
            Report::Applied { files: 2, output: "Success.\nM /p/a\n".into() }.to_text(),
            "CVB patch APPLIED files=2\nSuccess.\nM /p/a\n"
        );
        assert_eq!(
            Report::Failed { exit: Some(1), output: "boom".into() }.to_text(),
            "CVB patch FAILED (apply_patch exit 1)\nboom\nNo files were modified.\n"
        );
        assert_eq!(
            Report::Failed { exit: Some(1), output: String::new() }.to_text(),
            "CVB patch FAILED (apply_patch exit 1)\nNo files were modified.\n"
        );
        assert_eq!(
            Report::Rejected("bad".into()).to_text(),
            "CVB patch REJECTED: bad\n"
        );
    }
}
