use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::cascade;

const SURGERY_REMINDER: &str =
    "Edit with: cvb surgery --spec (one envelope per paste, use the printed token).";

fn append_surgery_reminder(stdout: &mut String) {
    stdout.push_str(SURGERY_REMINDER);
    stdout.push('\n');
}

pub fn tracked_files(dir: Option<&Path>) -> Result<Vec<OsString>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["ls-files", "-z"]);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }

    let output = cmd
        .output()
        .map_err(|error| format!("CVB pull: unable to run git ls-files: {error}"))?;

    if !output.status.success() {
        return Err(
            "CVB pull: unable to list Git-tracked files; run inside a Git repository.".to_string(),
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
        return Err("CVB pull: no Git-tracked files found.".to_string());
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

fn extract_file_scope(
    args: &[String],
) -> Result<(Option<String>, Vec<(usize, usize)>, Vec<String>), String> {
    let mut scope = None;
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
            if scope.is_some() {
                return Err("CVB pull: only one file: scope is allowed.".to_string());
            }
            scope = Some(path.to_string());
        } else if let Some(range) = arg.strip_prefix("lines:") {
            line_ranges.push(parse_line_range(range)?);
        } else {
            query_args.push(arg.clone());
        }
    }

    if !line_ranges.is_empty() && scope.is_none() {
        return Err("CVB pull: lines: requires a file: scope.".to_string());
    }

    if query_args.is_empty() && line_ranges.is_empty() {
        return Err("CVB pull: search query is empty.".to_string());
    }

    Ok((scope, line_ranges, query_args))
}

pub fn filter_file_scope(
    files: Vec<OsString>,
    scope: Option<&str>,
    base_dir: Option<&Path>,
) -> Result<Vec<OsString>, String> {
    let Some(scope) = scope else {
        return Ok(files);
    };

    let norm_scope = normalize_path_str(scope, base_dir);

    let matching = files
        .into_iter()
        .filter(|path| {
            let path_str = path.to_string_lossy();
            path == OsStr::new(scope)
                || path_str == norm_scope
                || normalize_path_str(&path_str, base_dir) == norm_scope
        })
        .collect::<Vec<_>>();

    if matching.is_empty() {
        return Err(format!(
            "CVB pull: file scope matches no Git-tracked file: {scope}"
        ));
    }

    Ok(matching)
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
    let mut separators = record.match_indices(':');
    while let Some((index, _)) = separators.next() {
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

            if let Ok(line_str) = std::str::from_utf8(line_bytes) {
                if let Ok(line_num) = line_str.parse::<usize>() {
                    let path_str = String::from_utf8_lossy(path_bytes).into_owned();
                    let content_str = String::from_utf8_lossy(content_bytes).into_owned();
                    results.push((path_str, line_num, content_str));
                }
            }
        }

        return results;
    }

    let mut results = Vec::new();
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        if let Some(content) = matching_content(line) {
            let mut parts = line.splitn(3, ':');
            if let (Some(path), Some(line_str)) = (parts.next(), parts.next()) {
                if let Ok(line_num) = line_str.parse::<usize>() {
                    results.push((path.to_string(), line_num, content.to_string()));
                }
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

fn extract_callers_scope(args: &[String]) -> Result<Option<(String, Option<String>)>, String> {
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

    if target.is_empty() || syn::parse_str::<syn::Ident>(target).is_err() {
        return Err(
            "CVB pull: callers: must be followed by a Rust function or method name.".to_string(),
        );
    }

    let mut scope = None;

    for arg in args {
        if arg == selectors[0] {
            continue;
        }

        if let Some(path) = arg.strip_prefix("file:") {
            if path.is_empty() {
                return Err("CVB pull: file: must be followed by a path.".to_string());
            }
            if scope.is_some() {
                return Err("CVB pull: only one file: scope is allowed.".to_string());
            }
            if Path::new(path).extension() != Some(OsStr::new("rs")) {
                return Err("CVB pull: callers: file scope must be a Rust .rs file.".to_string());
            }
            scope = Some(path.to_string());
        } else if arg.starts_with("lines:") {
            return Err("CVB pull: callers: cannot be combined with lines:.".to_string());
        } else {
            return Err("CVB pull: callers: cannot be combined with search terms.".to_string());
        }
    }

    Ok(Some((target.to_string(), scope)))
}

fn run_callers(
    target: &str,
    scope: Option<&str>,
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

    let files = match filter_file_scope(files, scope, dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return 2;
        }
    };

    let rust_files: Vec<OsString> = files
        .into_iter()
        .filter(|path| Path::new(path).extension() == Some(OsStr::new("rs")))
        .collect();

    let mut found = false;
    let mut incomplete = false;

    for path in rust_files {
        let display_path = path.to_string_lossy();
        let full_path = if let Some(d) = dir {
            d.join(&path)
        } else {
            PathBuf::from(&path)
        };

        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(error) => {
                stderr.push_str(&format!(
                    "CVB pull: warning: unable to read Rust file {display_path}: {error}; \
                     caller results may be incomplete.\n"
                ));
                incomplete = true;
                continue;
            }
        };

        let matches = match rust_callers(&source, target) {
            Some(matches) => matches,
            None => {
                stderr.push_str(&format!(
                    "CVB pull: warning: skipping unparseable Rust file {display_path}; \
                     caller results may be incomplete.\n"
                ));
                incomplete = true;
                continue;
            }
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

fn count_units_for_term(
    term: &str,
    files: &[OsString],
    file_cache: &mut HashMap<String, String>,
    dir: Option<&Path>,
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

    match extract_callers_scope(&args_without_cascade) {
        Ok(Some((target, scope))) => {
            let status = run_callers(&target, scope.as_deref(), dir, &mut stdout, &mut stderr);
            return (status, stdout, stderr);
        }
        Ok(None) => {}
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    }

    let whole_file_capture =
        args_without_cascade.len() == 1 && args_without_cascade[0].starts_with("file:");

    if args_without_cascade.is_empty() {
        stderr.push_str(format_usage());
        return (2, stdout, stderr);
    }

    let parsed = if whole_file_capture {
        extract_file_scope(&[args_without_cascade[0].clone(), "lines:1".to_string()])
    } else {
        extract_file_scope(&args_without_cascade)
    };
    let (scope, mut line_ranges, query_args) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    if !line_ranges.is_empty() && !query_args.is_empty() {
        stderr.push_str("CVB pull: lines: cannot be combined with search terms.\n");
        return (2, stdout, stderr);
    }

    let query = if !line_ranges.is_empty() {
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

    let files = match filter_file_scope(files, scope.as_deref(), dir) {
        Ok(files) => files,
        Err(error) => {
            stderr.push_str(&format!("{error}\n"));
            return (2, stdout, stderr);
        }
    };

    if whole_file_capture {
        let Some(path) = scope.as_deref() else {
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
        line_ranges = vec![(1, line_count)];
    }

    if let Some(path) = scope.as_deref().filter(|_| !line_ranges.is_empty()) {
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

        for (start, end) in line_ranges {
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
                stdout.push_str(&format!(
                    "CVB pull: warning: requested line range {start}-{end} extends past EOF; truncated to {start}-{actual_end} ({path} has {} lines).\n",
                    lines.len()
                ));
            }
            for number in start..=actual_end {
                stdout.push_str(&format!("{path}:{number}: {}\n", lines[number - 1]));
            }
        }
        append_surgery_reminder(&mut stdout);
        return (0, stdout, stderr);
    }

    let search_terms: Vec<&str> = if !query.alternatives.is_empty() {
        query.alternatives.iter().map(String::as_str).collect()
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

    let mut matching_units: Vec<(SearchUnit, Vec<(String, Vec<usize>)>)> = Vec::new();
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
        stdout.push_str("No matches.\n");

        let mut pos_counts = Vec::new();
        for alt in &query.alternatives {
            let count = count_units_for_term(alt, &files, &mut file_cache, dir);
            let unit_str = if count == 1 { "unit" } else { "units" };
            pos_counts.push(format!("{alt}: {count} {unit_str}"));
        }
        for req in &query.required {
            let count = count_units_for_term(req, &files, &mut file_cache, dir);
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

    if let Some(depth) = cascade_depth {
        if depth > 0 {
            let scope_desc = if let Some(s) = scope.as_deref() {
                format!("initial search in {s}, dependencies repo-wide")
            } else {
                "repo-wide".to_string()
            };
            stdout.push_str(&format!(
                "Cascade: depth {depth} | limits {} units, {} chars | scope: {scope_desc}\n",
                cascade::MAX_ADDED_UNITS,
                cascade::MAX_ADDED_CHARS
            ));
        }
    }

    for exc in &query.excluded {
        if let Some(&count) = eliminated_by_exclusion.get(exc) {
            if count > 0 {
                let unit_str = if count == 1 { "unit" } else { "units" };
                stdout.push_str(&format!("-{exc}: excluded {count} {unit_str}\n"));
            }
        }
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

    if let Some(depth) = cascade_depth {
        if depth > 0 {
            let cascade_bundle = cascade::run_with_reserved_chars(
                &matching_units,
                dir,
                depth,
                SURGERY_REMINDER.chars().count() + 1,
            );
            stdout.push_str(&cascade_bundle);
        }
    }

    append_surgery_reminder(&mut stdout);
    (0, stdout, stderr)
}

fn format_usage() -> &'static str {
    "Usage: cvb pull [file:PATH] [cascade:N] <search-term>...\n\
           cvb pull file:PATH lines:LINE|START-END\n\
           cvb pull [file:PATH] callers:IDENT\n\n\
     Operators:\n\
         foo bar          OR: match either term\n\
         +foo             AND: require term anywhere in the matching unit\n\
         -foo             NOT: exclude any unit containing term\n\
         cascade:N        Expand dependencies up to depth N (0..=5)\n\
         \"foo bar\"        Match exact phrase\n\
         \"--dry-run\"      Match literal term starting with - or +\n\n\
     Examples:\n\
         cvb pull CRLF BOM\n\
         cvb pull +CRLF +BOM cascade:2\n\
         cvb pull CRLF BOM +split_inclusive -tests\n\
         cvb pull callers:run\n\
         cvb pull file:src/pull.rs lines:10-25\n"
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
        if let TokenTree::Group(group) = next {
            if group.delimiter() == Delimiter::Parenthesis {
                return true;
            }
        }

        if let TokenTree::Punct(p1) = next {
            if p1.as_char() == ':' {
                if let (Some(TokenTree::Punct(p2)), Some(TokenTree::Punct(p3))) =
                    (trees.get(i + 2), trees.get(i + 3))
                {
                    if p2.as_char() == ':' && p3.as_char() == '<' {
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
                                        {
                                            if call_group.delimiter() == Delimiter::Parenthesis {
                                                return true;
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                            j += 1;
                        }
                    }
                }
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
            if let syn::Expr::Path(path) = node.func.as_ref() {
                if path
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
            if fence.is_none() {
                if let Some(level) = markdown_heading_level(line) {
                    headings.push((index + 1, level));
                }
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
            Some(("run".to_string(), None))
        );
        assert_eq!(
            extract_callers_scope(&["file:src/pull.rs".to_string(), "callers:run".to_string(),])
                .unwrap(),
            Some(("run".to_string(), Some("src/pull.rs".to_string())))
        );
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
                "file:src/main.rs".to_string(),
            ],
            vec!["file:manual.md".to_string(), "callers:run".to_string()],
            vec!["callers:module::run".to_string()],
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

        let filtered = filter_file_scope(files, Some("src/pull.rs"), None).unwrap();
        assert_eq!(filtered, vec![OsString::from("src/pull.rs")]);
    }

    #[test]
    fn filter_file_scope_normalizes_relative_dot_prefix() {
        let files = vec![OsString::from("src/pull.rs")];
        let filtered = filter_file_scope(files, Some("./src/pull.rs"), None).unwrap();
        assert_eq!(filtered, vec![OsString::from("src/pull.rs")]);
    }

    #[test]
    fn filter_file_scope_without_scope_preserves_all_files() {
        let files = vec![OsString::from("src/pull.rs"), OsString::from("manual.md")];
        assert_eq!(filter_file_scope(files.clone(), None, None).unwrap(), files);
    }

    #[test]
    fn filter_file_scope_rejects_untracked_path() {
        let files = vec![OsString::from("src/pull.rs")];
        let error = filter_file_scope(files, Some("manual.md"), None).unwrap_err();
        assert!(error.contains("no Git-tracked file"));
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
        assert_eq!(scope.as_deref(), Some("src/pull.rs"));
        assert!(lines.is_empty());
        assert_eq!(query, vec!["protocol", "+line-endings", "-example"]);
    }

    #[test]
    fn extract_file_scope_rejects_empty_or_multiple_scopes() {
        assert!(extract_file_scope(&["file:".into(), "needle".into()]).is_err());
        assert!(
            extract_file_scope(&[
                "file:src/pull.rs".into(),
                "file:manual.md".into(),
                "needle".into(),
            ])
            .is_err()
        );
        assert!(extract_file_scope(&["file:src/pull.rs".into()]).is_err());
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
        assert_eq!(scope.as_deref(), Some("src/pull.rs"));
        assert_eq!(lines, vec![(96, 110)]);
        assert!(query.is_empty());

        let single_line = vec!["file:manual.md".into(), "lines:155".into()];
        let (scope, lines, query) = extract_file_scope(&single_line).unwrap();
        assert_eq!(scope.as_deref(), Some("manual.md"));
        assert_eq!(lines, vec![(155, 155)]);
        assert!(query.is_empty());

        let multiple = extract_file_scope(&[
            "file:src/pull.rs".into(),
            "lines:96-110".into(),
            "lines:5-8".into(),
            "lines:7".into(),
        ])
        .unwrap();
        assert_eq!(multiple.0.as_deref(), Some("src/pull.rs"));
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
    fn surgery_reminder_only_appears_once_on_successful_content_captures() {
        const REMINDER: &str =
            "Edit with: cvb surgery --spec (one envelope per paste, use the printed token).";

        let dir = std::env::temp_dir()
            .join(format!("cvb-pull-surgery-reminder-{}", std::process::id()));
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
}
