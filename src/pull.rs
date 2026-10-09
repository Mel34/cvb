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


fn rust_function_range(source: &str, hit_line: usize) -> Option<(usize, usize)> {
    fn collect_ranges(items: &[syn::Item], ranges: &mut Vec<(usize, usize)>) {
        use syn::spanned::Spanned;

        for item in items {
            match item {
                syn::Item::Fn(function) => {
                    let span = function.span();
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

        if !path.ends_with(".rs") {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let Some((start, end)) = rust_function_range(&source, line) else {
            continue;
        };

        let key = (path.to_string(), start, end);
        if !shown_functions.insert(key) {
            continue;
        }

        output.push_str(&format!("  Rust function: {path}:{start}-{end}\n"));

        for (index, source_line) in source.lines().enumerate() {
            let number = index + 1;
            if start <= number && number <= end {
                output.push_str(&format!("  {number:>5} | {source_line}\n"));
            }
        }
    }

    output
}

#[cfg(test)]
mod pull_tests {
    use super::rust_function_range;

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
