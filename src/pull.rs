use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::cascade;
use crate::units;

const PATCH_REMINDER: &str =
    "Edit with a patch: apply_patch format or git diff, pasted at the prompt of this session.";

/// The `file:` scopes (exact duplicates removed, first-seen order), the `lines:`
/// ranges and the remaining query arguments of a pull.
type FileScope = (Vec<String>, Vec<(usize, usize)>, Vec<String>);

fn append_patch_reminder(stdout: &mut String) {
    stdout.push_str(PATCH_REMINDER);
    stdout.push('\n');
}

pub fn tracked_files(dir: Option<&Path>) -> Result<Vec<OsString>, String> {
    let mut cmd = Command::new("git");
    // Tracked files plus untracked ones that .gitignore does not hide, so a file
    // that has not been added or committed yet can be pulled.
    cmd.args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }

    let output = cmd
        .output()
        .map_err(|error| format!("CVB pull: unable to run git ls-files: {error}"))?;

    if !output.status.success() {
        return Err(
            "CVB pull: unable to list files; run inside a Git repository.".to_string(),
        );
    }

    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_vec(path.to_vec()))
        .filter(|path| {
            let p = Path::new(path);
            let full_path = if let Some(d) = dir {
                d.join(p)
            } else {
                p.to_path_buf()
            };
            full_path.exists()
        })
        .collect::<Vec<_>>();

    if files.is_empty() {
        return Err("CVB pull: no files found (tracked, or untracked and not ignored).".to_string());
    }

    Ok(files)
}

pub fn normalize_path_str(path: &str, base_dir: Option<&Path>) -> String {
    let p = Path::new(path);
    let p = if let Some(base) = base_dir {
        p.strip_prefix(base).unwrap_or(p)
    } else if let Ok(cwd) = std::env::current_dir() {
        p.strip_prefix(&cwd).unwrap_or(p)
    } else {
        p
    };

    let mut parts = Vec::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(c) => parts.push(c.to_string_lossy().into_owned()),
            _ => parts.push(comp.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts.join("/")
}

/// `head:N` / `tail:N`: how many lines of a whole file or of `lines:` output to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trim {
    Head(usize),
    Tail(usize),
}

fn parse_trim_count(name: &str, value: &str) -> Result<usize, String> {
    let error = || {
        format!("CVB pull: {name}: must be followed by a positive integer (digits only, at least 1).")
    };
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error());
    }
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(error()),
    }
}

/// Splits the bare `list:` directive out of the arguments.
fn extract_list(args: &[String]) -> (bool, Vec<String>) {
    let list = args.iter().any(|arg| arg == "list:");
    let rest = args.iter().filter(|arg| *arg != "list:").cloned().collect();
    (list, rest)
}

/// From this many matching units on, a plain pull adds a line that says how to
/// narrow the search.
const MANY_UNITS: usize = 10;

/// Splits `head:N` / `tail:N` out of the arguments.
fn extract_trim(args: &[String]) -> Result<(Option<Trim>, Vec<String>), String> {
    let mut head = None;
    let mut tail = None;
    let mut remaining = Vec::new();

    for arg in args {
        if arg.starts_with("-head:") || arg.starts_with("-tail:") {
            return Err(format!(
                "CVB pull: negated selector '{arg}' is not supported; exclusions only apply to content terms."
            ));
        }
        if let Some(value) = arg.strip_prefix("head:") {
            if head.is_some() {
                return Err("CVB pull: only one head: directive is allowed.".to_string());
            }
            head = Some(parse_trim_count("head", value)?);
        } else if let Some(value) = arg.strip_prefix("tail:") {
            if tail.is_some() {
                return Err("CVB pull: only one tail: directive is allowed.".to_string());
            }
            tail = Some(parse_trim_count("tail", value)?);
        } else {
            remaining.push(arg.clone());
        }
    }

    match (head, tail) {
        (Some(_), Some(_)) => Err("CVB pull: head: and tail: cannot be combined.".to_string()),
        (Some(n), None) => Ok((Some(Trim::Head(n)), remaining)),
        (None, Some(n)) => Ok((Some(Trim::Tail(n)), remaining)),
        (None, None) => Ok((None, remaining)),
    }
}

fn format_ranges(ranges: &[(usize, usize)]) -> String {
    ranges
        .iter()
        .map(|&(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_line_range(range: &str) -> Result<(usize, usize), String> {
    let Some((start, end)) = range.split_once('-') else {
        let line = range.parse::<usize>().map_err(|_| {
            format!("CVB pull: invalid line selector '{range}'; expected LINE or START-END.")
        })?;

        if line == 0 {
            return Err(
                "CVB pull: invalid line selector '0'; line numbers start at 1.".to_string(),
            );
        }

        return Ok((line, line));
    };

    let start = start.parse::<usize>().map_err(|_| {
        format!("CVB pull: invalid line range '{range}'; START must be a positive line number.")
    })?;
    let end = end.parse::<usize>().map_err(|_| {
        format!("CVB pull: invalid line range '{range}'; END must be a positive line number.")
    })?;

    if start == 0 || end == 0 {
        return Err(format!(
            "CVB pull: invalid line range '{range}'; line numbers start at 1."
        ));
    }

    if start > end {
        return Err(format!(
            "CVB pull: invalid line range '{range}'; START must not exceed END."
        ));
    }

    Ok((start, end))
}

#[allow(dead_code)]
fn read_line_range(path: &str, start: usize, end: usize) -> Result<String, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("CVB pull: unable to read {path} as UTF-8: {error}"))?;
    let lines: Vec<&str> = source.lines().collect();

    if start == 0 || end == 0 || start > end {
        return Err(
            "CVB pull: invalid line range; expected positive START not exceeding END.".to_string(),
        );
    }

    if end > lines.len() {
        return Err(format!(
            "CVB pull: line range {start}-{end} exceeds {path}, which has {} lines.",
            lines.len()
        ));
    }

    let mut output = String::new();
    for number in start..=end {
        output.push_str(&format!("{path}:{number}: {}\n", lines[number - 1]));
    }

    Ok(output)
}

fn extract_file_scope(args: &[String]) -> Result<FileScope, String> {
    let mut scopes: Vec<String> = Vec::new();
    let mut file_args = 0;
    let mut line_ranges = Vec::new();
    let mut query_args = Vec::new();

    for arg in args {
        if arg.starts_with("-file:")
            || arg.starts_with("-lines:")
            || arg.starts_with("-callers:")
            || arg.starts_with("-cascade:")
        {
            return Err(format!(
                "CVB pull: negated selector '{arg}' is not supported; exclusions only apply to content terms."
            ));
        }

        if let Some(path) = arg.strip_prefix("file:") {
            if path.is_empty() {
                return Err("CVB pull: file: must be followed by a path.".to_string());
            }
            file_args += 1;
            if !scopes.iter().any(|known| known == path) {
                scopes.push(path.to_string());
            }
        } else if let Some(range) = arg.strip_prefix("lines:") {
            line_ranges.push(parse_line_range(range)?);
        } else {
            query_args.push(arg.clone());
        }
    }

    if !line_ranges.is_empty() && scopes.is_empty() {
        return Err("CVB pull: lines: requires a file: scope.".to_string());
    }

    if !line_ranges.is_empty() && scopes.len() > 1 {
        return Err("CVB pull: lines: requires exactly one file: scope.".to_string());
    }

    if query_args.is_empty() && line_ranges.is_empty() {
        if file_args > 1 {
            return Err(
                "CVB pull: several file: scopes need a search term; a whole-file capture takes exactly one file:."
                    .to_string(),
            );
        }
        return Err("CVB pull: search query is empty.".to_string());
    }

    Ok((scopes, line_ranges, query_args))
}

fn scope_matches(path: &OsStr, scope: &str, norm_scope: &str, base_dir: Option<&Path>) -> bool {
    let path_str = path.to_string_lossy();
    path == OsStr::new(scope)
        || path_str == norm_scope
        || normalize_path_str(&path_str, base_dir) == norm_scope
}

/// Keeps the files named by any of `scopes`. An empty list keeps everything.
/// Every scope must match a file; the first one that does not fails the whole
/// call, so a typo never yields partial results. The kept files stay in the
/// order of `files`, so the order of the scopes never shows in the output.
pub fn filter_file_scope(
    files: Vec<OsString>,
    scopes: &[String],
    base_dir: Option<&Path>,
) -> Result<Vec<OsString>, String> {
    if scopes.is_empty() {
        return Ok(files);
    }

    let normalized: Vec<(&str, String)> = scopes
        .iter()
        .map(|scope| (scope.as_str(), normalize_path_str(scope, base_dir)))
        .collect();

    for (scope, norm_scope) in &normalized {
        if !files
            .iter()
            .any(|path| scope_matches(path, scope, norm_scope, base_dir))
        {
            return Err(format!(
                "CVB pull: file scope matches no searchable file (tracked, or untracked and not ignored by Git): {scope}"
            ));
        }
    }

    Ok(files
        .into_iter()
        .filter(|path| {
            normalized
                .iter()
                .any(|(scope, norm_scope)| scope_matches(path, scope, norm_scope, base_dir))
        })
        .collect())
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub alternatives: Vec<String>,
    pub required: Vec<String>,
    pub excluded: Vec<String>,
}

impl SearchQuery {
    pub fn format_echo(&self) -> String {
        let mut parts = Vec::new();
        if self.alternatives.len() == 1 {
            parts.push(self.alternatives[0].clone());
        } else if self.alternatives.len() > 1 {
            let or_part = self.alternatives.join(" OR ");
            if !self.required.is_empty() || !self.excluded.is_empty() {
                parts.push(format!("({or_part})"));
            } else {
                parts.push(or_part);
            }
        }

        for req in &self.required {
            parts.push(req.clone());
        }

        for exc in &self.excluded {
            parts.push(format!("NOT {exc}"));
        }

        format!("Query: {}", parts.join(" AND "))
    }
}

pub fn parse_query(args: &[String]) -> Result<SearchQuery, String> {
    let mut query = SearchQuery::default();

    for arg in args {
        if arg.starts_with("-file:")
            || arg.starts_with("-lines:")
            || arg.starts_with("-callers:")
            || arg.starts_with("-cascade:")
        {
            return Err(format!(
                "CVB pull: negated selector '{arg}' is not supported; exclusions only apply to content terms."
            ));
        }

        if let Some(rest) = arg.strip_prefix('\\') {
            if rest.is_empty() {
                return Err("CVB pull: search operators must be followed by a term.".to_string());
            }
            query.alternatives.push(rest.to_string());
            continue;
        }

        if arg.len() >= 2 && arg.starts_with('"') && arg.ends_with('"') {
            let inner = &arg[1..arg.len() - 1];
            if inner.is_empty() {
                return Err("CVB pull: search operators must be followed by a term.".to_string());
            }
            query.alternatives.push(inner.to_string());
            continue;
        }

        if let Some(mut rest) = arg.strip_prefix('+') {
            if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
                rest = &rest[1..rest.len() - 1];
            }
            if rest.is_empty() {
                return Err("CVB pull: search operators must be followed by a term.".to_string());
            }
            query.required.push(rest.to_string());
            continue;
        }

        if let Some(mut rest) = arg.strip_prefix('-') {
            if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
                rest = &rest[1..rest.len() - 1];
            }
            if rest.is_empty() {
                return Err("CVB pull: search operators must be followed by a term.".to_string());
            }
            query.excluded.push(rest.to_string());
            continue;
        }

        if arg.is_empty() {
            return Err("CVB pull: search query is empty.".to_string());
        }

        query.alternatives.push(arg.clone());
    }

    if query.alternatives.is_empty() && query.required.is_empty() && query.excluded.is_empty() {
        return Err("CVB pull: search query is empty.".to_string());
    }

    if query.alternatives.is_empty() && query.required.is_empty() {
        return Err(
            "CVB pull: exclusion-only queries are not allowed; exclusions need at least one positive term."
                .to_string(),
        );
    }

    Ok(query)
}

#[allow(dead_code)]
pub fn matches_query(query: &SearchQuery, text: &str) -> bool {
    let has_alternative =
        query.alternatives.is_empty() || query.alternatives.iter().any(|term| text.contains(term));

    has_alternative
        && query.required.iter().all(|term| text.contains(term))
        && query.excluded.iter().all(|term| !text.contains(term))
}

fn run_search(
    program: &str,
    term: &str,
    files: &[OsString],
    dir: Option<&Path>,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    if let Some(d) = dir {
        command.current_dir(d);
    }

    if program.ends_with("/rg") || program == "rg" {
        command.args([
            "--fixed-strings",
            "--line-number",
            "--with-filename",
            "--color",
            "never",
            "--no-heading",
            "--null",
            "--",
        ]);
    } else {
        command.args(["-F", "-n", "-H", "-Z", "--"]);
    }

    command.arg(term);
    command.args(files);
    command.output()
}

fn matching_content(record: &str) -> Option<&str> {
    for (index, _) in record.match_indices(':') {
        let rest = &record[index + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();

        if digits > 0 && rest.as_bytes().get(digits) == Some(&b':') {
            return Some(&rest[digits + 1..]);
        }
    }
    None
}

fn parse_search_records(raw: &[u8]) -> Vec<(String, usize, String)> {
    if raw.is_empty() {
        return Vec::new();
    }

    if raw.contains(&0) {
        let mut results = Vec::new();
        let mut i = 0;
        let n = raw.len();

        while i < n {
            let Some(null_rel) = raw[i..].iter().position(|&b| b == 0) else {
                break;
            };
            let null_idx = i + null_rel;
            let path_bytes = &raw[i..null_idx];

            let Some(colon_rel) = raw[null_idx + 1..].iter().position(|&b| b == b':') else {
                break;
            };
            let colon_idx = null_idx + 1 + colon_rel;
            let line_bytes = &raw[null_idx + 1..colon_idx];

            let (content_bytes, next_i) =
                match raw[colon_idx + 1..].iter().position(|&b| b == b'\n') {
                    Some(nl_rel) => {
                        let nl_idx = colon_idx + 1 + nl_rel;
                        (&raw[colon_idx + 1..nl_idx], nl_idx + 1)
                    }
                    None => (&raw[colon_idx + 1..], n),
                };

            i = next_i;

            if let Ok(line_str) = std::str::from_utf8(line_bytes)
                && let Ok(line_num) = line_str.parse::<usize>() {
                    let path_str = String::from_utf8_lossy(path_bytes).into_owned();
                    let content_str = String::from_utf8_lossy(content_bytes).into_owned();
                    results.push((path_str, line_num, content_str));
                }
        }

        return results;
    }

    let mut results = Vec::new();
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        if let Some(content) = matching_content(line) {
            let mut parts = line.splitn(3, ':');
            if let (Some(path), Some(line_str)) = (parts.next(), parts.next())
                && let Ok(line_num) = line_str.parse::<usize>() {
                    results.push((path.to_string(), line_num, content.to_string()));
                }
        }
    }
    results
}

#[allow(dead_code)]
pub fn filter_matches(query: &SearchQuery, matches: &str) -> String {
    let mut seen = HashSet::new();
    let mut filtered = String::new();

    for record in matches.lines() {
        let Some(content) = matching_content(record) else {
            continue;
        };

        if matches_query(query, content) && seen.insert(record.to_string()) {
            filtered.push_str(record);
            filtered.push('\n');
        }
    }

    filtered
}

/// A plain name: letters, digits, `_`, `$`, and `-` after the first character
/// (Bash function names). Paths such as `module::run` are not names.
fn is_callable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '-'))
}

/// Whether `callers:` reads `path`: Rust, or a language `units` knows (Bash
/// included).
fn callers_understands(path: &str) -> bool {
    Path::new(path).extension() == Some(OsStr::new("rs")) || units::callers_language_for(path).is_some()
}

fn extract_callers_scope(args: &[String]) -> Result<Option<(String, Vec<String>)>, String> {
    for arg in args {
        if arg.starts_with("-callers:")
            || arg.starts_with("-file:")
            || arg.starts_with("-lines:")
            || arg.starts_with("-cascade:")
        {
            return Err(format!(
                "CVB pull: negated selector '{arg}' is not supported; exclusions only apply to content terms."
            ));
        }
    }

    let selectors: Vec<&String> = args
        .iter()
        .filter(|arg| arg.starts_with("callers:"))
        .collect();

    if selectors.is_empty() {
        return Ok(None);
    }
    if selectors.len() != 1 {
        return Err("CVB pull: only one callers: selector is allowed.".to_string());
    }

    let target = selectors[0]
        .strip_prefix("callers:")
        .expect("selector prefix was checked");

    if !is_callable_name(target) {
        return Err(
            "CVB pull: callers: must be followed by a function or method name.".to_string(),
        );
    }

    let mut scopes: Vec<String> = Vec::new();

    for arg in args {
        if arg == selectors[0] {
            continue;
        }

        if let Some(path) = arg.strip_prefix("file:") {
            if path.is_empty() {
                return Err("CVB pull: file: must be followed by a path.".to_string());
            }
            if !callers_understands(path) {
                return Err(
                    "CVB pull: callers: file scope must be a source file in a supported language \
                     (Rust, Bash, Python, Zig, C, C++, JavaScript, TypeScript, Go, Java, Lua)."
                        .to_string(),
                );
            }
            if !scopes.iter().any(|known| known == path) {
                scopes.push(path.to_string());
            }
        } else if arg.starts_with("lines:") {
            return Err("CVB pull: callers: cannot be combined with lines:.".to_string());
        } else {
            return Err("CVB pull: callers: cannot be combined with search terms.".to_string());
        }
    }

    Ok(Some((target.to_string(), scopes)))
}

fn run_callers(
    target: &str,
    scopes: &[String],
    dir: Option<&Path>,
    stdout: &mut String,
    stderr: &mut String,
) -> i32 {
    let files = match tracked_files(dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return 2;
        }
    };

    let files = match filter_file_scope(files, scopes, dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return 2;
        }
    };

    let source_files: Vec<OsString> = files
        .into_iter()
        .filter(|path| callers_understands(&path.to_string_lossy()))
        .collect();

    let mut found = false;
    let mut incomplete = false;

    for path in source_files {
        let display_path = path.to_string_lossy();
        let is_rust = Path::new(&path).extension() == Some(OsStr::new("rs"));
        let full_path = if let Some(d) = dir {
            d.join(&path)
        } else {
            PathBuf::from(&path)
        };

        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(error) => {
                stderr.push_str(&format!(
                    "CVB pull: warning: unable to read file {display_path}: {error}; \
                     caller results may be incomplete.\n"
                ));
                incomplete = true;
                continue;
            }
        };

        let matches = if is_rust {
            rust_callers(&source, target)
        } else {
            let lang = units::callers_language_for(&display_path).expect("checked by the filter");
            units::callers(lang, &source, target).map(|(sites, has_errors)| {
                if has_errors {
                    stderr.push_str(&format!(
                        "CVB pull: warning: {display_path} has syntax errors; calls in the \
                         broken parts are skipped and caller results may be incomplete.\n"
                    ));
                    incomplete = true;
                }
                sites
                    .into_iter()
                    .map(|site| RustCallerMatch {
                        line: site.line,
                        enclosing_function: site.enclosing,
                        kind: if site.member {
                            RustCallKind::Method
                        } else {
                            RustCallKind::Function
                        },
                    })
                    .collect()
            })
        };
        let Some(matches) = matches else {
            stderr.push_str(&format!(
                "CVB pull: warning: skipping unparseable file {display_path}; \
                 caller results may be incomplete.\n"
            ));
            incomplete = true;
            continue;
        };

        for item in matches {
            found = true;
            let kind = match item.kind {
                RustCallKind::Function => "function call",
                RustCallKind::Method => "method-call candidate",
                RustCallKind::MacroArgument => "macro-argument candidate",
            };

            match item.enclosing_function.as_deref() {
                Some(function) => stdout.push_str(&format!(
                    "{display_path}:{}: {kind} to {target} inside {function}\n",
                    item.line
                )),
                None => stdout.push_str(&format!(
                    "{display_path}:{}: {kind} to {target} outside a function\n",
                    item.line
                )),
            }
        }
    }

    let status = callers_exit_status(found, incomplete);

    if status == 2 {
        stderr.push_str("CVB pull: caller scan incomplete; no reliable no-callers conclusion.\n");
    } else if status == 1 {
        stdout.push_str(&format!("No callers found for: {target}\n"));
    }

    status
}

fn callers_exit_status(found: bool, incomplete: bool) -> i32 {
    if found {
        0
    } else if incomplete {
        2
    } else {
        1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitKey {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct SearchUnit {
    pub key: UnitKey,
    pub label: &'static str,
    pub warning: Option<String>,
}

pub type MatchingSeed = (SearchUnit, Vec<(String, Vec<usize>)>);

fn resolve_unit(path: &str, line: usize, source: &str) -> SearchUnit {
    let is_rust = path.ends_with(".rs");
    let is_bash = path.ends_with(".bash") || path.ends_with(".sh");
    let is_markdown = path.ends_with(".md") || path.ends_with(".markdown");

    if is_rust {
        match rust_unit_range(source, line) {
            Ok(((start, end), label)) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start,
                    end,
                },
                label,
                warning: None,
            },
            Err(()) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start: line,
                    end: line,
                },
                label: "Line",
                warning: Some(format!(
                    "skipping unparseable Rust file {path}; falling back to line-level matching"
                )),
            },
        }
    } else if is_bash {
        match bash_unit_range(source, line) {
            Ok(((start, end), label)) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start,
                    end,
                },
                label,
                warning: None,
            },
            Err(()) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start: line,
                    end: line,
                },
                label: "Line",
                warning: Some(format!(
                    "skipping unparseable Bash file {path}; falling back to line-level matching"
                )),
            },
        }
    } else if let Some(lang) = crate::units::language_for(path) {
        match crate::units::unit_range(lang, source, line) {
            Ok(((start, end), label)) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start,
                    end,
                },
                label,
                warning: None,
            },
            Err(()) => SearchUnit {
                key: UnitKey {
                    path: path.to_string(),
                    start: line,
                    end: line,
                },
                label: "Line",
                warning: Some(format!(
                    "skipping unparseable file {path}; falling back to line-level matching"
                )),
            },
        }
    } else if is_markdown {
        let ((start, end), label) = markdown_unit_range(source, line);
        SearchUnit {
            key: UnitKey {
                path: path.to_string(),
                start,
                end,
            },
            label,
            warning: None,
        }
    } else {
        SearchUnit {
            key: UnitKey {
                path: path.to_string(),
                start: line,
                end: line,
            },
            label: "Line",
            warning: None,
        }
    }
}

/// An empty list means no restriction.
fn line_in_ranges(line: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.is_empty() || ranges.iter().any(|&(start, end)| start <= line && line <= end)
}

fn count_units_for_term(
    term: &str,
    files: &[OsString],
    file_cache: &mut HashMap<String, String>,
    dir: Option<&Path>,
    restrict: &[(usize, usize)],
) -> usize {
    let output = match run_search("rg", term, files, dir) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match run_search("/usr/bin/grep", term, files, dir) {
                Ok(output) => output,
                Err(_) => return 0,
            }
        }
        Err(_) => return 0,
    };

    let records = parse_search_records(&output.stdout);
    let mut unique_units = HashSet::new();

    for (path, line, _) in records {
        if !line_in_ranges(line, restrict) {
            continue;
        }
        let source = if let Some(src) = file_cache.get(&path) {
            src.clone()
        } else {
            let full_path = if let Some(d) = dir {
                d.join(&path)
            } else {
                PathBuf::from(&path)
            };
            if let Ok(src) = std::fs::read_to_string(&full_path) {
                file_cache.insert(path.clone(), src.clone());
                src
            } else {
                continue;
            }
        };

        let unit = resolve_unit(&path, line, &source);
        unique_units.insert(unit.key);
    }

    unique_units.len()
}

pub fn run(args: &[String]) -> i32 {
    run_in_dir(args, None)
}

pub fn run_in_dir(args: &[String], dir: Option<&Path>) -> i32 {
    let (status, stdout, stderr) = run_capture(args, dir);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
    status
}

#[allow(dead_code)]
pub fn run_capture(args: &[String], dir: Option<&Path>) -> (i32, String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();

    let (cascade_depth, args_without_cascade) = match cascade::extract_directive(args) {
        Ok(result) => result,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    let (list_mode, args_without_cascade) = extract_list(&args_without_cascade);

    let (trim, args_without_cascade) = match extract_trim(&args_without_cascade) {
        Ok(result) => result,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    if trim.is_some() {
        let conflict = if cascade_depth.is_some() {
            Some("CVB pull: head:/tail: cannot be combined with cascade:.")
        } else if args_without_cascade
            .iter()
            .any(|arg| arg.starts_with("callers:"))
        {
            Some("CVB pull: head:/tail: cannot be combined with callers:.")
        } else {
            None
        };
        if let Some(message) = conflict {
            stderr.push_str(&format!("{message}\n"));
            return (2, stdout, stderr);
        }
    }

    if list_mode {
        let conflict = if cascade_depth.is_some() {
            Some("CVB pull: list: cannot be combined with cascade:.")
        } else if trim.is_some() {
            Some("CVB pull: list: cannot be combined with head:/tail:.")
        } else if args_without_cascade
            .iter()
            .any(|arg| arg.starts_with("callers:") || arg.starts_with("lines:"))
        {
            Some("CVB pull: list: cannot be combined with callers: or lines:.")
        } else if args_without_cascade
            .iter()
            .all(|arg| arg.starts_with("file:"))
        {
            Some("CVB pull: list: needs search terms.")
        } else {
            None
        };
        if let Some(message) = conflict {
            stderr.push_str(&format!("{message}\n"));
            return (2, stdout, stderr);
        }
    }

    match extract_callers_scope(&args_without_cascade) {
        Ok(Some((target, scopes))) => {
            let status = run_callers(&target, &scopes, dir, &mut stdout, &mut stderr);
            return (status, stdout, stderr);
        }
        Ok(None) => {}
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    }

    if args_without_cascade.is_empty() {
        if trim.is_some() {
            stderr.push_str("CVB pull: head:/tail: requires a file: scope.\n");
        } else {
            stderr.push_str(format_usage());
        }
        return (2, stdout, stderr);
    }

    if trim.is_some()
        && args_without_cascade
            .iter()
            .filter(|arg| arg.starts_with("file:"))
            .count()
            > 1
    {
        stderr.push_str("CVB pull: head:/tail: requires exactly one file: scope.\n");
        return (2, stdout, stderr);
    }

    let whole_file_capture =
        args_without_cascade.len() == 1 && args_without_cascade[0].starts_with("file:");

    let parsed = if whole_file_capture {
        extract_file_scope(&[args_without_cascade[0].clone(), "lines:1".to_string()])
    } else {
        extract_file_scope(&args_without_cascade)
    };
    let (scopes, mut line_ranges, query_args) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    if trim.is_some() && !query_args.is_empty() {
        stderr.push_str("CVB pull: head:/tail: cannot be combined with search terms.\n");
        return (2, stdout, stderr);
    }
    if trim.is_some() && scopes.is_empty() {
        stderr.push_str("CVB pull: head:/tail: requires a file: scope.\n");
        return (2, stdout, stderr);
    }

    // `lines:` without search terms prints raw lines; with search terms it
    // restricts where hits may come from.
    let restrict_search = !line_ranges.is_empty() && !query_args.is_empty();

    let query = if !line_ranges.is_empty() && query_args.is_empty() {
        SearchQuery::default()
    } else {
        match parse_query(&query_args) {
            Ok(query) => query,
            Err(error) => {
                stderr.push_str(&format!("{error}\n"));
                return (2, stdout, stderr);
            }
        }
    };

    let files = match tracked_files(dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    let files = match filter_file_scope(files, &scopes, dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    if whole_file_capture {
        let Some(path) = scopes.first().map(String::as_str) else {
            stderr.push_str("CVB pull: internal error: missing whole-file path.\n");
            return (2, stdout, stderr);
        };
        let full_path = if let Some(d) = dir {
            d.join(path)
        } else {
            PathBuf::from(path)
        };
        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(error) => {
                stderr.push_str(&format!(
                    "CVB pull: unable to read {path} as UTF-8: {error}\n"
                ));
                return (2, stdout, stderr);
            }
        };
        let line_count = source.lines().count();
        if line_count == 0 {
            stdout.push_str(&format!("{path}: file is empty.\n"));
            return (0, stdout, stderr);
        }
        // A trim larger than the file simply returns the whole file.
        line_ranges = match trim {
            Some(Trim::Head(n)) => vec![(1, n.min(line_count))],
            Some(Trim::Tail(n)) => vec![(line_count - n.min(line_count) + 1, line_count)],
            None => vec![(1, line_count)],
        };
    }

    // Ranges as they apply to the file: validated, and clamped at its end.
    let mut restriction_note = String::new();
    let mut restriction: Vec<(usize, usize)> = Vec::new();

    if let Some(path) = scopes.first().filter(|_| !line_ranges.is_empty()) {
        let full_path = if let Some(d) = dir {
            d.join(path)
        } else {
            PathBuf::from(path)
        };
        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(error) => {
                stderr.push_str(&format!(
                    "CVB pull: unable to read {path} as UTF-8: {error}\n"
                ));
                return (2, stdout, stderr);
            }
        };
        let lines: Vec<&str> = source.lines().collect();

        enum Out {
            Warning(String),
            Line(usize),
        }
        let mut output: Vec<Out> = Vec::new();

        for &(start, end) in &line_ranges {
            if start == 0 || end == 0 || start > end {
                stderr.push_str(
                    "CVB pull: invalid line range; expected positive START not exceeding END.\n",
                );
                return (2, stdout, stderr);
            }
            if start > lines.len() {
                stderr.push_str(&format!(
                    "CVB pull: line range {start}-{end} starts beyond {path}, which has {} lines.\n",
                    lines.len()
                ));
                return (2, stdout, stderr);
            }
            let actual_end = end.min(lines.len());
            if actual_end < end {
                output.push(Out::Warning(format!(
                    "CVB pull: warning: requested line range {start}-{end} extends past EOF; truncated to {start}-{actual_end} ({path} has {} lines).\n",
                    lines.len()
                )));
            }
            restriction.push((start, actual_end));
            for number in start..=actual_end {
                output.push(Out::Line(number));
            }
        }

        if !restrict_search {
            // head:/tail: trim the combined line output.
            let total = output
                .iter()
                .filter(|item| matches!(item, Out::Line(_)))
                .count();
            let (skip, keep) = match trim {
                Some(Trim::Head(n)) => (0, n.min(total)),
                Some(Trim::Tail(n)) => (total - n.min(total), n.min(total)),
                None => (0, total),
            };
            let mut seen = 0;
            for item in &output {
                match item {
                    Out::Warning(text) => stdout.push_str(text),
                    Out::Line(number) => {
                        if seen >= skip && seen < skip + keep {
                            stdout.push_str(&format!("{path}:{number}: {}\n", lines[number - 1]));
                        }
                        seen += 1;
                    }
                }
            }
            append_patch_reminder(&mut stdout);
            return (0, stdout, stderr);
        }

        restriction_note.push_str(&format!(
            "Restricted to lines {} of {path}: hits must fall in range; units are returned whole and may extend beyond it.\n",
            format_ranges(&restriction)
        ));
        for item in &output {
            if let Out::Warning(text) = item {
                restriction_note.push_str(text);
            }
        }
    }

    let search_terms: Vec<&str> = if !query.alternatives.is_empty() {
        query.alternatives.iter().map(String::as_str).collect()
    } else if restrict_search {
        // Any required term may seed a unit, so a range that holds only the
        // second required term still finds the unit.
        query.required.iter().map(String::as_str).collect()
    } else if let Some(required) = query.required.first() {
        vec![required.as_str()]
    } else {
        vec![""]
    };

    let mut candidate_records = Vec::new();
    let mut search_warnings = Vec::new();
    let mut had_search_error = false;

    for search_term in search_terms {
        let output = match run_search("rg", search_term, &files, dir) {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match run_search("/usr/bin/grep", search_term, &files, dir) {
                    Ok(output) => output,
                    Err(error) => {
                        stderr.push_str(&format!("CVB pull: unable to launch grep: {error}\n"));
                        return (2, stdout, stderr);
                    }
                }
            }
            Err(error) => {
                stderr.push_str(&format!("CVB pull: unable to launch rg: {error}\n"));
                return (2, stdout, stderr);
            }
        };

        let stdout_empty = output.stdout.is_empty();
        match output.status.code() {
            Some(0) => {
                candidate_records.extend(parse_search_records(&output.stdout));
            }
            Some(1) => {}
            _ => {
                had_search_error = true;
                if !stdout_empty {
                    candidate_records.extend(parse_search_records(&output.stdout));
                    search_warnings.push(String::from_utf8_lossy(&output.stderr).into_owned());
                } else {
                    stderr.push_str(&String::from_utf8_lossy(&output.stderr));
                    stderr.push_str("CVB pull: search failed.\n");
                    return (2, stdout, stderr);
                }
            }
        }
    }

    for warning in &search_warnings {
        if !warning.is_empty() {
            stderr.push_str(warning);
        }
    }

    candidate_records.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    candidate_records.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    // lines: restricts the hits (the seeds); operators and unit expansion below
    // still look at the whole unit.
    candidate_records.retain(|(_, line, _)| line_in_ranges(*line, &restriction));

    let mut file_cache: HashMap<String, String> = HashMap::new();
    let mut resolved_units: HashMap<UnitKey, SearchUnit> = HashMap::new();
    let mut ordered_unit_keys: Vec<UnitKey> = Vec::new();

    for (path, line, _) in &candidate_records {
        let source = if let Some(src) = file_cache.get(path) {
            src.clone()
        } else {
            let full_path = if let Some(d) = dir {
                d.join(path)
            } else {
                PathBuf::from(path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => {
                    file_cache.insert(path.clone(), src.clone());
                    src
                }
                Err(error) => {
                    stderr.push_str(&format!(
                        "CVB pull: warning: unable to read {path}: {error}\n"
                    ));
                    continue;
                }
            }
        };

        let unit = resolve_unit(path, *line, &source);
        if !resolved_units.contains_key(&unit.key) {
            ordered_unit_keys.push(unit.key.clone());
            resolved_units.insert(unit.key.clone(), unit);
        }
    }

    ordered_unit_keys.sort();

    let mut matching_units: Vec<MatchingSeed> = Vec::new();
    let mut eliminated_by_exclusion: HashMap<String, usize> = HashMap::new();

    for key in ordered_unit_keys {
        let unit = resolved_units.remove(&key).unwrap();
        let Some(source) = file_cache.get(&unit.key.path) else {
            continue;
        };

        let lines: Vec<&str> = source.lines().collect();
        let start = unit.key.start.max(1);
        let end = unit.key.end.min(lines.len());
        if start > end {
            continue;
        }

        if !query.alternatives.is_empty() {
            let has_alt = query
                .alternatives
                .iter()
                .any(|alt| (start..=end).any(|idx| lines[idx - 1].contains(alt)));
            if !has_alt {
                continue;
            }
        }

        let has_all_req = query
            .required
            .iter()
            .all(|req| (start..=end).any(|idx| lines[idx - 1].contains(req)));
        if !has_all_req {
            continue;
        }

        let mut was_excluded = false;
        for exc in &query.excluded {
            let has_exc = (start..=end).any(|idx| lines[idx - 1].contains(exc));
            if has_exc {
                *eliminated_by_exclusion.entry(exc.clone()).or_insert(0) += 1;
                was_excluded = true;
            }
        }
        if was_excluded {
            continue;
        }

        let mut term_hits = Vec::new();

        for alt in &query.alternatives {
            let hit_lines: Vec<usize> = (start..=end)
                .filter(|idx| lines[idx - 1].contains(alt))
                .collect();
            if !hit_lines.is_empty() {
                term_hits.push((alt.clone(), hit_lines));
            }
        }

        for req in &query.required {
            let hit_lines: Vec<usize> = (start..=end)
                .filter(|idx| lines[idx - 1].contains(req))
                .collect();
            if !hit_lines.is_empty() {
                term_hits.push((format!("+{req}"), hit_lines));
            }
        }

        term_hits.sort_by_key(|(_, hits)| hits.first().copied().unwrap_or(0));

        matching_units.push((unit, term_hits));
    }

    if matching_units.is_empty() {
        if had_search_error {
            stderr.push_str(
                "CVB pull: search failed on one or more terms; results may be incomplete.\n",
            );
            return (2, stdout, stderr);
        }

        stdout.push_str(&format!("{}\n", query.format_echo()));
        stdout.push_str(&restriction_note);
        stdout.push_str("No matches.\n");

        let mut pos_counts = Vec::new();
        for alt in &query.alternatives {
            let count = count_units_for_term(alt, &files, &mut file_cache, dir, &restriction);
            let unit_str = if count == 1 { "unit" } else { "units" };
            pos_counts.push(format!("{alt}: {count} {unit_str}"));
        }
        for req in &query.required {
            let count = count_units_for_term(req, &files, &mut file_cache, dir, &restriction);
            let unit_str = if count == 1 { "unit" } else { "units" };
            pos_counts.push(format!("+{req}: {count} {unit_str}"));
        }

        if !pos_counts.is_empty() {
            stdout.push_str(&format!("  {}\n", pos_counts.join(", ")));
        }

        for exc in &query.excluded {
            let count = eliminated_by_exclusion.get(exc).copied().unwrap_or(0);
            let unit_str = if count == 1 { "unit" } else { "units" };
            stdout.push_str(&format!(
                "  -{exc}: would have removed {count} {unit_str}\n"
            ));
        }

        return (1, stdout, stderr);
    }

    stdout.push_str(&format!("{}\n", query.format_echo()));
    stdout.push_str(&restriction_note);

    if let Some(depth) = cascade_depth
        && depth > 0 {
            let scope_desc = if !scopes.is_empty() {
                format!("initial search in {}, dependencies repo-wide", scopes.join(", "))
            } else {
                "repo-wide".to_string()
            };
            stdout.push_str(&format!(
                "Cascade: depth {depth} | limits {} units, {} chars | scope: {scope_desc}\n",
                cascade::MAX_ADDED_UNITS,
                cascade::MAX_ADDED_CHARS
            ));
        }

    for exc in &query.excluded {
        if let Some(&count) = eliminated_by_exclusion.get(exc)
            && count > 0 {
                let unit_str = if count == 1 { "unit" } else { "units" };
                stdout.push_str(&format!("-{exc}: excluded {count} {unit_str}\n"));
            }
    }

    if list_mode {
        let files_hit = matching_units
            .iter()
            .map(|(unit, _)| unit.key.path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        stdout.push_str(&format!(
            "List: {} unit{} in {} file{}; bodies not shown. Pull one with file:PATH lines:START-END or its terms.\n",
            matching_units.len(),
            if matching_units.len() == 1 { "" } else { "s" },
            files_hit,
            if files_hit == 1 { "" } else { "s" },
        ));
        for (unit, term_hits) in &matching_units {
            let Some(source) = file_cache.get(&unit.key.path) else {
                continue;
            };
            let lines: Vec<&str> = source.lines().collect();
            let (path, start, end) = (&unit.key.path, unit.key.start, unit.key.end);
            let first = lines
                .get(start.saturating_sub(1))
                .map(|line| line.trim())
                .unwrap_or("");
            if unit.label == "Line" {
                stdout.push_str(&format!("  {path}:{start}: {}\n", cut_signature(first)));
            } else {
                let count = end.saturating_sub(start) + 1;
                stdout.push_str(&format!(
                    "  {}: {path}:{start}-{end} ({count} lines) {}\n",
                    unit.label,
                    cut_signature(first)
                ));
            }
            let hits: Vec<String> = term_hits
                .iter()
                .map(|(term, lines)| {
                    let list = lines.iter().map(usize::to_string).collect::<Vec<_>>().join(",");
                    format!("{term}: {list}")
                })
                .collect();
            if !hits.is_empty() {
                stdout.push_str(&format!("    {}\n", hits.join("; ")));
            }
        }
        append_patch_reminder(&mut stdout);
        return (0, stdout, stderr);
    }

    if matching_units.len() >= MANY_UNITS {
        let files_hit = matching_units
            .iter()
            .map(|(unit, _)| unit.key.path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let total_lines: usize = matching_units
            .iter()
            .map(|(unit, _)| unit.key.end.saturating_sub(unit.key.start) + 1)
            .sum();
        stdout.push_str(&format!(
            "Note: {} units match in {} file{} ({} lines in all). Narrow with file:PATH, +TERM or -TERM, or add list: to see only names and line ranges.\n",
            matching_units.len(),
            files_hit,
            if files_hit == 1 { "" } else { "s" },
            total_lines,
        ));
    }

    for (unit, term_hits) in &matching_units {
        if let Some(warning) = &unit.warning {
            stdout.push_str(&format!("CVB pull: warning: {warning}\n"));
        }

        let Some(source) = file_cache.get(&unit.key.path) else {
            continue;
        };
        let lines: Vec<&str> = source.lines().collect();
        let path = &unit.key.path;
        let start = unit.key.start;
        let end = unit.key.end;

        if unit.label == "Line" {
            if start <= lines.len() {
                stdout.push_str(&format!("{path}:{start}: {}\n", lines[start - 1]));
            }
        } else {
            stdout.push_str(&format!("  {}: {path}:{start}-{end}\n", unit.label));
            for (term_display, hits) in term_hits {
                let hit_csv = hits
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                stdout.push_str(&format!("  {term_display}: {hit_csv}\n"));
            }
            for number in start..=end {
                if number <= lines.len() {
                    stdout.push_str(&format!("  {number:>5} | {}\n", lines[number - 1]));
                }
            }
        }
    }

    if let Some(depth) = cascade_depth
        && depth > 0 {
            let cascade_bundle = cascade::run_with_reserved_chars(
                &matching_units,
                dir,
                depth,
                PATCH_REMINDER.chars().count() + 1,
            );
            stdout.push_str(&cascade_bundle);
        }

    append_patch_reminder(&mut stdout);
    (0, stdout, stderr)
}

/// A unit's first line, cut for a listing.
fn cut_signature(text: &str) -> String {
    const MAX: usize = 100;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let cut: String = text.chars().take(MAX).collect();
        format!("{cut}...")
    }
}

fn format_usage() -> &'static str {
    "Usage: cvb pull [file:PATH]... [cascade:N] <search-term>...\n\
           cvb pull file:PATH [head:N|tail:N]\n\
           cvb pull file:PATH lines:LINE|START-END... [head:N|tail:N]\n\
           cvb pull file:PATH lines:LINE|START-END... <search-term>...\n\
           cvb pull [file:PATH]... callers:IDENT\n\
           cvb pull [file:PATH]... list: <search-term>...\n\n\
     Operators:\n\
         foo bar          OR: match either term\n\
         +foo             AND: require term anywhere in the matching unit\n\
         -foo             NOT: exclude any unit containing term\n\
         cascade:N        Expand dependencies up to depth N (0..=5)\n\
         list:            Print matching units as names and line ranges, no bodies\n\
         \"foo bar\"        Match exact phrase\n\
         \"--dry-run\"      Match literal term starting with - or +\n\n\
     Examples:\n\
         cvb pull CRLF BOM\n\
         cvb pull +CRLF +BOM cascade:2\n\
         cvb pull CRLF BOM +split_inclusive -tests\n\
         cvb pull callers:run\n\
         cvb pull file:src/pull.rs lines:10-25\n\
         cvb pull file:a.rs file:b.rs term\n\
         cvb pull file:src/pull.rs tail:40\n\
         cvb pull file:src/pull.rs lines:100-300 foo\n\
         cvb pull list: CRLF BOM\n"
}

fn bash_unit_range(source: &str, hit_line: usize) -> Result<((usize, usize), &'static str), ()> {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return Err(());
    }

    let Some(tree) = parser.parse(source, None) else {
        return Err(());
    };
    let root = tree.root_node();

    if root.has_error() {
        return Err(());
    }

    fn collect_ranges(node: tree_sitter::Node<'_>, ranges: &mut Vec<(usize, usize)>) {
        if node.kind() == "function_definition" {
            ranges.push((node.start_position().row + 1, node.end_position().row + 1));
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_ranges(child, ranges);
        }
    }

    let mut ranges = Vec::new();
    collect_ranges(root, &mut ranges);

    let matching = ranges
        .into_iter()
        .filter(|(start, end)| *start <= hit_line && hit_line <= *end)
        .min_by_key(|(start, end)| end - start);

    Ok(matching
        .map(|r| (r, "Bash function"))
        .unwrap_or(((hit_line, hit_line), "Line")))
}

#[allow(dead_code)]
pub fn bash_function_range(source: &str, hit_line: usize) -> Option<(usize, usize)> {
    bash_unit_range(source, hit_line)
        .ok()
        .and_then(|(range, label)| {
            if label == "Bash function" {
                Some(range)
            } else {
                None
            }
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustItemKind {
    Function,
    Struct,
    Enum,
    Impl,
    Trait,
}

impl RustItemKind {
    pub fn label(&self) -> &'static str {
        match self {
            RustItemKind::Function => "Rust function",
            RustItemKind::Struct => "Rust struct",
            RustItemKind::Enum => "Rust enum",
            RustItemKind::Impl => "Rust impl",
            RustItemKind::Trait => "Rust trait",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RustItem {
    pub kind: RustItemKind,
    pub name: Option<String>,
    pub owner: Option<String>,
    pub start: usize,
    pub end: usize,
    pub in_test_mod: bool,
}

impl RustItem {
    pub fn label(&self) -> &'static str {
        self.kind.label()
    }
}

fn meta_is_test(meta: &syn::meta::ParseNestedMeta) -> bool {
    if meta.path.is_ident("test") {
        return true;
    }
    if meta.path.is_ident("all") || meta.path.is_ident("any") {
        let mut found = false;
        let _ = meta.parse_nested_meta(|inner| {
            if meta_is_test(&inner) {
                found = true;
            }
            Ok(())
        });
        return found;
    }
    false
}

fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path().is_ident("test") {
            true
        } else if attr.path().is_ident("cfg") {
            let mut is_test = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta_is_test(&meta) {
                    is_test = true;
                }
                Ok(())
            });
            is_test
        } else {
            false
        }
    })
}

fn type_ident_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string()),
        _ => None,
    }
}

pub fn rust_items(file: &syn::File) -> Vec<RustItem> {
    let mut items = Vec::new();

    fn walk_items(syn_items: &[syn::Item], in_test: bool, items: &mut Vec<RustItem>) {
        use syn::spanned::Spanned;

        for item in syn_items {
            match item {
                syn::Item::Fn(function) => {
                    let span = function.span();
                    let is_test = in_test || has_test_attr(&function.attrs);
                    items.push(RustItem {
                        kind: RustItemKind::Function,
                        name: Some(function.sig.ident.to_string()),
                        owner: None,
                        start: span.start().line,
                        end: span.end().line,
                        in_test_mod: is_test,
                    });
                }
                syn::Item::Struct(s) => {
                    let span = s.span();
                    let is_test = in_test || has_test_attr(&s.attrs);
                    items.push(RustItem {
                        kind: RustItemKind::Struct,
                        name: Some(s.ident.to_string()),
                        owner: None,
                        start: span.start().line,
                        end: span.end().line,
                        in_test_mod: is_test,
                    });
                }
                syn::Item::Enum(e) => {
                    let span = e.span();
                    let is_test = in_test || has_test_attr(&e.attrs);
                    items.push(RustItem {
                        kind: RustItemKind::Enum,
                        name: Some(e.ident.to_string()),
                        owner: None,
                        start: span.start().line,
                        end: span.end().line,
                        in_test_mod: is_test,
                    });
                }
                syn::Item::Impl(implementation) => {
                    let span = implementation.span();
                    let is_test = in_test || has_test_attr(&implementation.attrs);
                    let owner_name = type_ident_name(&implementation.self_ty);
                    items.push(RustItem {
                        kind: RustItemKind::Impl,
                        name: owner_name.clone(),
                        owner: None,
                        start: span.start().line,
                        end: span.end().line,
                        in_test_mod: is_test,
                    });
                    for impl_item in &implementation.items {
                        if let syn::ImplItem::Fn(function) = impl_item {
                            let f_span = function.span();
                            let fn_test = is_test || has_test_attr(&function.attrs);
                            items.push(RustItem {
                                kind: RustItemKind::Function,
                                name: Some(function.sig.ident.to_string()),
                                owner: owner_name.clone(),
                                start: f_span.start().line,
                                end: f_span.end().line,
                                in_test_mod: fn_test,
                            });
                        }
                    }
                }
                syn::Item::Trait(trait_item) => {
                    let span = trait_item.span();
                    let is_test = in_test || has_test_attr(&trait_item.attrs);
                    let trait_name = trait_item.ident.to_string();
                    items.push(RustItem {
                        kind: RustItemKind::Trait,
                        name: Some(trait_name.clone()),
                        owner: None,
                        start: span.start().line,
                        end: span.end().line,
                        in_test_mod: is_test,
                    });
                    for tr_item in &trait_item.items {
                        if let syn::TraitItem::Fn(function) = tr_item {
                            let f_span = function.span();
                            let fn_test = is_test || has_test_attr(&function.attrs);
                            items.push(RustItem {
                                kind: RustItemKind::Function,
                                name: Some(function.sig.ident.to_string()),
                                owner: Some(trait_name.clone()),
                                start: f_span.start().line,
                                end: f_span.end().line,
                                in_test_mod: fn_test,
                            });
                        }
                    }
                }
                syn::Item::Mod(module) => {
                    let is_test = in_test
                        || has_test_attr(&module.attrs)
                        || module.ident == "tests"
                        || module.ident == "test";
                    if let Some((_, nested_items)) = &module.content {
                        walk_items(nested_items, is_test, items);
                    }
                }
                _ => {}
            }
        }
    }

    walk_items(&file.items, false, &mut items);
    items
}

fn rust_unit_range(source: &str, hit_line: usize) -> Result<((usize, usize), &'static str), ()> {
    let Ok(file) = syn::parse_file(source) else {
        return Err(());
    };

    let items = rust_items(&file);

    let matching = items
        .into_iter()
        .filter(|item| item.start <= hit_line && hit_line <= item.end)
        .min_by_key(|item| item.end - item.start);

    Ok(matching
        .map(|item| ((item.start, item.end), item.label()))
        .unwrap_or(((hit_line, hit_line), "Line")))
}

#[allow(dead_code)]
pub fn rust_function_range(source: &str, hit_line: usize) -> Option<(usize, usize)> {
    rust_unit_range(source, hit_line)
        .ok()
        .and_then(|(range, label)| {
            if label == "Rust function" || label == "Rust struct" || label == "Rust enum" {
                Some(range)
            } else {
                None
            }
        })
}

#[derive(Debug, PartialEq, Eq)]
pub enum RustCallKind {
    Function,
    Method,
    MacroArgument,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RustCallerMatch {
    pub line: usize,
    pub enclosing_function: Option<String>,
    pub kind: RustCallKind,
}

pub fn is_call_invocation(trees: &[proc_macro2::TokenTree], i: usize) -> bool {
    use proc_macro2::{Delimiter, TokenTree};

    if let Some(next) = trees.get(i + 1) {
        if let TokenTree::Group(group) = next
            && group.delimiter() == Delimiter::Parenthesis {
                return true;
            }

        if let TokenTree::Punct(p1) = next
            && p1.as_char() == ':'
                && let (Some(TokenTree::Punct(p2)), Some(TokenTree::Punct(p3))) =
                    (trees.get(i + 2), trees.get(i + 3))
                    && p2.as_char() == ':' && p3.as_char() == '<' {
                        let mut depth = 0;
                        let mut j = i + 3;
                        while j < trees.len() {
                            if let TokenTree::Punct(p) = &trees[j] {
                                if p.as_char() == '<' {
                                    depth += 1;
                                } else if p.as_char() == '>' {
                                    depth -= 1;
                                    if depth == 0 {
                                        if let Some(TokenTree::Group(call_group)) = trees.get(j + 1)
                                            && call_group.delimiter() == Delimiter::Parenthesis {
                                                return true;
                                            }
                                        break;
                                    }
                                }
                            }
                            j += 1;
                        }
                    }
    }
    false
}

pub fn rust_callers(source: &str, target: &str) -> Option<Vec<RustCallerMatch>> {
    use proc_macro2::TokenTree;
    use syn::spanned::Spanned;
    use syn::visit::{self, Visit};

    struct CallerVisitor<'a> {
        target: &'a str,
        enclosing_function: Option<String>,
        matches: Vec<RustCallerMatch>,
    }

    impl CallerVisitor<'_> {
        fn with_function(&mut self, name: String, visit_body: impl FnOnce(&mut Self)) {
            let previous = self.enclosing_function.replace(name);
            visit_body(self);
            self.enclosing_function = previous;
        }

        fn scan_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            let trees: Vec<TokenTree> = tokens.into_iter().collect();
            for i in 0..trees.len() {
                match &trees[i] {
                    TokenTree::Ident(ident) => {
                        if ident == self.target && is_call_invocation(&trees, i) {
                            self.matches.push(RustCallerMatch {
                                line: ident.span().start().line,
                                enclosing_function: self.enclosing_function.clone(),
                                kind: RustCallKind::MacroArgument,
                            });
                        }
                    }
                    TokenTree::Group(group) => {
                        self.scan_tokens(group.stream());
                    }
                    _ => {}
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for CallerVisitor<'_> {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            let name = node.sig.ident.to_string();
            self.with_function(name, |visitor| {
                visit::visit_item_fn(visitor, node);
            });
        }

        fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
            let name = node.sig.ident.to_string();
            self.with_function(name, |visitor| {
                visit::visit_impl_item_fn(visitor, node);
            });
        }

        fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
            let name = node.sig.ident.to_string();
            self.with_function(name, |visitor| {
                visit::visit_trait_item_fn(visitor, node);
            });
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = node.func.as_ref()
                && path
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == self.target)
                {
                    self.matches.push(RustCallerMatch {
                        line: node.span().start().line,
                        enclosing_function: self.enclosing_function.clone(),
                        kind: RustCallKind::Function,
                    });
                }

            visit::visit_expr_call(self, node);
        }

        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if node.method == self.target {
                self.matches.push(RustCallerMatch {
                    line: node.span().start().line,
                    enclosing_function: self.enclosing_function.clone(),
                    kind: RustCallKind::Method,
                });
            }

            visit::visit_expr_method_call(self, node);
        }

        fn visit_macro(&mut self, node: &'ast syn::Macro) {
            self.scan_tokens(node.tokens.clone());
            visit::visit_macro(self, node);
        }
    }

    let file = syn::parse_file(source).ok()?;
    let mut visitor = CallerVisitor {
        target,
        enclosing_function: None,
        matches: Vec::new(),
    };

    visitor.visit_file(&file);
    visitor.matches.sort_by_key(|item| item.line);
    Some(visitor.matches)
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let line = &line[indent..];

    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }

    match line.as_bytes().get(hashes) {
        None | Some(b' ') | Some(b'\t') => Some(hashes),
        _ => None,
    }
}

pub fn markdown_section_range(source: &str, match_line: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    if match_line == 0 || match_line > lines.len() {
        return None;
    }

    let mut headings = Vec::new();
    let mut fence: Option<(u8, usize)> = None;

    for (index, line) in lines.iter().enumerate() {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        if indent > 3 {
            if fence.is_none()
                && let Some(level) = markdown_heading_level(line) {
                    headings.push((index + 1, level));
                }
            continue;
        }

        let content = &line[indent..];
        let bytes = content.as_bytes();
        let marker = bytes.first().copied();
        let run = marker.map_or(0, |marker| {
            bytes.iter().take_while(|byte| **byte == marker).count()
        });

        if let Some((fence_marker, fence_len)) = fence {
            if marker == Some(fence_marker)
                && run >= fence_len
                && bytes[run..].iter().all(|byte| matches!(byte, b' ' | b'\t'))
            {
                fence = None;
            }
            continue;
        }

        if matches!(marker, Some(b'`' | b'~')) && run >= 3 {
            fence = Some((marker.unwrap(), run));
            continue;
        }

        if let Some(level) = markdown_heading_level(line) {
            headings.push((index + 1, level));
        }
    }

    let preceding = headings
        .iter()
        .copied()
        .take_while(|(line, _)| *line <= match_line)
        .last();

    let (start, level) = match preceding {
        Some((start, level)) => (start, Some(level)),
        None => (1, None),
    };

    let end = headings
        .iter()
        .copied()
        .find(|(line, heading_level)| {
            *line > match_line
                && match level {
                    Some(level) => *heading_level <= level,
                    None => true,
                }
        })
        .map(|(line, _)| line - 1)
        .unwrap_or(lines.len());

    (start <= end).then_some((start, end))
}

fn markdown_unit_range(source: &str, hit_line: usize) -> ((usize, usize), &'static str) {
    markdown_section_range(source, hit_line)
        .map(|r| (r, "Markdown section"))
        .unwrap_or(((hit_line, hit_line), "Line"))
}

pub fn expand_matches(matches: &str) -> String {
    use std::fs;

    let mut output = String::new();
    let mut shown_units = HashSet::new();

    for matched_line in matches.lines() {
        output.push_str(matched_line);
        output.push('\n');

        let mut fields = matched_line.splitn(3, ':');
        let Some(path) = fields.next() else {
            continue;
        };
        let Some(line) = fields.next().and_then(|value| value.parse::<usize>().ok()) else {
            continue;
        };

        let is_rust = path.ends_with(".rs");
        let is_bash = path.ends_with(".bash") || path.ends_with(".sh");
        let is_markdown = path.ends_with(".md") || path.ends_with(".markdown");
        if !is_rust && !is_bash && !is_markdown {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };

        let unit_opt = if is_rust {
            rust_unit_range(&source, line).ok()
        } else if is_bash {
            bash_unit_range(&source, line).ok()
        } else {
            Some(markdown_unit_range(&source, line))
        };

        let Some(((start, end), label)) = unit_opt else {
            continue;
        };

        if label == "Line" {
            continue;
        }

        let key = (path.to_string(), start, end);
        if !shown_units.insert(key) {
            continue;
        }

        output.push_str(&format!("  {label}: {path}:{start}-{end}\n"));

        for (index, source_line) in source.lines().enumerate() {
            let number = index + 1;
            if start <= number && number <= end {
                output.push_str(&format!("  {number:>5} | {source_line}\n"));
            }
        }
    }

    output
}

#[allow(dead_code)]
pub fn expand_rust_matches(matches: &str) -> String {
    expand_matches(matches)
}

#[cfg(test)]
mod pull_tests {
    use super::*;

    #[test]
    fn cascade_zero_is_byte_identical_to_plain_pull() {
        let dir = std::env::temp_dir().join(format!("cvb-test-cascade0-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = "fn process() {\n    let crlf = 1;\n    let split_inclusive = 2;\n}\n";
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let (status_plain, stdout_plain, stderr_plain) =
            run_capture(&["+crlf".into(), "+split_inclusive".into()], Some(&dir));
        let (status_c0, stdout_c0, stderr_c0) = run_capture(
            &[
                "+crlf".into(),
                "+split_inclusive".into(),
                "cascade:0".into(),
            ],
            Some(&dir),
        );

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(status_plain, status_c0, "exit status must match");
        assert_eq!(
            stdout_plain, stdout_c0,
            "stdout must be byte-identical between plain pull and cascade:0"
        );
        assert_eq!(
            stderr_plain, stderr_c0,
            "stderr must be byte-identical between plain pull and cascade:0"
        );
    }

    #[test]
    fn file_scope_expands_dependencies_repo_wide() {
        let dir = std::env::temp_dir().join(format!("cvb-test-filescope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn caller() {\n    helper_fn();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/helper.rs"),
            "pub fn helper_fn() -> usize {\n    42\n}\n",
        )
        .unwrap();
        run_cmd(&["git", "add", "src/lib.rs", "src/helper.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let (status, stdout, stderr) = run_capture(
            &[
                "file:src/lib.rs".into(),
                "caller".into(),
                "cascade:1".into(),
            ],
            Some(&dir),
        );

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(status, 0);
        assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        assert!(
            stdout.contains("Rust function: src/lib.rs:1-3"),
            "missing seed in stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("Rust function: src/helper.rs:1-3"),
            "missing cascaded dependency outside file scope:\n{stdout}"
        );
        assert!(stdout.contains("callee of caller (call at :2)"));
    }

    #[test]
    fn callers_exit_status_is_success_when_matches_are_found() {
        assert_eq!(callers_exit_status(true, false), 0);
        assert_eq!(callers_exit_status(true, true), 0);
    }

    #[test]
    fn callers_exit_status_distinguishes_complete_and_incomplete_no_match() {
        assert_eq!(callers_exit_status(false, false), 1);
        assert_eq!(callers_exit_status(false, true), 2);
    }

    #[test]
    fn rust_callers_finds_unqualified_and_qualified_function_calls() {
        let source = r#"fn caller() {
    run();
    module::run();
}
"#;

        let matches = rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[0].kind, RustCallKind::Function);
        assert_eq!(matches[1].line, 3);
        assert_eq!(matches[1].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[1].kind, RustCallKind::Function);
    }

    #[test]
    fn rust_callers_finds_method_call_candidates() {
        let source = r#"fn caller(value: Example) {
    value.run();
}
"#;

        let matches = rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[0].kind, RustCallKind::Method);
    }

    #[test]
    fn rust_callers_finds_calls_inside_macros() {
        let source = r#"fn caller() {
    assert_eq!(run(), 1);
    println!("{}", run());
}
"#;

        let matches = rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[0].kind, RustCallKind::MacroArgument);
        assert_eq!(matches[1].line, 3);
        assert_eq!(matches[1].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[1].kind, RustCallKind::MacroArgument);
    }

    #[test]
    fn rust_callers_finds_generic_calls_inside_macros() {
        let source = r#"fn caller() {
    assert_eq!(run::<u32>(1), 2);
    assert_eq!(run::<Option<String>>(), None);
}
"#;

        let matches = rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].kind, RustCallKind::MacroArgument);
        assert_eq!(matches[1].line, 3);
        assert_eq!(matches[1].kind, RustCallKind::MacroArgument);
    }

    #[test]
    fn rust_callers_ignores_non_call_identifiers_inside_macros() {
        let source = r#"fn caller() {
    assert_eq!(run, expected);
    let x = vec![run];
    assert_eq!(run::<u32>, not_called);
}
"#;

        let matches = rust_callers(source, "run").unwrap();
        assert!(
            matches.is_empty(),
            "identifiers passed as values should not be reported as calls"
        );
    }

    #[test]
    fn rust_callers_ignores_mentions_in_comments_and_strings() {
        let source = r#"fn caller() {
    // run();
    let text = "run()";
}
"#;

        assert!(rust_callers(source, "run").unwrap().is_empty());
    }

    #[test]
    fn rust_callers_rejects_unparseable_source() {
        assert!(rust_callers("fn broken( {", "run").is_none());
    }

    #[test]
    fn extract_callers_scope_accepts_target_with_optional_file_scope() {
        assert_eq!(
            extract_callers_scope(&["callers:run".to_string()]).unwrap(),
            Some(("run".to_string(), Vec::new()))
        );
        assert_eq!(
            extract_callers_scope(&["file:src/pull.rs".to_string(), "callers:run".to_string(),])
                .unwrap(),
            Some(("run".to_string(), vec!["src/pull.rs".to_string()]))
        );
    }

    #[test]
    fn callers_accept_names_and_files_of_every_supported_language() {
        for target in ["run", "_run", "run2", "do-it", "$el"] {
            let args = [format!("callers:{target}")];
            assert!(extract_callers_scope(&args).unwrap().is_some(), "{target}");
        }
        for file in ["a.rs", "a.py", "a.go", "a.sh", "a.tsx", "a.java", "a.lua", "a.zig", "a.cpp", "a.js"] {
            let args = [format!("file:{file}"), "callers:run".to_string()];
            assert!(extract_callers_scope(&args).is_ok(), "{file}");
        }
    }

    #[test]
    fn extract_callers_scope_rejects_invalid_combinations() {
        for args in [
            vec!["callers:".to_string()],
            vec!["callers:run".to_string(), "callers:other".to_string()],
            vec!["callers:run".to_string(), "lines:1".to_string()],
            vec!["callers:run".to_string(), "run".to_string()],
            vec![
                "file:src/pull.rs".to_string(),
                "callers:run".to_string(),
                "file:manual.md".to_string(),
            ],
            vec!["file:manual.md".to_string(), "callers:run".to_string()],
            vec!["callers:module::run".to_string()],
            vec!["callers:-run".to_string()],
            vec!["callers:a b".to_string()],
            vec!["file:notes.txt".to_string(), "callers:run".to_string()],
            vec!["-callers:run".to_string()],
        ] {
            assert!(
                extract_callers_scope(&args).is_err(),
                "expected selector combination to be rejected: {args:?}"
            );
        }
    }

    #[test]
    fn bash_function_range_finds_multiline_function_with_nested_control_flow() {
        let source = "target ()\n{\n    if true; then\n        echo nested\n    fi\n}\n";

        assert_eq!(bash_function_range(source, 4), Some((1, 6)));
    }

    #[test]
    fn bash_function_range_ignores_lines_outside_functions() {
        let source = "before() { echo before; }\n\ntarget() {\n    echo inside\n}\n";

        assert_eq!(bash_function_range(source, 2), None);
        assert_eq!(bash_function_range(source, 4), Some((3, 5)));
    }

    #[test]
    fn bash_function_range_rejects_unparseable_source() {
        assert_eq!(bash_function_range("target() { if then", 1), None);
    }

    #[test]
    fn rust_function_range_finds_multiline_function() {
        let source = r#"fn before() {}

fn target(
    first: &str,
    second: usize,
) -> bool {
    first.len() == second
}

fn after() {}
"#;

        assert_eq!(rust_function_range(source, 5), Some((3, 8)));
    }

    #[test]
    fn rust_function_range_finds_multiline_struct() {
        let source = r#"struct CaptureState {
    capturing: bool,
    captured: Vec<u8>,
}
"#;

        assert_eq!(rust_function_range(source, 3), Some((1, 4)));
    }

    #[test]
    fn rust_function_range_finds_impl_method() {
        let source = r#"struct Example;

impl Example {
    fn target(&self) {
        let value = 42;
        assert_eq!(value, 42);
    }
}
"#;

        assert_eq!(rust_function_range(source, 5), Some((4, 7)));
    }

    #[test]
    fn rust_function_range_finds_function_in_inline_module() {
        let source = r#"mod nested {
    fn target() {
        let value = 42;
    }
}
"#;

        assert_eq!(rust_function_range(source, 3), Some((2, 4)));
    }

    #[test]
    fn rust_function_range_returns_none_outside_functions() {
        let source = r#"const VALUE: usize = 42;

fn target() {
    let value = VALUE;
}
"#;

        assert_eq!(rust_function_range(source, 1), None);
    }

    #[test]
    fn rust_function_range_rejects_unparseable_source() {
        assert_eq!(rust_function_range("fn broken( {", 1), None);
    }

    #[test]
    fn expansion_keeps_struct_declaration_separate_from_impl() {
        let directory =
            std::env::temp_dir().join(format!("cvb-pull-struct-impl-sep-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();

        let file = directory.join("example.rs");
        let source = "struct Example {\n    value: usize,\n}\n\nimpl Example {\n    fn get(&self) -> usize {\n        self.value\n    }\n}\n";
        std::fs::write(&file, source).unwrap();

        let matches = format!("{}:1:struct Example {{", file.display());
        let output = expand_matches(&matches);

        assert!(
            output.contains("Rust struct: ") && output.contains("1 | struct Example {"),
            "missing struct expansion:\n{output}"
        );
        assert!(
            !output.contains("Rust impl: "),
            "struct and impl should be separate units:\n{output}"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn filter_file_scope_matches_exact_tracked_path() {
        let files = vec![
            OsString::from("src/pull.rs"),
            OsString::from("src/pull_tests.rs"),
            OsString::from("manual.md"),
        ];

        let filtered = filter_file_scope(files, &["src/pull.rs".into()], None).unwrap();
        assert_eq!(filtered, vec![OsString::from("src/pull.rs")]);
    }

    #[test]
    fn filter_file_scope_normalizes_relative_dot_prefix() {
        let files = vec![OsString::from("src/pull.rs")];
        let filtered = filter_file_scope(files, &["./src/pull.rs".into()], None).unwrap();
        assert_eq!(filtered, vec![OsString::from("src/pull.rs")]);
    }

    #[test]
    fn filter_file_scope_without_scope_preserves_all_files() {
        let files = vec![OsString::from("src/pull.rs"), OsString::from("manual.md")];
        assert_eq!(filter_file_scope(files.clone(), &[], None).unwrap(), files);
    }

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cvb-pull-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(dir.join("tracked.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("gone.rs"), "fn g() {}\n").unwrap();
        git(&["add", ".gitignore", "tracked.rs", "gone.rs"]);
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        std::fs::write(dir.join("fresh.rs"), "fn b() {}\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/out.rs"), "fn c() {}\n").unwrap();
        std::fs::write(dir.join("debug.log"), "x\n").unwrap();
        dir
    }

    #[test]
    fn list_prints_names_and_line_ranges_without_bodies() {
        let dir = scratch_repo("list");
        std::fs::write(
            dir.join("tool.py"),
            "import os\n\ndef compute():\n    secret_body_line = 8\n    return 8\n\ndef other():\n    return compute()\n",
        )
        .unwrap();
        let (code, out, err) = run_capture(&["list:".into(), "compute".into()], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("List: 2 units in 1 file; bodies not shown."), "{out}");
        assert!(out.contains("  Python function: tool.py:3-5 (3 lines) def compute():"), "{out}");
        assert!(out.contains("  Python function: tool.py:7-8 (2 lines) def other():"), "{out}");
        assert!(out.contains("compute: 3"), "{out}");
        assert!(!out.contains("secret_body_line"), "{out}");
        assert!(!out.contains("return 8"), "{out}");
    }

    #[test]
    fn list_needs_terms_and_refuses_incompatible_directives() {
        let dir = scratch_repo("listerr");
        for (args, text) in [
            (vec!["list:", "file:tool.py"], "needs search terms"),
            (vec!["list:"], "needs search terms"),
            (vec!["list:", "x", "cascade:1"], "cascade:"),
            (vec!["list:", "x", "head:3", "file:a"], "head:/tail:"),
            (vec!["list:", "x", "callers:y"], "callers: or lines:"),
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            let (code, _, err) = run_capture(&args, Some(&dir));
            assert_eq!(code, 2, "{args:?}: {err}");
            assert!(err.contains(text), "{args:?}: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_matching_units_get_a_note_that_says_how_to_narrow() {
        let dir = scratch_repo("many");
        let mut source = String::new();
        for n in 0..12 {
            source.push_str(&format!("def f{n}():\n    return needle_{n} + needle\n\n"));
        }
        std::fs::write(dir.join("many.py"), &source).unwrap();
        let (code, out, err) = run_capture(&["needle".into()], Some(&dir));
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Note: 12 units match in 1 file (24 lines in all). Narrow with file:PATH, +TERM or -TERM, or add list:"), "{out}");
        // A few matches stay quiet.
        std::fs::write(dir.join("few.py"), "def only():\n    return few_one\n").unwrap();
        let (_, quiet, _) = run_capture(&["few_one".into()], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!quiet.contains("Note:"), "{quiet}");
    }

    #[test]
    fn a_search_finds_units_in_other_languages_and_in_untracked_files() {
        let dir = scratch_repo("langs");
        std::fs::write(
            dir.join("fresh.zig"),
            "const std = @import(\"std\");\npub fn compute() u32 {\n    return 7;\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("tool.py"), "import os\n\ndef compute():\n    return 8\n\nX = 1\n").unwrap();
        std::fs::write(dir.join("lib.c"), "int compute(void) {\n  return 9;\n}\n").unwrap();
        let (code, out, err) = run_capture(&["compute".into()], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Zig function: fresh.zig:2-4"), "{out}");
        assert!(out.contains("Python function: tool.py:3-4"), "{out}");
        assert!(out.contains("C function: lib.c:1-3"), "{out}");
        // The whole unit is printed, not only the matching line.
        assert!(out.contains("return 7;") && out.contains("return 8") && out.contains("return 9;"), "{out}");
    }

    #[test]
    fn callers_scans_every_supported_language() {
        let dir = scratch_repo("callers-langs");
        std::fs::write(dir.join("a.py"), "def f():\n    run()\n").unwrap();
        std::fs::write(dir.join("b.go"), "package p\n\nfunc g() {\n\ts.run()\n}\n").unwrap();
        std::fs::write(dir.join("c.sh"), "h() {\n  run now\n}\n").unwrap();
        std::fs::write(dir.join("d.rs"), "fn k() { run(); }\n").unwrap();
        let (code, out, err) = run_capture(&["callers:run".into()], Some(&dir));
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("a.py:2: function call to run inside f"), "{out}");
        assert!(out.contains("b.go:4: method-call candidate to run inside g"), "{out}");
        assert!(out.contains("c.sh:2: function call to run inside h"), "{out}");
        assert!(out.contains("d.rs:1: function call to run inside k"), "{out}");

        let (code, out, _) = run_capture(&["file:a.py".into(), "callers:run".into()], Some(&dir));
        assert_eq!(code, 0);
        assert!(out.contains("a.py:2") && !out.contains("b.go"), "{out}");

        let (code, out, _) = run_capture(&["callers:nothing_calls_this".into()], Some(&dir));
        assert_eq!(code, 1);
        assert!(out.contains("No callers found for: nothing_calls_this"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn callers_in_a_broken_file_are_found_but_an_empty_result_is_not_trusted() {
        let dir = scratch_repo("callers-broken");
        std::fs::write(dir.join("a.py"), "def good():\n    run()\n\ndef bad(:\n    x = (\n").unwrap();
        let (code, out, err) = run_capture(&["callers:run".into()], Some(&dir));
        assert_eq!(code, 0, "{out}{err}");
        assert!(out.contains("a.py:2: function call to run inside good"), "{out}");
        assert!(err.contains("a.py has syntax errors"), "{err}");

        let (code, _, err) = run_capture(&["callers:absent".into()], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("no reliable no-callers conclusion"), "{err}");
    }

    #[test]
    fn a_broken_file_in_another_language_falls_back_to_lines_with_a_warning() {
        let dir = scratch_repo("broken");
        std::fs::write(dir.join("bad.py"), "def f(:\n    needle = (\n").unwrap();
        let (code, out, err) = run_capture(&["needle".into()], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("skipping unparseable file bad.py") || out.contains("skipping unparseable file bad.py"), "{out}{err}");
        assert!(out.contains("needle"), "{out}");
    }

    #[test]
    fn untracked_files_that_are_not_ignored_are_listed() {
        let dir = scratch_repo("list");
        let mut names: Vec<String> = tracked_files(Some(&dir))
            .unwrap()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        names.sort();
        let _ = std::fs::remove_dir_all(&dir);
        // Tracked and fresh files are in; ignored files and a deleted file are not.
        assert_eq!(names, vec![".gitignore", "fresh.rs", "tracked.rs"]);
    }

    #[test]
    fn a_file_scope_can_name_an_untracked_file() {
        let dir = scratch_repo("scope");
        let files = tracked_files(Some(&dir)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let matching = filter_file_scope(files, &["fresh.rs".into()], None).unwrap();
        assert_eq!(matching, vec![OsString::from("fresh.rs")]);
    }

    #[test]
    fn filter_file_scope_rejects_untracked_path() {
        let files = vec![OsString::from("src/pull.rs")];
        let error = filter_file_scope(files, &["manual.md".into()], None).unwrap_err();
        assert!(error.contains("no searchable file"));
        assert!(error.contains("manual.md"));
    }

    #[test]
    fn read_line_range_returns_requested_lines_with_original_numbers() {
        let path = std::env::temp_dir().join(format!("cvb-pull-lines-{}.txt", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let path_text = path.to_string_lossy();

        let single = read_line_range(&path_text, 2, 2).unwrap();
        assert_eq!(single, format!("{path_text}:2: beta\n"));

        let multiple = read_line_range(&path_text, 1, 3).unwrap();
        assert_eq!(
            multiple,
            format!("{path_text}:1: alpha\n{path_text}:2: beta\n{path_text}:3: gamma\n")
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_line_range_rejects_invalid_or_out_of_bounds_ranges() {
        let path =
            std::env::temp_dir().join(format!("cvb-pull-lines-bounds-{}.txt", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let path_text = path.to_string_lossy();

        assert!(read_line_range(&path_text, 0, 1).is_err());
        assert!(read_line_range(&path_text, 2, 1).is_err());
        assert!(read_line_range(&path_text, 1, 3).is_err());

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn parse_line_range_accepts_valid_inclusive_ranges() {
        assert_eq!(parse_line_range("155").unwrap(), (155, 155));
        assert_eq!(parse_line_range("1").unwrap(), (1, 1));
        assert_eq!(parse_line_range("155-215").unwrap(), (155, 215));
        assert_eq!(parse_line_range("1-1").unwrap(), (1, 1));
    }

    #[test]
    fn parse_line_range_rejects_invalid_ranges() {
        for range in ["", "0-10", "1-0", "20-10", "abc-20", "1-", "-10", "1-2-3"] {
            assert!(parse_line_range(range).is_err(), "accepted: {range}");
        }
    }

    #[test]
    fn extract_file_scope_preserves_query_operators() {
        let args = vec![
            "file:src/pull.rs".into(),
            "protocol".into(),
            "+line-endings".into(),
            "-example".into(),
        ];

        let (scope, lines, query) = extract_file_scope(&args).unwrap();
        assert_eq!(scope, vec!["src/pull.rs"]);
        assert!(lines.is_empty());
        assert_eq!(query, vec!["protocol", "+line-endings", "-example"]);
    }

    #[test]
    fn extract_file_scope_accepts_several_scopes_and_dedupes() {
        let (scopes, lines, query) = extract_file_scope(&[
            "file:src/b.rs".into(),
            "file:src/a.rs".into(),
            "file:src/b.rs".into(),
            "needle".into(),
        ])
        .unwrap();
        assert_eq!(scopes, vec!["src/b.rs", "src/a.rs"]);
        assert!(lines.is_empty());
        assert_eq!(query, vec!["needle"]);
    }

    #[test]
    fn extract_file_scope_rejects_empty_path_and_scope_without_terms() {
        assert!(extract_file_scope(&["file:".into(), "needle".into()]).is_err());
        assert!(extract_file_scope(&["file:src/pull.rs".into()]).is_err());
        let error = extract_file_scope(&["file:a.rs".into(), "file:b.rs".into()]).unwrap_err();
        assert!(error.contains("several file: scopes"), "{error}");
    }

    #[test]
    fn extract_file_scope_lines_need_exactly_one_scope() {
        let error = extract_file_scope(&[
            "file:a.rs".into(),
            "file:b.rs".into(),
            "lines:1-2".into(),
        ])
        .unwrap_err();
        assert_eq!(error, "CVB pull: lines: requires exactly one file: scope.");
        assert!(extract_file_scope(&["file:a.rs".into(), "file:a.rs".into(), "lines:1".into()]).is_ok());
    }

    #[test]
    fn extract_file_scope_rejects_negated_selectors() {
        for arg in [
            "-file:src/pull.rs",
            "-lines:5",
            "-callers:run",
            "-cascade:2",
        ] {
            assert!(
                extract_file_scope(&[arg.to_string()]).is_err(),
                "should reject negated selector {arg}"
            );
        }
    }

    #[test]
    fn extract_file_scope_parses_line_ranges() {
        let args = vec!["file:src/pull.rs".into(), "lines:96-110".into()];
        let (scope, lines, query) = extract_file_scope(&args).unwrap();
        assert_eq!(scope, vec!["src/pull.rs"]);
        assert_eq!(lines, vec![(96, 110)]);
        assert!(query.is_empty());

        let single_line = vec!["file:manual.md".into(), "lines:155".into()];
        let (scope, lines, query) = extract_file_scope(&single_line).unwrap();
        assert_eq!(scope, vec!["manual.md"]);
        assert_eq!(lines, vec![(155, 155)]);
        assert!(query.is_empty());

        let multiple = extract_file_scope(&[
            "file:src/pull.rs".into(),
            "lines:96-110".into(),
            "lines:5-8".into(),
            "lines:7".into(),
        ])
        .unwrap();
        assert_eq!(multiple.0, vec!["src/pull.rs"]);
        assert_eq!(multiple.1, vec![(96, 110), (5, 8), (7, 7)]);
        assert!(multiple.2.is_empty());

        assert!(extract_file_scope(&["lines:1-3".into()]).is_err());
    }

    #[test]
    fn query_bare_terms_use_or_semantics() {
        let query = parse_query(&["CRLF".into(), "BOM".into()]).unwrap();
        assert!(matches_query(&query, "file uses CRLF endings"));
        assert!(matches_query(&query, "file contains a BOM"));
        assert!(!matches_query(&query, "UTF-8 only"));
    }

    #[test]
    fn query_plus_terms_are_required() {
        let query = parse_query(&["CRLF".into(), "+BOM".into()]).unwrap();
        assert!(matches_query(&query, "CRLF and BOM"));
        assert!(!matches_query(&query, "CRLF only"));
        assert!(!matches_query(&query, "BOM only"));
    }

    #[test]
    fn query_minus_terms_are_excluded() {
        let query = parse_query(&["CRLF".into(), "-tests".into()]).unwrap();
        assert!(matches_query(&query, "CRLF implementation"));
        assert!(!matches_query(&query, "CRLF tests"));
    }

    #[test]
    fn query_combines_or_required_and_excluded_terms() {
        let query = parse_query(&[
            "CRLF".into(),
            "BOM".into(),
            "+split_inclusive".into(),
            "-tests".into(),
        ])
        .unwrap();

        assert!(matches_query(&query, "CRLF split_inclusive implementation"));
        assert!(matches_query(&query, "BOM split_inclusive implementation"));
        assert!(!matches_query(&query, "CRLF implementation"));
        assert!(!matches_query(&query, "BOM split_inclusive tests"));
        assert!(!matches_query(&query, "unrelated split_inclusive"));
    }

    #[test]
    fn query_supports_quoted_phrases_and_operators() {
        let query = parse_query(&[
            "\"line ending\"".into(),
            "+\"invalid UTF-8\"".into(),
            "-\"unit test\"".into(),
        ])
        .unwrap();

        assert_eq!(query.alternatives, vec!["line ending"]);
        assert_eq!(query.required, vec!["invalid UTF-8"]);
        assert_eq!(query.excluded, vec!["unit test"]);
    }

    #[test]
    fn query_supports_literal_operator_characters_when_quoted_or_escaped() {
        let query = parse_query(&[
            "\"--dry-run\"".into(),
            "\"-> bool\"".into(),
            "\"+=\"".into(),
            "\"-1\"".into(),
            r"\--verbose".into(),
        ])
        .unwrap();

        assert_eq!(
            query.alternatives,
            vec!["--dry-run", "-> bool", "+=", "-1", "--verbose"]
        );
        assert!(query.required.is_empty());
        assert!(query.excluded.is_empty());
    }

    #[test]
    fn query_preserves_argument_tokens_with_spaces() {
        let query = parse_query(&["foo bar".into(), "+baz qux".into()]).unwrap();
        assert_eq!(query.alternatives, vec!["foo bar"]);
        assert_eq!(query.required, vec!["baz qux"]);
    }

    #[test]
    fn query_rejects_exclusion_only_queries() {
        let err = parse_query(&["-tests".into()]).unwrap_err();
        assert!(
            err.contains("exclusion-only queries are not allowed"),
            "expected exclusion-only rejection: {err}"
        );
    }

    #[test]
    fn query_rejects_empty_operator_terms() {
        assert!(parse_query(&["+".into()]).is_err());
        assert!(parse_query(&["-".into()]).is_err());
        assert!(parse_query(&["\"\"".into()]).is_err());
    }

    #[test]
    fn query_echo_formatting() {
        let q1 = parse_query(&[
            "CRLF".into(),
            "BOM".into(),
            "+split_inclusive".into(),
            "-tests".into(),
        ])
        .unwrap();
        assert_eq!(
            q1.format_echo(),
            "Query: (CRLF OR BOM) AND split_inclusive AND NOT tests"
        );

        let q2 = parse_query(&["CRLF".into(), "BOM".into()]).unwrap();
        assert_eq!(q2.format_echo(), "Query: CRLF OR BOM");

        let q3 = parse_query(&["+CRLF".into(), "+BOM".into()]).unwrap();
        assert_eq!(q3.format_echo(), "Query: CRLF AND BOM");
    }

    #[test]
    fn unit_level_matching_matches_required_terms_across_lines_in_same_function() {
        let source = r#"fn target() {
    let alpha = 1;
    let beta = 2;
}
fn other() {
    let alpha = 1;
}
"#;
        let query = parse_query(&["+alpha".into(), "+beta".into()]).unwrap();

        let target_unit = rust_unit_range(source, 2).unwrap();
        assert_eq!(target_unit.1, "Rust function");
        let (start, end) = target_unit.0;
        let lines: Vec<&str> = source.lines().collect();

        let has_all = query
            .required
            .iter()
            .all(|req| (start..=end).any(|idx| lines[idx - 1].contains(req)));
        assert!(has_all, "target() should contain both +alpha and +beta");

        let other_unit = rust_unit_range(source, 6).unwrap();
        let (o_start, o_end) = other_unit.0;
        let other_has_all = query
            .required
            .iter()
            .all(|req| (o_start..=o_end).any(|idx| lines[idx - 1].contains(req)));
        assert!(!other_has_all, "other() lacks +beta and should not match");
    }

    #[test]
    fn unit_level_not_excludes_entire_unit_when_term_appears_anywhere() {
        let source = r#"fn target() {
    let alpha = 1;
    let tests = 2;
}
"#;
        let query = parse_query(&["+alpha".into(), "-tests".into()]).unwrap();
        let unit = rust_unit_range(source, 2).unwrap();
        let (start, end) = unit.0;
        let lines: Vec<&str> = source.lines().collect();

        let has_exc = query
            .excluded
            .iter()
            .any(|exc| (start..=end).any(|idx| lines[idx - 1].contains(exc)));
        assert!(
            has_exc,
            "unit contains -tests and must be excluded entirely"
        );
    }

    #[test]
    fn unit_fallbacks_to_line_on_syntax_error() {
        let broken_rust = "fn broken( { let needle = 1;";
        let unit = resolve_unit("broken.rs", 1, broken_rust);
        assert_eq!(unit.label, "Line");
        assert!(unit.warning.is_some());
        assert!(unit.warning.unwrap().contains("unparseable Rust file"));
    }

    #[test]
    fn parse_search_records_handles_colons_and_newlines_in_path() {
        let raw = b"src/odd:path\nwith_newline.rs\x0042:let value = 100;\nsrc/regular.rs\x005:println!();\n";
        let records = parse_search_records(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "src/odd:path\nwith_newline.rs");
        assert_eq!(records[0].1, 42);
        assert_eq!(records[0].2, "let value = 100;");
        assert_eq!(records[1].0, "src/regular.rs");
        assert_eq!(records[1].1, 5);
        assert_eq!(records[1].2, "println!();");
    }

    #[test]
    fn parse_search_records_handles_standard_colon_fallback() {
        let raw = b"src/lib.rs:10:fn target() {\nsrc/main.rs:25:run();\n";
        let records = parse_search_records(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "src/lib.rs");
        assert_eq!(records[0].1, 10);
        assert_eq!(records[0].2, "fn target() {");
        assert_eq!(records[1].0, "src/main.rs");
        assert_eq!(records[1].1, 25);
        assert_eq!(records[1].2, "run();");
    }

    #[test]
    fn parse_search_records_handles_empty_and_malformed_input() {
        assert!(parse_search_records(b"").is_empty());
        assert!(parse_search_records(b"malformed string without colons").is_empty());
    }

    #[test]
    fn filter_matches_applies_query_to_record_content() {
        let query = parse_query(&[
            "CRLF".into(),
            "BOM".into(),
            "+split_inclusive".into(),
            "-tests".into(),
        ])
        .unwrap();

        let input = concat!(
            "src/a.rs:10:CRLF with split_inclusive\n",
            "src/b.rs:20:BOM with split_inclusive\n",
            "src/c.rs:30:CRLF without required term\n",
            "src/d.rs:40:BOM split_inclusive tests\n",
            "src/CRLF.rs:50:unrelated split_inclusive\n",
        );

        assert_eq!(
            filter_matches(&query, input),
            concat!(
                "src/a.rs:10:CRLF with split_inclusive\n",
                "src/b.rs:20:BOM with split_inclusive\n",
            )
        );
    }

    #[test]
    fn markdown_expansion_includes_nested_headings_and_deduplicates_section() {
        let path = std::env::temp_dir().join(format!(
            "cvb-pull-markdown-parent-{}.md",
            std::process::id()
        ));
        let source = concat!(
            "# Guide\n",
            "Introduction\n",
            "## Alpha\n",
            "first needle\n",
            "second needle\n",
            "### Details\n",
            "Nested detail\n",
            "## Beta\n",
            "Sibling section\n",
        );
        std::fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!("{path_text}:4:first needle\n{path_text}:5:second needle\n");

        let expanded = expand_matches(&matches);
        let _ = std::fs::remove_file(&path);

        assert_eq!(expanded.matches("Markdown section:").count(), 1);
        assert!(expanded.contains(&format!("Markdown section: {path_text}:3-7")));
        assert!(expanded.contains("    6 | ### Details"));
        assert!(expanded.contains("    7 | Nested detail"));
        assert!(!expanded.contains("Sibling section"));
    }

    #[test]
    fn markdown_expansion_ignores_headings_inside_fenced_code() {
        let source = concat!(
            "## Real section\n",
            "```markdown\n",
            "# Not a heading\n",
            "```\n",
            "needle\n",
            "## Next section\n",
            "other\n",
        );

        assert_eq!(markdown_section_range(source, 5), Some((1, 5)));
    }

    #[test]
    fn markdown_expansion_starts_at_nearest_nested_heading() {
        let path = std::env::temp_dir().join(format!(
            "cvb-pull-markdown-nested-{}.md",
            std::process::id()
        ));
        let source = concat!(
            "# Guide\n",
            "Introduction\n",
            "## Alpha\n",
            "Alpha content\n",
            "### Details\n",
            "nested needle\n",
            "More details\n",
            "## Beta\n",
            "Sibling section\n",
        );
        std::fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!("{path_text}:6:nested needle\n");

        let expanded = expand_matches(&matches);
        let _ = std::fs::remove_file(&path);

        assert!(expanded.contains(&format!("Markdown section: {path_text}:5-7")));
        assert!(expanded.contains("    5 | ### Details"));
        assert!(expanded.contains("    6 | nested needle"));
        assert!(expanded.contains("    7 | More details"));
        assert!(!expanded.contains("Alpha content"));
        assert!(!expanded.contains("Sibling section"));
    }

    #[test]
    fn expansion_deduplicates_function_blocks_and_preserves_matches() {
        let path = std::env::temp_dir().join(format!("cvb-pull-dedup-{}.rs", std::process::id()));
        let source = "fn target() {\n    let needle = 1;\n    assert_eq!(needle, 1);\n}\n";
        std::fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!(
            "{path_text}:2:    let needle = 1;\n\
             {path_text}:3:    assert_eq!(needle, 1);\n\
             README.md:1:needle in documentation\n"
        );

        let expanded = expand_matches(&matches);
        let _ = std::fs::remove_file(&path);

        assert_eq!(expanded.matches("Rust function:").count(), 1);
        assert!(expanded.contains(&format!("Rust function: {path_text}:1-4")));
        assert!(expanded.contains("README.md:1:needle in documentation"));
    }

    #[test]
    fn unit_level_not_excludes_entire_markdown_section() {
        let source = concat!(
            "# Guide\n",
            "## surgery\n",
            "surgery techniques\n",
            "surgery protocol details\n",
            "## clean surgery\n",
            "surgery techniques\n",
            "clean instruments\n",
        );
        let query = parse_query(&["+surgery".into(), "-protocol".into()]).unwrap();
        let lines: Vec<&str> = source.lines().collect();

        let (s1_start, s1_end) = markdown_section_range(source, 3).unwrap();
        assert_eq!((s1_start, s1_end), (2, 4));
        let s1_has_req = query
            .required
            .iter()
            .all(|req| (s1_start..=s1_end).any(|idx| lines[idx - 1].contains(req)));
        let s1_has_exc = query
            .excluded
            .iter()
            .any(|exc| (s1_start..=s1_end).any(|idx| lines[idx - 1].contains(exc)));
        assert!(s1_has_req, "section 1 should contain +surgery");
        assert!(
            s1_has_exc,
            "section 1 contains -protocol and must be excluded"
        );

        let (s2_start, s2_end) = markdown_section_range(source, 6).unwrap();
        assert_eq!((s2_start, s2_end), (5, 7));
        let s2_has_req = query
            .required
            .iter()
            .all(|req| (s2_start..=s2_end).any(|idx| lines[idx - 1].contains(req)));
        let s2_has_exc = query
            .excluded
            .iter()
            .any(|exc| (s2_start..=s2_end).any(|idx| lines[idx - 1].contains(exc)));
        assert!(s2_has_req, "section 2 should contain +surgery");
        assert!(
            !s2_has_exc,
            "section 2 lacks -protocol and must be accepted"
        );
    }

    #[test]
    fn run_capture_truncates_line_range_at_eof() {
        let dir = std::env::temp_dir()
            .join(format!("cvb-pull-lines-eof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        std::fs::write(dir.join("src/lib.rs"), "alpha\nbeta\ngamma\n").unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let (status, stdout, stderr) = run_capture(
            &["file:src/lib.rs".into(), "lines:2-5".into()],
            Some(&dir),
        );

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(status, 0, "range extending past EOF should succeed");
        assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        assert!(
            stdout.contains("warning:") && stdout.contains("2-5"),
            "expected a warning preserving the requested range:\n{stdout}"
        );
        assert!(
            stdout.contains("src/lib.rs:2: beta\nsrc/lib.rs:3: gamma\n"),
            "expected all available requested lines:\n{stdout}"
        );
        assert!(
            !stdout.contains("src/lib.rs:4:") && !stdout.contains("src/lib.rs:5:"),
            "must not emit nonexistent lines:\\n{stdout}"
        );
    }

    #[test]
    fn patch_reminder_only_appears_once_on_successful_content_captures() {
        const REMINDER: &str =
            "Edit with a patch: apply_patch format or git diff, pasted at the prompt of this session.";

        let dir = std::env::temp_dir()
            .join(format!("cvb-pull-patch-reminder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap()
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn alpha() {}\npub fn caller() { alpha(); }\n",
        )
        .unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let (line_status, line_stdout, line_stderr) = run_capture(
            &["file:src/lib.rs".into(), "lines:1-2".into()],
            Some(&dir),
        );
        assert_eq!(line_status, 0, "{line_stderr}");
        assert_eq!(
            line_stdout.matches(REMINDER).count(),
            1,
            "successful line capture must contain exactly one reminder:\n{line_stdout}"
        );

        let (search_status, search_stdout, search_stderr) =
            run_capture(&["alpha".into()], Some(&dir));
        assert_eq!(search_status, 0, "{search_stderr}");
        assert_eq!(
            search_stdout.matches(REMINDER).count(),
            1,
            "successful search capture must contain exactly one reminder:\n{search_stdout}"
        );

        let (miss_status, miss_stdout, miss_stderr) =
            run_capture(&["definitely_missing_cvb_term".into()], Some(&dir));
        assert_eq!(miss_status, 1, "{miss_stderr}");
        assert!(
            !miss_stdout.contains(REMINDER),
            "no-match output must not contain the reminder:\n{miss_stdout}"
        );

        let (callers_status, callers_stdout, callers_stderr) =
            run_capture(&["callers:alpha".into()], Some(&dir));
        assert_eq!(callers_status, 0, "{callers_stderr}");
        assert!(
            !callers_stdout.contains(REMINDER),
            "callers output must not contain the reminder:\n{callers_stdout}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn patch_reminder_names_both_patch_formats() {
        assert!(PATCH_REMINDER.contains("apply_patch"));
        assert!(PATCH_REMINDER.contains("git diff"));
        assert!(!PATCH_REMINDER.contains("surgery"));
        assert!(!PATCH_REMINDER.contains("token"));
    }


    #[test]
    fn run_end_to_end_in_temporary_git_repo() {
        let dir = std::env::temp_dir().join(format!("cvb-pull-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = "fn process() {\n    let crlf = 1;\n    let split_inclusive = 2;\n}\n";
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();

        let md_source = "# Guide\n## surgery\nsurgery techniques\nsurgery protocol details\n## clean surgery\nsurgery techniques\nclean instruments\n";
        std::fs::write(dir.join("guide.md"), md_source).unwrap();

        run_cmd(&["git", "add", "src/lib.rs", "guide.md"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let success_exit = run_in_dir(&["+crlf".into(), "+split_inclusive".into()], Some(&dir));
        assert_eq!(success_exit, 0);

        let no_match_exit = run_in_dir(&["+crlf".into(), "+missing".into()], Some(&dir));
        assert_eq!(no_match_exit, 1);

        let (md_match_exit, stdout, _) =
            run_capture(&["+surgery".into(), "-protocol".into()], Some(&dir));
        assert_eq!(md_match_exit, 0);
        assert!(
            stdout.contains("clean instruments"),
            "clean surgery section should be included in stdout:\n{stdout}"
        );
        assert!(
            !stdout.contains("surgery protocol details"),
            "excluded section containing 'protocol' must not leak into stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("-protocol: excluded 1 unit"),
            "exclusion note should be present in stdout:\n{stdout}"
        );

        let (md_excluded_exit, no_match_stdout, _) =
            run_capture(&["+protocol".into(), "-surgery".into()], Some(&dir));
        assert_eq!(md_excluded_exit, 1);
        assert!(no_match_stdout.contains("No matches."));
        assert!(
            !no_match_stdout.contains("clean instruments")
                && !no_match_stdout.contains("surgery protocol details")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- multiple file: scopes, head:/tail:, lines: with search terms ----

    fn pull_repo(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cvb-pull-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
            .status()
            .unwrap();
        assert!(status.success());
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    fn pull(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        run_capture(&args, Some(dir))
    }

    const TWO_FILES: [(&str, &str); 3] = [
        ("a.rs", "fn alpha() {\n    let needle = 1;\n}\n"),
        ("b.rs", "fn beta() {\n    let needle = 2;\n}\n"),
        ("c.rs", "fn gamma() {\n    let needle = 3;\n}\n"),
    ];

    #[test]
    fn two_file_scopes_search_both_files_only() {
        let dir = pull_repo("two-scopes", &TWO_FILES);
        let (code, out, err) = pull(&dir, &["file:a.rs", "file:b.rs", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("a.rs:1-3") && out.contains("b.rs:1-3"), "{out}");
        assert!(!out.contains("c.rs"), "{out}");
    }

    #[test]
    fn one_matching_scope_among_several_is_enough_for_results() {
        let dir = pull_repo("one-hits", &[
            ("a.rs", "fn alpha() {\n    let needle = 1;\n}\n"),
            ("b.rs", "fn beta() {}\n"),
        ]);
        let (code, out, err) = pull(&dir, &["file:a.rs", "file:b.rs", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("a.rs:1-3") && !out.contains("b.rs"), "{out}");
    }

    #[test]
    fn duplicate_file_scopes_are_deduplicated_silently() {
        let dir = pull_repo("dup-scopes", &TWO_FILES);
        let (code, once, err) = pull(&dir, &["file:a.rs", "needle"]);
        assert_eq!(code, 0, "{err}");
        let (code, twice, err) = pull(&dir, &["file:a.rs", "file:./a.rs", "file:a.rs", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(err.is_empty(), "{err}");
        assert_eq!(once, twice);
    }

    #[test]
    fn a_missing_scope_fails_the_whole_command() {
        let dir = pull_repo("missing-scope", &TWO_FILES);
        let (code, out, err) = pull(&dir, &["file:a.rs", "file:nope.rs", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(out.is_empty(), "no partial results: {out}");
        assert!(err.contains("file scope matches no searchable file") && err.contains("nope.rs"), "{err}");
    }

    #[test]
    fn output_does_not_depend_on_the_order_of_file_arguments() {
        let dir = pull_repo("scope-order", &TWO_FILES);
        let (_, forward, _) = pull(&dir, &["file:a.rs", "file:c.rs", "needle"]);
        let (_, backward, _) = pull(&dir, &["file:c.rs", "file:a.rs", "needle"]);
        let (_, callers_f, _) = pull(&dir, &["file:a.rs", "file:c.rs", "callers:alpha"]);
        let (_, callers_b, _) = pull(&dir, &["file:c.rs", "file:a.rs", "callers:alpha"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(forward, backward);
        assert_eq!(callers_f, callers_b);
        assert!(forward.find("a.rs").unwrap() < forward.find("c.rs").unwrap());
    }

    #[test]
    fn callers_accepts_two_file_scopes() {
        let dir = pull_repo("callers-two", &[
            ("a.rs", "fn one() { run(); }\n"),
            ("b.rs", "fn two() { run(); }\n"),
            ("c.rs", "fn three() { run(); }\n"),
        ]);
        let (code, out, err) = pull(&dir, &["file:a.rs", "file:b.rs", "callers:run"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("a.rs:1") && out.contains("b.rs:1") && !out.contains("c.rs"), "{out}");
        let (code, _, err) = pull(&dir, &["file:a.rs", "file:missing.rs", "callers:run"]);
        assert_eq!(code, 2);
        assert!(err.contains("missing.rs"), "{err}");
        let (code, _, err) = pull(&dir, &["file:a.rs", "file:notes.txt", "callers:run"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(err.contains("callers: file scope must be a source file"), "{err}");
    }

    #[test]
    fn several_scopes_without_terms_or_lines_are_an_error() {
        let dir = pull_repo("scopes-bare", &TWO_FILES);
        let (code, out, err) = pull(&dir, &["file:a.rs", "file:b.rs"]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("several file: scopes"), "{err}");
        let (code, _, err) = pull(&dir, &["file:a.rs", "file:b.rs", "lines:1-2"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(err.contains("lines: requires exactly one file: scope."), "{err}");
    }

    #[test]
    fn a_lone_file_scope_is_still_a_whole_file_capture() {
        let dir = pull_repo("lone-file", &TWO_FILES);
        let (code, out, _) = pull(&dir, &["file:a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0);
        assert!(out.starts_with("a.rs:1: fn alpha() {\na.rs:2:     let needle = 1;\na.rs:3: }\n"), "{out}");
    }

    fn numbered(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    fn line_numbers(out: &str, path: &str) -> Vec<usize> {
        out.lines()
            .filter_map(|l| l.strip_prefix(path)?.strip_prefix(':')?.split(':').next()?.parse().ok())
            .collect()
    }

    #[test]
    fn head_and_tail_on_a_long_file() {
        let dir = pull_repo("headtail", &[("long.txt", &numbered(300)), ("short.txt", &numbered(3))]);
        let (code, out, err) = pull(&dir, &["file:long.txt", "head:50"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(line_numbers(&out, "long.txt"), (1..=50).collect::<Vec<_>>());
        assert!(out.contains("long.txt:50: line 50\n"));

        let (code, out, _) = pull(&dir, &["tail:20", "file:long.txt"]);
        assert_eq!(code, 0);
        assert_eq!(line_numbers(&out, "long.txt"), (281..=300).collect::<Vec<_>>());
        assert!(out.contains("long.txt:281: line 281\n"));

        let (_, out, _) = pull(&dir, &["file:short.txt", "head:2"]);
        assert_eq!(line_numbers(&out, "short.txt"), vec![1, 2]);
        let (_, out, _) = pull(&dir, &["file:short.txt", "tail:1"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(line_numbers(&out, "short.txt"), vec![3]);
    }

    #[test]
    fn head_and_tail_equal_below_and_above_the_file_length() {
        let dir = pull_repo("headtail-len", &[("f.txt", &numbered(5))]);
        let whole = pull(&dir, &["file:f.txt"]).1;
        for arg in ["head:5", "tail:5", "head:6", "tail:500"] {
            let (code, out, err) = pull(&dir, &["file:f.txt", arg]);
            assert_eq!(code, 0, "{arg}: {err}");
            assert_eq!(out, whole, "{arg}");
        }
        let (_, out, _) = pull(&dir, &["file:f.txt", "head:4"]);
        assert_eq!(line_numbers(&out, "f.txt"), vec![1, 2, 3, 4]);
        let (_, out, _) = pull(&dir, &["file:f.txt", "tail:4"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(line_numbers(&out, "f.txt"), vec![2, 3, 4, 5]);
    }

    #[test]
    fn head_and_tail_on_an_empty_file_behave_like_a_plain_capture() {
        let dir = pull_repo("headtail-empty", &[("e.txt", "")]);
        let plain = pull(&dir, &["file:e.txt"]);
        let head = pull(&dir, &["file:e.txt", "head:3"]);
        let tail = pull(&dir, &["file:e.txt", "tail:3"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(plain.0, 0);
        assert_eq!(plain, head);
        assert_eq!(plain, tail);
    }

    #[test]
    fn head_and_tail_reject_zero_and_non_numeric_counts() {
        let dir = pull_repo("headtail-bad", &[("f.txt", &numbered(5))]);
        for arg in ["head:0", "tail:0", "head:abc", "tail:-3", "head:+3", "head:", "tail:1.5", "head: 3", "head:99999999999999999999999"] {
            let (code, out, err) = pull(&dir, &["file:f.txt", arg]);
            assert_eq!(code, 2, "{arg}");
            assert!(out.is_empty(), "{arg}: {out}");
            assert!(err.contains("positive integer"), "{arg}: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn head_and_tail_reject_every_combination_but_a_single_file_and_lines() {
        let dir = pull_repo("headtail-combos", &[("f.rs", "fn f() { run(); }\n"), ("g.rs", "fn g() {}\n")]);
        let cases: [(&[&str], &str); 9] = [
            (&["file:f.rs", "head:1", "tail:1"], "head: and tail: cannot be combined"),
            (&["file:f.rs", "head:1", "head:2"], "only one head:"),
            (&["file:f.rs", "head:1", "run"], "head:/tail: cannot be combined with search terms."),
            (&["head:1", "run"], "head:/tail: cannot be combined with search terms."),
            (&["file:f.rs", "tail:1", "lines:1", "run"], "head:/tail: cannot be combined with search terms."),
            (&["callers:run", "head:1"], "head:/tail: cannot be combined with callers:."),
            (&["file:f.rs", "head:1", "cascade:1"], "head:/tail: cannot be combined with cascade:."),
            (&["head:1"], "head:/tail: requires a file: scope."),
            (&["file:f.rs", "file:g.rs", "tail:1"], "head:/tail: requires exactly one file: scope."),
        ];
        for (args, expected) in cases {
            let (code, out, err) = pull(&dir, args);
            assert_eq!(code, 2, "{args:?}");
            assert!(out.is_empty(), "{args:?}: {out}");
            assert!(err.contains(expected), "{args:?}: {err}");
        }
        let (code, _, err) = pull(&dir, &["-head:3", "file:f.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(err.contains("negated selector"), "{err}");
    }

    #[test]
    fn head_and_tail_trim_the_combined_lines_output() {
        let dir = pull_repo("headtail-lines", &[("f.txt", &numbered(30))]);
        let (code, out, err) = pull(&dir, &["file:f.txt", "lines:2-4", "lines:10-12", "head:4"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(line_numbers(&out, "f.txt"), vec![2, 3, 4, 10]);
        let (_, out, _) = pull(&dir, &["file:f.txt", "lines:2-4", "lines:10-12", "tail:4"]);
        assert_eq!(line_numbers(&out, "f.txt"), vec![4, 10, 11, 12]);
        let (_, out, _) = pull(&dir, &["file:f.txt", "lines:2-4", "tail:50"]);
        assert_eq!(line_numbers(&out, "f.txt"), vec![2, 3, 4]);
        // Out-of-range lines: stays an error.
        let (code, _, err) = pull(&dir, &["file:f.txt", "lines:40-50", "head:3"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(err.contains("starts beyond"), "{err}");
    }

    const FN_FILE: &str = "fn before() {\n    let x = 1;\n}\n\nfn target() {\n    let first = 1;\n    let needle = 2;\n    let last = 3;\n}\n\nfn after() {\n    let needle = 4;\n}\n";

    #[test]
    fn lines_with_terms_return_the_whole_unit_beyond_the_range() {
        let dir = pull_repo("lines-terms", &[("x.rs", FN_FILE)]);
        // The range covers only the line with the hit, inside target() (5-9).
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:7", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Rust function: x.rs:5-9"), "{out}");
        assert!(out.contains("let first = 1;") && out.contains("let last = 3;"), "{out}");
        assert!(!out.contains("fn after"), "{out}");
        assert!(out.contains("Query: needle\nRestricted to lines 7 of x.rs"), "{out}");
    }

    #[test]
    fn lines_with_terms_ignore_hits_outside_the_range() {
        let dir = pull_repo("lines-outside", &[("x.rs", FN_FILE)]);
        let (code, out, _) = pull(&dir, &["file:x.rs", "lines:1-9", "needle"]);
        assert_eq!(code, 0);
        assert!(out.contains("x.rs:5-9") && !out.contains("x.rs:11-13") && !out.contains("let needle = 4;"), "{out}");
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:10-13", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("x.rs:11-13") && !out.contains("x.rs:5-9"), "{out}");
    }

    #[test]
    fn several_line_ranges_are_ored() {
        let dir = pull_repo("lines-multi", &[("x.rs", FN_FILE)]);
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:7", "lines:12", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("x.rs:5-9") && out.contains("x.rs:11-13"), "{out}");
        assert!(out.contains("Restricted to lines 7, 12 of x.rs"), "{out}");
    }

    #[test]
    fn no_hit_in_range_is_the_normal_no_match_result() {
        let dir = pull_repo("lines-nomatch", &[("x.rs", FN_FILE)]);
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:1-3", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 1, "{err}");
        assert!(out.contains("Query: needle\n") && out.contains("Restricted to lines 1-3 of x.rs"), "{out}");
        assert!(out.contains("No matches.") && out.contains("needle: 0 units"), "{out}");
    }

    #[test]
    fn lines_with_terms_range_beyond_the_file() {
        let dir = pull_repo("lines-eof", &[("x.rs", FN_FILE)]);
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:12-99", "needle"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("truncated to 12-13") && out.contains("x.rs:11-13"), "{out}");
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:50-60", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(out.is_empty() && err.contains("starts beyond"), "{out}{err}");
    }

    #[test]
    fn operators_see_the_whole_unit_but_hits_must_be_in_range() {
        let dir = pull_repo("lines-ops", &[("x.rs", FN_FILE)]);
        // +last is outside the range but inside the unit: still satisfied.
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:7", "needle", "+last"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("x.rs:5-9"), "{out}");
        // -first is outside the range but inside the unit: still excludes it.
        let (code, out, _) = pull(&dir, &["file:x.rs", "lines:7", "needle", "-first"]);
        assert_eq!(code, 1);
        assert!(out.contains("-first: would have removed 1 unit"), "{out}");
        // Required-only query: a hit of any required term in range seeds the unit.
        let (code, out, err) = pull(&dir, &["file:x.rs", "lines:8", "+needle", "+last"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("x.rs:5-9"), "{out}");
        let (code, out, _) = pull(&dir, &["file:x.rs", "lines:6", "+needle", "+last"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 1, "no required term hits line 6: {out}");
    }

    #[test]
    fn lines_without_terms_are_unchanged_and_callers_still_reject_lines() {
        let dir = pull_repo("lines-plain", &[("x.rs", FN_FILE)]);
        let (code, out, _) = pull(&dir, &["file:x.rs", "lines:6-7"]);
        assert_eq!(code, 0);
        assert!(out.starts_with("x.rs:6:     let first = 1;\nx.rs:7:     let needle = 2;\n"), "{out}");
        assert!(!out.contains("Restricted"), "{out}");
        let (code, _, err) = pull(&dir, &["file:x.rs", "lines:6", "callers:needle"]);
        assert_eq!(code, 2);
        assert!(err.contains("callers: cannot be combined with lines:"), "{err}");
        let (code, _, err) = pull(&dir, &["lines:6", "needle"]);
        assert_eq!(code, 2);
        assert!(err.contains("lines: requires a file: scope."), "{err}");
        let (code, _, err) = pull(&dir, &["file:x.rs", "file:y.rs", "lines:6", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 2);
        assert!(err.contains("lines: requires exactly one file: scope."), "{err}");
    }

    #[test]
    fn lines_with_terms_restrict_markdown_and_bash_units_too() {
        let md = "# One\nalpha\nneedle here\n# Two\nbeta\nneedle again\n";
        let sh = "first() {\n  echo needle\n}\nsecond() {\n  echo one\n  echo needle\n}\n";
        let dir = pull_repo("lines-md-sh", &[("d.md", md), ("s.sh", sh)]);
        let (code, out, err) = pull(&dir, &["file:d.md", "lines:6", "needle"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("d.md:4-6") && out.contains("beta") && !out.contains("alpha"), "{out}");
        let (code, out, err) = pull(&dir, &["file:s.sh", "lines:6", "needle"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Bash function: s.sh:4-7") && out.contains("echo one") && !out.contains("first()"), "{out}");
    }
}
