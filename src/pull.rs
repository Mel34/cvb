use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, Output};

fn tracked_files() -> Result<Vec<OsString>, String> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| format!("CVB pull: unable to run git ls-files: {error}"))?;

    if !output.status.success() {
        return Err("CVB pull: unable to list Git-tracked files; run inside a Git repository."
            .to_string());
    }

    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_vec(path.to_vec()))
        .collect::<Vec<_>>();

    if files.is_empty() {
        return Err("CVB pull: no Git-tracked files found.".to_string());
    }

    Ok(files)
}


fn parse_line_range(range: &str) -> Result<(usize, usize), String> {
    let Some((start, end)) = range.split_once('-') else {
        let line = range.parse::<usize>().map_err(|_| {
            format!(
                "CVB pull: invalid line selector '{range}'; expected LINE or START-END."
            )
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

fn read_line_range(path: &str, start: usize, end: usize) -> Result<String, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("CVB pull: unable to read {path}: {error}"))?;
    let lines: Vec<&str> = source.lines().collect();

    if start == 0 || end == 0 || start > end {
        return Err("CVB pull: invalid line range; expected positive START not exceeding END."
            .to_string());
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

fn filter_file_scope(
    files: Vec<OsString>,
    scope: Option<&str>,
) -> Result<Vec<OsString>, String> {
    let Some(scope) = scope else {
        return Ok(files);
    };

    let matching = files
        .into_iter()
        .filter(|path| path == OsStr::new(scope))
        .collect::<Vec<_>>();

    if matching.is_empty() {
        return Err(format!(
            "CVB pull: file scope matches no Git-tracked file: {scope}"
        ));
    }

    Ok(matching)
}

#[derive(Debug, Default)]
struct SearchQuery {
    alternatives: Vec<String>,
    required: Vec<String>,
    excluded: Vec<String>,
}

fn parse_query(args: &[String]) -> Result<SearchQuery, String> {
    let input = args.join(" ");
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut token_started = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                token_started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            c => {
                current.push(c);
                token_started = true;
            }
        }
    }

    if quoted {
        return Err("CVB pull: unmatched quote in search query.".to_string());
    }
    if token_started {
        tokens.push(current);
    }

    let mut query = SearchQuery::default();

    for token in tokens {
        let (kind, term) = match token.strip_prefix('+') {
            Some(term) => ('+', term),
            None => match token.strip_prefix('-') {
                Some(term) => ('-', term),
                None => (' ', token.as_str()),
            },
        };

        if term.is_empty() {
            return Err("CVB pull: search operators must be followed by a term.".to_string());
        }

        match kind {
            '+' => query.required.push(term.to_string()),
            '-' => query.excluded.push(term.to_string()),
            _ => query.alternatives.push(term.to_string()),
        }
    }

    if query.alternatives.is_empty()
        && query.required.is_empty()
        && query.excluded.is_empty()
    {
        return Err("CVB pull: search query is empty.".to_string());
    }

    Ok(query)
}

fn matches_query(query: &SearchQuery, line: &str) -> bool {
    let has_alternative = query.alternatives.is_empty()
        || query.alternatives.iter().any(|term| line.contains(term));

    has_alternative
        && query.required.iter().all(|term| line.contains(term))
        && query.excluded.iter().all(|term| !line.contains(term))
}

fn run_search(program: &str, term: &str, files: &[OsString]) -> io::Result<Output> {
    let mut command = Command::new(program);

    if program.ends_with("/rg") || program == "rg" {
        command.args([
            "--fixed-strings",
            "--line-number",
            "--with-filename",
            "--color",
            "never",
            "--no-heading",
            "--",
        ]);
    } else {
        command.args(["-F", "-n", "-H", "--"]);
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


fn filter_matches(query: &SearchQuery, matches: &str) -> String {
    let mut seen = std::collections::HashSet::new();
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

fn extract_callers_scope(
    args: &[String],
) -> Result<Option<(String, Option<String>)>, String> {
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
            "CVB pull: callers: must be followed by a Rust function or method name."
                .to_string(),
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
            if std::path::Path::new(path).extension() != Some(OsStr::new("rs")) {
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

fn run_callers(target: &str, scope: Option<&str>) -> i32 {
    let files = match tracked_files() {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let files = match filter_file_scope(files, scope) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let rust_files: Vec<OsString> = files
        .into_iter()
        .filter(|path| std::path::Path::new(path).extension() == Some(OsStr::new("rs")))
        .collect();

    let mut found = false;
    let mut incomplete = false;

    for path in rust_files {
        let display_path = path.to_string_lossy();

        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!(
                    "CVB pull: warning: unable to read Rust file {display_path}: {error}; \
                     caller results may be incomplete."
                );
                incomplete = true;
                continue;
            }
        };

        let matches = match rust_callers(&source, target) {
            Some(matches) => matches,
            None => {
                eprintln!(
                    "CVB pull: warning: skipping unparseable Rust file {display_path}; \
                     caller results may be incomplete."
                );
                incomplete = true;
                continue;
            }
        };

        for item in matches {
            found = true;
            let kind = match item.kind {
                RustCallKind::Function => "function call",
                RustCallKind::Method => "method-call candidate",
            };

            match item.enclosing_function.as_deref() {
                Some(function) => println!(
                    "{display_path}:{}: {kind} to {target} inside {function}",
                    item.line
                ),
                None => println!(
                    "{display_path}:{}: {kind} to {target} outside a function",
                    item.line
                ),
            }
        }
    }

    let status = callers_exit_status(found, incomplete);

    if status == 2 {
        eprintln!("CVB pull: caller scan incomplete; no reliable no-callers conclusion.");
    } else if status == 1 {
        println!("No callers found for: {target}");
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

pub fn run(args: &[String]) -> i32 {
    match extract_callers_scope(args) {
        Ok(Some((target, scope))) => return run_callers(&target, scope.as_deref()),
        Ok(None) => {}
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    }

    if args.len() == 1 {
        if let Some(file_path) = args[0].strip_prefix("file:") {
            if file_path.is_empty() {
                eprintln!("CVB pull: file: must be followed by a path.");
                return 2;
            }

            let files = match tracked_files() {
                Ok(files) => files,
                Err(error) => {
                    eprintln!("{error}");
                    return 2;
                }
            };

            if let Err(error) = filter_file_scope(files, Some(file_path)) {
                eprintln!("{error}");
                return 2;
            }

            let paths = [std::path::PathBuf::from(file_path)];
            return match crate::bridge_paths(&paths) {
                Ok(message) => {
                    eprintln!("{message}");
                    0
                }
                Err(error) => {
                    eprintln!("{error}");
                    1
                }
            };
        }
    }

    if args.is_empty() {
        eprintln!("Usage: cvb pull [file:PATH] <search-term>...");
        eprintln!("       cvb pull file:PATH lines:LINE|START-END");
        return 2;
    }

    let term = args.join(" ");
    let (scope, line_ranges, query_args) = match extract_file_scope(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    if !line_ranges.is_empty() && !query_args.is_empty() {
        eprintln!("CVB pull: lines: cannot be combined with search terms.");
        return 2;
    }

    let query = if !line_ranges.is_empty() {
        SearchQuery::default()
    } else {
        match parse_query(&query_args) {
            Ok(query) => query,
            Err(error) => {
                eprintln!("{error}");
                return 2;
            }
        }
    };

    let files = match tracked_files() {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let files = match filter_file_scope(files, scope.as_deref()) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    if let Some(path) = scope.as_deref().filter(|_| !line_ranges.is_empty()) {
        let mut output = String::new();
        for (start, end) in line_ranges {
            match read_line_range(path, start, end) {
                Ok(range_output) => output.push_str(&range_output),
                Err(error) => {
                    eprintln!("{error}");
                    return 2;
                }
            }
        }
        print!("{output}");
        return 0;
    }

    let search_terms: Vec<&str> = if !query.alternatives.is_empty() {
        query.alternatives.iter().map(String::as_str).collect()
    } else if let Some(required) = query.required.first() {
        vec![required.as_str()]
    } else {
        vec![""]
    };

    let mut combined = String::new();
    let mut combined_stderr = Vec::new();

    for search_term in search_terms {
        let output = match run_search("rg", search_term, &files) {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match run_search("/usr/bin/grep", search_term, &files) {
                    Ok(output) => output,
                    Err(error) => {
                        eprintln!("CVB pull: unable to launch grep: {error}");
                        return 2;
                    }
                }
            }
            Err(error) => {
                eprintln!("CVB pull: unable to launch rg: {error}");
                return 2;
            }
        };

        match output.status.code() {
            Some(0) => combined.push_str(&String::from_utf8_lossy(&output.stdout)),
            Some(1) => {}
            _ => {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
                eprintln!("CVB pull: search failed.");
                return 2;
            }
        }
        combined_stderr.extend_from_slice(&output.stderr);
    }

    let filtered = filter_matches(&query, &combined);

    if !combined_stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&combined_stderr));
    }

    if filtered.is_empty() {
        println!("No matches found for: {term}");
        return 1;
    }

    print!("{}", expand_rust_matches(&filtered));
    0
}


fn bash_function_range(source: &str, hit_line: usize) -> Option<(usize, usize)> {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    parser.set_language(&language.into()).ok()?;

    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    if root.has_error() {
        return None;
    }

    fn collect_ranges(node: tree_sitter::Node<'_>, ranges: &mut Vec<(usize, usize)>) {
        if node.kind() == "function_definition" {
            ranges.push((
                node.start_position().row + 1,
                node.end_position().row + 1,
            ));
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_ranges(child, ranges);
        }
    }

    let mut ranges = Vec::new();
    collect_ranges(root, &mut ranges);

    ranges
        .into_iter()
        .filter(|(start, end)| *start <= hit_line && hit_line <= *end)
        .min_by_key(|(start, end)| end - start)
}

fn rust_function_range(source: &str, hit_line: usize) -> Option<(usize, usize)> {
    fn collect_ranges(items: &[syn::Item], ranges: &mut Vec<(usize, usize)>) {
        use syn::spanned::Spanned;

        for item in items {
            match item {
                syn::Item::Fn(function) => {
                    let span = function.span();
                    ranges.push((span.start().line, span.end().line));
                }
                syn::Item::Struct(item) => {
                    let span = item.span();
                    ranges.push((span.start().line, span.end().line));
                }
                syn::Item::Enum(item) => {
                    let span = item.span();
                    ranges.push((span.start().line, span.end().line));
                }
                syn::Item::Impl(implementation) => {
                    for item in &implementation.items {
                        if let syn::ImplItem::Fn(function) = item {
                            let span = function.span();
                            ranges.push((span.start().line, span.end().line));
                        }
                    }
                }
                syn::Item::Trait(trait_item) => {
                    for item in &trait_item.items {
                        if let syn::TraitItem::Fn(function) = item {
                            let span = function.span();
                            ranges.push((span.start().line, span.end().line));
                        }
                    }
                }
                syn::Item::Mod(module) => {
                    if let Some((_, nested_items)) = &module.content {
                        collect_ranges(nested_items, ranges);
                    }
                }
                _ => {}
            }
        }
    }

    let file = syn::parse_file(source).ok()?;
    let mut ranges = Vec::new();
    collect_ranges(&file.items, &mut ranges);

    ranges
        .into_iter()
        .filter(|(start, end)| *start <= hit_line && hit_line <= *end)
        .min_by_key(|(start, end)| end - start)
}


#[derive(Debug, PartialEq, Eq)]
enum RustCallKind {
    Function,
    Method,
}

#[derive(Debug, PartialEq, Eq)]
struct RustCallerMatch {
    line: usize,
    enclosing_function: Option<String>,
    kind: RustCallKind,
}

fn rust_callers(source: &str, target: &str) -> Option<Vec<RustCallerMatch>> {
    use syn::spanned::Spanned;
    use syn::visit::{self, Visit};

    struct CallerVisitor<'a> {
        target: &'a str,
        enclosing_function: Option<String>,
        matches: Vec<RustCallerMatch>,
    }

    impl CallerVisitor<'_> {
        fn with_function(
            &mut self,
            name: String,
            visit_body: impl FnOnce(&mut Self),
        ) {
            let previous = self.enclosing_function.replace(name);
            visit_body(self);
            self.enclosing_function = previous;
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
                if path.path.segments.last().is_some_and(
                    |segment| segment.ident == self.target
                ) {
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

fn rust_expansion_ranges(
    source: &str,
    hit_line: usize,
) -> Vec<((usize, usize), &'static str)> {
    use syn::spanned::Spanned;

    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };

    let matched_type = file.items.iter().find_map(|item| {
        let (name, span, label) = match item {
            syn::Item::Struct(item) => (&item.ident, item.span(), "Rust struct"),
            syn::Item::Enum(item) => (&item.ident, item.span(), "Rust enum"),
            _ => return None,
        };

        (span.start().line <= hit_line && hit_line <= span.end().line)
            .then(|| (name.to_string(), (span.start().line, span.end().line), label))
    });

    if let Some((name, declaration, label)) = matched_type {
        let mut ranges = vec![(declaration, label)];

        for item in &file.items {
            let syn::Item::Impl(implementation) = item else {
                continue;
            };
            let syn::Type::Path(self_type) = implementation.self_ty.as_ref() else {
                continue;
            };

            if self_type.qself.is_some()
                || self_type.path.segments.last().map(|segment| segment.ident.to_string()).as_deref()
                    != Some(name.as_str())
            {
                continue;
            }

            let span = implementation.span();
            ranges.push(((span.start().line, span.end().line), "Rust impl"));
        }

        return ranges;
    }

    rust_function_range(source, hit_line)
        .map(|range| vec![(range, "Rust function")])
        .unwrap_or_default()
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

fn markdown_section_range(source: &str, match_line: usize) -> Option<(usize, usize)> {
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

fn expand_rust_matches(matches: &str) -> String {
    use std::collections::HashSet;
    use std::fs;

    let mut output = String::new();
    let mut shown_functions = HashSet::new();

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

        let ranges = if is_rust {
            rust_expansion_ranges(&source, line)
        } else if is_bash {
            bash_function_range(&source, line)
                .map(|range| vec![(range, "Bash function")])
                .unwrap_or_default()
        } else {
            markdown_section_range(&source, line)
                .map(|range| vec![(range, "Markdown section")])
                .unwrap_or_default()
        };

        for ((start, end), label) in ranges {
            let key = (path.to_string(), start, end);
            if !shown_functions.insert(key) {
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
    }

    output
}

#[cfg(test)]
mod pull_tests {
    use super::rust_function_range;

    #[test]
    fn callers_exit_status_is_success_when_matches_are_found() {
        assert_eq!(super::callers_exit_status(true, false), 0);
        assert_eq!(super::callers_exit_status(true, true), 0);
    }

    #[test]
    fn callers_exit_status_distinguishes_complete_and_incomplete_no_match() {
        assert_eq!(super::callers_exit_status(false, false), 1);
        assert_eq!(super::callers_exit_status(false, true), 2);
    }

    #[test]
    fn rust_callers_finds_unqualified_and_qualified_function_calls() {
        let source = r#"fn caller() {
    run();
    module::run();
}
"#;

        let matches = super::rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[0].kind, super::RustCallKind::Function);
        assert_eq!(matches[1].line, 3);
        assert_eq!(matches[1].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[1].kind, super::RustCallKind::Function);
    }

    #[test]
    fn rust_callers_finds_method_call_candidates() {
        let source = r#"fn caller(value: Example) {
    value.run();
}
"#;

        let matches = super::rust_callers(source, "run").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].enclosing_function.as_deref(), Some("caller"));
        assert_eq!(matches[0].kind, super::RustCallKind::Method);
    }

    #[test]
    fn rust_callers_ignores_mentions_in_comments_and_strings() {
        let source = r#"fn caller() {
    // run();
    let text = "run()";
}
"#;

        assert!(super::rust_callers(source, "run").unwrap().is_empty());
    }

    #[test]
    fn rust_callers_rejects_unparseable_source() {
        assert!(super::rust_callers("fn broken( {", "run").is_none());
    }

    #[test]
    fn extract_callers_scope_accepts_target_with_optional_file_scope() {
        assert_eq!(
            super::extract_callers_scope(&["callers:run".to_string()]).unwrap(),
            Some(("run".to_string(), None))
        );
        assert_eq!(
            super::extract_callers_scope(&[
                "file:src/pull.rs".to_string(),
                "callers:run".to_string(),
            ])
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
            vec!["file:src/pull.rs".to_string(), "callers:run".to_string(),
                 "file:src/main.rs".to_string()],
            vec!["file:manual.md".to_string(), "callers:run".to_string()],
            vec!["callers:module::run".to_string()],
        ] {
            assert!(
                super::extract_callers_scope(&args).is_err(),
                "expected selector combination to be rejected: {args:?}"
            );
        }
    }

    #[test]
    fn bash_function_range_finds_multiline_function_with_nested_control_flow() {
        let source = "target ()\n{\n    if true; then\n        echo nested\n    fi\n}\n";

        assert_eq!(super::bash_function_range(source, 4), Some((1, 6)));
    }

    #[test]
    fn bash_function_range_ignores_lines_outside_functions() {
        let source = "before() { echo before; }\n\ntarget() {\n    echo inside\n}\n";

        assert_eq!(super::bash_function_range(source, 2), None);
        assert_eq!(super::bash_function_range(source, 4), Some((3, 5)));
    }

    #[test]
    fn bash_function_range_rejects_unparseable_source() {
        assert_eq!(super::bash_function_range("target() { if then", 1), None);
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
    fn expansion_includes_impl_for_matched_struct() {
        let directory = std::env::temp_dir().join(format!(
            "cvb-pull-struct-impl-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();

        let file = directory.join("example.rs");
        let source = "struct Example {\n    value: usize,\n}\n\nimpl Example {\n    fn get(&self) -> usize {\n        self.value\n    }\n}\n";
        std::fs::write(&file, source).unwrap();

        let matches = format!("{}:1:struct Example {{", file.display());
        let output = super::expand_rust_matches(&matches);

        assert!(
            output.contains("Rust struct: ") && output.contains("1 | struct Example {"),
            "missing struct expansion:\n{output}"
        );
        assert!(
            output.contains("Rust impl: ") && output.contains("5 | impl Example {"),
            "missing associated impl expansion:\n{output}"
        );
        assert!(output.contains("9 | }"));

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn filter_file_scope_matches_exact_tracked_path() {
        use super::filter_file_scope;
        use std::ffi::OsString;

        let files = vec![
            OsString::from("src/pull.rs"),
            OsString::from("src/pull_tests.rs"),
            OsString::from("manual.md"),
        ];

        let filtered = filter_file_scope(files, Some("src/pull.rs")).unwrap();

        assert_eq!(filtered, vec![OsString::from("src/pull.rs")]);
    }

    #[test]
    fn filter_file_scope_without_scope_preserves_all_files() {
        use super::filter_file_scope;
        use std::ffi::OsString;

        let files = vec![OsString::from("src/pull.rs"), OsString::from("manual.md")];

        assert_eq!(filter_file_scope(files.clone(), None).unwrap(), files);
    }

    #[test]
    fn filter_file_scope_rejects_untracked_path() {
        use super::filter_file_scope;
        use std::ffi::OsString;

        let files = vec![OsString::from("src/pull.rs")];

        let error = filter_file_scope(files, Some("manual.md")).unwrap_err();

        assert!(error.contains("no Git-tracked file"));
        assert!(error.contains("manual.md"));
    }

    #[test]
    fn read_line_range_returns_requested_lines_with_original_numbers() {
        use super::read_line_range;

        let path = std::env::temp_dir().join(format!(
            "cvb-pull-lines-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let path_text = path.to_string_lossy();

        let single = read_line_range(&path_text, 2, 2).unwrap();
        assert_eq!(single, format!("{path_text}:2: beta\n"));

        let multiple = read_line_range(&path_text, 1, 3).unwrap();
        assert_eq!(
            multiple,
            format!(
                "{path_text}:1: alpha\n{path_text}:2: beta\n{path_text}:3: gamma\n"
            )
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_line_range_rejects_invalid_or_out_of_bounds_ranges() {
        use super::read_line_range;

        let path = std::env::temp_dir().join(format!(
            "cvb-pull-lines-bounds-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let path_text = path.to_string_lossy();

        assert!(read_line_range(&path_text, 0, 1).is_err());
        assert!(read_line_range(&path_text, 2, 1).is_err());
        assert!(read_line_range(&path_text, 1, 3).is_err());

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn parse_line_range_accepts_valid_inclusive_ranges() {
        use super::parse_line_range;

        assert_eq!(parse_line_range("155").unwrap(), (155, 155));
        assert_eq!(parse_line_range("1").unwrap(), (1, 1));
        assert_eq!(parse_line_range("155-215").unwrap(), (155, 215));
        assert_eq!(parse_line_range("1-1").unwrap(), (1, 1));
    }

    #[test]
    fn parse_line_range_rejects_invalid_ranges() {
        use super::parse_line_range;

        for range in ["", "0-10", "1-0", "20-10", "abc-20", "1-", "-10", "1-2-3"] {
            assert!(parse_line_range(range).is_err(), "accepted: {range}");
        }
    }

    #[test]
    fn extract_file_scope_preserves_query_operators() {
        use super::extract_file_scope;

        let args = vec![
            "file:src/pull.rs".into(),
            "protocol".into(),
            "+line-endings".into(),
            "-example".into(),
        ];

        let (scope, lines, query) = extract_file_scope(&args).unwrap();

        assert_eq!(scope.as_deref(), Some("src/pull.rs"));
        assert!(lines.is_empty());
        assert_eq!(
            query,
            vec!["protocol", "+line-endings", "-example"]
        );
    }

    #[test]
    fn extract_file_scope_parses_line_ranges() {
        use super::extract_file_scope;

        let args = vec![
            "file:src/pull.rs".into(),
            "lines:96-110".into(),
        ];

        let (scope, lines, query) = extract_file_scope(&args).unwrap();

        assert_eq!(scope.as_deref(), Some("src/pull.rs"));
        assert_eq!(lines, vec![(96, 110)]);
        assert!(query.is_empty());

        let single_line = vec![
            "file:manual.md".into(),
            "lines:155".into(),
        ];

        let (scope, lines, query) = extract_file_scope(&single_line).unwrap();

        assert_eq!(scope.as_deref(), Some("manual.md"));
        assert_eq!(lines, vec![(155, 155)]);
        assert!(query.is_empty());

        let multiple = extract_file_scope(&[
            "file:src/pull.rs".into(),
            "lines:96-110".into(),
            "lines:5-8".into(),
            "lines:7".into(),
        ]).unwrap();
        assert_eq!(multiple.0.as_deref(), Some("src/pull.rs"));
        assert_eq!(multiple.1, vec![(96, 110), (5, 8), (7, 7)]);
        assert!(multiple.2.is_empty());

        assert!(extract_file_scope(&["lines:1-3".into()]).is_err());
    }

    #[test]
    fn extract_file_scope_rejects_empty_or_multiple_scopes() {
        use super::extract_file_scope;

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
    fn query_bare_terms_use_or_semantics() {
        use super::{matches_query, parse_query};

        let query = parse_query(&["CRLF".into(), "BOM".into()]).unwrap();

        assert!(matches_query(&query, "file uses CRLF endings"));
        assert!(matches_query(&query, "file contains a BOM"));
        assert!(!matches_query(&query, "UTF-8 only"));
    }

    #[test]
    fn query_plus_terms_are_required() {
        use super::{matches_query, parse_query};

        let query = parse_query(&["CRLF".into(), "+BOM".into()]).unwrap();

        assert!(matches_query(&query, "CRLF and BOM"));
        assert!(!matches_query(&query, "CRLF only"));
        assert!(!matches_query(&query, "BOM only"));
    }

    #[test]
    fn query_minus_terms_are_excluded() {
        use super::{matches_query, parse_query};

        let query = parse_query(&["CRLF".into(), "-tests".into()]).unwrap();

        assert!(matches_query(&query, "CRLF implementation"));
        assert!(!matches_query(&query, "CRLF tests"));
    }

    #[test]
    fn query_combines_or_required_and_excluded_terms() {
        use super::{matches_query, parse_query};

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
    fn query_supports_quoted_phrases_with_operators() {
        use super::{matches_query, parse_query};

        let query = parse_query(&[
            "\"line ending\"".into(),
            "+\"invalid UTF-8\"".into(),
            "-\"unit test\"".into(),
        ])
        .unwrap();

        assert!(matches_query(
            &query,
            "line ending handling for invalid UTF-8 input"
        ));
        assert!(!matches_query(
            &query,
            "line ending handling for invalid UTF-8 unit test"
        ));
        assert!(!matches_query(
            &query,
            "line ending handling for invalid input"
        ));
    }

    #[test]
    fn query_rejects_empty_operator_terms() {
        use super::parse_query;

        assert!(parse_query(&["+".into()]).is_err());
        assert!(parse_query(&["-".into()]).is_err());
        assert!(parse_query(&["\"\"".into()]).is_err());
    }

    #[test]
    fn filter_matches_applies_query_to_record_content() {
        use super::{filter_matches, parse_query};

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
    fn filter_matches_deduplicates_records_preserving_order() {
        use super::{filter_matches, parse_query};

        let query = parse_query(&["needle".into()]).unwrap();
        let input = concat!(
            "src/a.rs:1:first needle\n",
            "src/a.rs:1:first needle\n",
            "src/b.rs:2:second needle\n",
            "src/a.rs:1:first needle\n",
        );

        assert_eq!(
            filter_matches(&query, input),
            concat!(
                "src/a.rs:1:first needle\n",
                "src/b.rs:2:second needle\n",
            )
        );
    }

    #[test]
    fn markdown_expansion_includes_nested_headings_and_deduplicates_section() {
        use super::expand_rust_matches;
        use std::fs;

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
        fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!(
            "{path_text}:4:first needle\n{path_text}:5:second needle\n"
        );

        let expanded = expand_rust_matches(&matches);
        let _ = fs::remove_file(&path);

        assert_eq!(expanded.matches("Markdown section:").count(), 1);
        assert!(expanded.contains(&format!(
            "Markdown section: {path_text}:3-7"
        )));
        assert!(expanded.contains("    6 | ### Details"));
        assert!(expanded.contains("    7 | Nested detail"));
        assert!(!expanded.contains("Sibling section"));
    }

    #[test]
    fn markdown_expansion_ignores_headings_inside_fenced_code() {
        use super::markdown_section_range;

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
        use super::expand_rust_matches;
        use std::fs;

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
        fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!("{path_text}:6:nested needle\n");

        let expanded = expand_rust_matches(&matches);
        let _ = fs::remove_file(&path);

        assert!(expanded.contains(&format!(
            "Markdown section: {path_text}:5-7"
        )));
        assert!(expanded.contains("    5 | ### Details"));
        assert!(expanded.contains("    6 | nested needle"));
        assert!(expanded.contains("    7 | More details"));
        assert!(!expanded.contains("Alpha content"));
        assert!(!expanded.contains("Sibling section"));
    }

    #[test]
    fn excluded_terms_filter_matches_before_markdown_expansion() {
        use super::{expand_rust_matches, filter_matches, parse_query};
        use std::fs;

        let path = std::env::temp_dir().join(format!(
            "cvb-pull-exclusion-expansion-{}.md",
            std::process::id()
        ));
        let source = concat!(
            "# Guide\n",
            "## surgery\n",
            "surgery techniques\n",
            "surgery protocol details\n",
            "More surgery techniques\n",
            "## Next section\n",
            "Unrelated content\n",
        );
        fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let input = format!(
            "{path_text}:3:surgery techniques\n\
             {path_text}:4:surgery protocol details\n\
             {path_text}:5:More surgery techniques\n"
        );
        let query = parse_query(&["+surgery".into(), "-protocol".into()]).unwrap();

        let filtered = filter_matches(&query, &input);
        assert!(!filtered.contains(":4:surgery protocol details"));
        assert!(filtered.contains(":3:surgery techniques"));
        assert!(filtered.contains(":5:More surgery techniques"));

        let expanded = expand_rust_matches(&filtered);
        let _ = fs::remove_file(&path);

        assert!(expanded.contains("4 | surgery protocol details"));
        assert!(!expanded.contains("Next section"));
    }

    #[test]
    fn expansion_deduplicates_function_blocks_and_preserves_matches() {
        use super::expand_rust_matches;
        use std::fs;

        let path = std::env::temp_dir().join(format!(
            "cvb-pull-dedup-{}.rs",
            std::process::id()
        ));
        let source = "fn target() {\n    let needle = 1;\n    assert_eq!(needle, 1);\n}\n";
        fs::write(&path, source).unwrap();

        let path_text = path.to_string_lossy();
        let matches = format!(
            "{path_text}:2:    let needle = 1;\n\
             {path_text}:3:    assert_eq!(needle, 1);\n\
             README.md:1:needle in documentation\n"
        );

        let expanded = expand_rust_matches(&matches);
        let _ = fs::remove_file(&path);

        assert_eq!(expanded.matches("Rust function:").count(), 1);
        assert!(expanded.contains(&format!("Rust function: {path_text}:1-4")));
        assert!(expanded.contains("README.md:1:needle in documentation"));
    }
}
