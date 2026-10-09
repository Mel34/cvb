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

pub fn run(args: &[String]) -> i32 {
    let term = args.join(" ");
    if term.is_empty() {
        eprintln!("Usage: cvb pull <search-term>...");
        return 2;
    }

    let files = match tracked_files() {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let output = match run_search("rg", &term, &files) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match run_search("/usr/bin/grep", &term, &files) {
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
        Some(0) => {
            let matches = String::from_utf8_lossy(&output.stdout);
            print!("{}", expand_rust_matches(&matches));
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            0
        }
        Some(1) => {
            println!("No matches found for: {term}");
            1
        }
        _ => {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            eprintln!("CVB pull: search failed.");
            2
        }
    }
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
        if !is_rust && !is_bash {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };

        let ranges = if is_rust {
            rust_expansion_ranges(&source, line)
        } else {
            bash_function_range(&source, line)
                .map(|range| vec![(range, "Bash function")])
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
