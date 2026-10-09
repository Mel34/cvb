use std::ffi::OsString;
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

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: cvb pull <search-term>...");
        return 2;
    }

    let term = args.join(" ");
    let query = match parse_query(args) {
        Ok(query) => query,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let files = match tracked_files() {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

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
