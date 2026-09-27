use std::fmt::Write as FmtWrite;
use std::io::Write as IoWrite;
use std::path::Path;
mod cli;
mod control;
mod input;
mod keyboard;
mod pty;
mod shell;

fn main() {
    match cli::parse() {
        cli::Command::On => match shell::create_rcfile() {
            Ok(rcfile) => match pty::run(&rcfile) {
                Ok(_) => {}
                Err(error) => eprintln!("{error}"),
            },
            Err(error) => eprintln!("{error}"),
        },
        cli::Command::Off => println!("CVB off"),
        cli::Command::Help => println!("CVB — Capture → View → Bridge"),
        cli::Command::Version => println!("cvb 0.1.0"),
        cli::Command::Target(args) => {
            if let Some(paths) = target_paths(&args) {
                match bridge_paths(&paths) {
                    Ok(()) => {}
                    Err(error) => {
                        eprintln!("{error}");
                        std::process::exit(1);
                    }
                }
                return;
            }

            let mut command = std::process::Command::new(&args[0]);
            command.args(&args[1..]);
            command.env("PAGER", "cat");
            command.env("GIT_PAGER", "cat");
            command.env("SYSTEMD_PAGER", "cat");

            match command.status() {
                Ok(status) => {
                    std::process::exit(status.code().unwrap_or(1));
                }
                Err(error) => {
                    eprintln!("CVB: unable to execute {}: {error}", args[0]);
                    std::process::exit(1);
                }
            }
        }
    }
}

fn target_paths(args: &[String]) -> Option<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();

    for arg in args {
        if let Ok(matches) = glob::glob(arg) {
            let matches = matches.filter_map(Result::ok).collect::<Vec<_>>();

            if !matches.is_empty() {
                paths.extend(matches);
                continue;
            }
        }

        let path = std::path::Path::new(arg);

        if path.exists() {
            paths.push(path.to_path_buf());
        } else {
            return None;
        }
    }

    Some(paths)
}

fn bridge_paths(paths: &[std::path::PathBuf]) -> Result<(), String> {
    if paths.len() == 1 {
        let path = &paths[0];

        if path.is_file() {
            return bridge_file(path);
        }

        if path.is_dir() {
            return bridge_directory(path);
        }
    }

    let output = build_paths_output(paths)?;
    copy_to_clipboard(output.as_bytes())
}

fn build_paths_output(paths: &[std::path::PathBuf]) -> Result<String, String> {
    let mut output = String::new();

    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }

        if path.is_file() {
            writeln!(output, "--- {} ---", path.display())
                .map_err(|error| format!("CVB: unable to build file output: {error}"))?;

            let contents = std::fs::read(path)
                .map_err(|error| format!("CVB: unable to read {}: {error}", path.display()))?;

            output.push_str(&String::from_utf8_lossy(&contents));
        } else if path.is_dir() {
            writeln!(output, "--- {} ---", path.display())
                .map_err(|error| format!("CVB: unable to build directory output: {error}"))?;

            let ignore = load_ignore_list()?;
            append_directory_entries(path, path, &mut output, &ignore)?;
        } else {
            return Err(format!("CVB: unsupported path: {}", path.display()));
        }
    }

    Ok(output)
}

fn bridge_file(path: &Path) -> Result<(), String> {
    let contents = std::fs::read(path)
        .map_err(|error| format!("CVB: unable to read {}: {error}", path.display()))?;

    copy_to_clipboard(&contents)
}

fn copy_to_clipboard(contents: &[u8]) -> Result<(), String> {
    let mut child = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(contents)
            .map_err(|error| format!("CVB: unable to write to wl-copy: {error}"))?;
    }

    let status = child
        .wait()
        .map_err(|error| format!("CVB: unable to wait for wl-copy: {error}"))?;

    if !status.success() {
        return Err(format!("CVB: wl-copy exited with status {status}"));
    }

    Ok(())
}

fn parse_ignore_list(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn load_ignore_list() -> Result<Vec<String>, String> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| "CVB: unable to determine config directory".to_string())?;

    let config_dir = config_dir.join("cvb");
    let ignore_file = config_dir.join("ignore");

    if !ignore_file.exists() {
        std::fs::create_dir_all(&config_dir).map_err(|error| {
            format!(
                "CVB: unable to create config directory {}: {error}",
                config_dir.display()
            )
        })?;

        let default_contents = "\
# This file contains directory names that CVB excludes from directory inventories.
# One directory name per line. Blank lines and lines starting with # are ignored.

.git
.hg
.svn
.bzr
.jj
";

        std::fs::write(&ignore_file, default_contents).map_err(|error| {
            format!(
                "CVB: unable to create ignore file {}: {error}",
                ignore_file.display()
            )
        })?;
    }

    let contents = std::fs::read_to_string(&ignore_file).map_err(|error| {
        format!(
            "CVB: unable to read ignore file {}: {error}",
            ignore_file.display()
        )
    })?;

    Ok(parse_ignore_list(&contents))
}

fn bridge_directory(path: &Path) -> Result<(), String> {
    let ignore = load_ignore_list()?;
    let mut output = String::from("path\ttype\tsize\n");

    append_directory_entries(path, path, &mut output, &ignore)?;

    copy_to_clipboard(output.as_bytes())
}

fn append_directory_entries(
    root: &Path,
    directory: &Path,
    output: &mut String,
    ignore: &[String],
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| format!("CVB: unable to read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "CVB: unable to read directory entry in {}: {error}",
                directory.display()
            )
        })?;

    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let entry_path = entry.path();
        let relative_path = entry_path
            .strip_prefix(root)
            .map_err(|error| format!("CVB: unable to determine relative path: {error}"))?;

        let relative_path = relative_path.to_string_lossy();

        if entry_path.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();

            if ignore.iter().any(|ignored| ignored == &name) {
                continue;
            }

            writeln!(output, "{relative_path}\tdir\t-")
                .map_err(|error| format!("CVB: unable to build directory inventory: {error}"))?;

            append_directory_entries(root, &entry_path, output, ignore)?;
        } else if entry_path.is_file() {
            let size = entry
                .metadata()
                .map_err(|error| {
                    format!(
                        "CVB: unable to read metadata for {}: {error}",
                        entry_path.display()
                    )
                })?
                .len();

            writeln!(output, "{relative_path}\tfile\t{size}")
                .map_err(|error| format!("CVB: unable to build directory inventory: {error}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_directory(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("cvb-test-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parse_ignore_list_ignores_comments_and_blank_lines() {
        let contents = "\
    # comment

    .git
        target

    # another comment
    ";

        assert_eq!(parse_ignore_list(contents), vec![".git", "target"]);
    }

    #[test]
    fn parse_ignore_list_preserves_directory_names() {
        let contents = "\
    .git
    node_modules
    __pycache__
    ";

        assert_eq!(
            parse_ignore_list(contents),
            vec![".git", "node_modules", "__pycache__"]
        );
    }
    #[test]
    fn directory_inventory_excludes_ignored_directories_at_any_depth() {
        let directory = test_directory("ignore-nested");
        let nested = directory.join("src").join("node_modules");
        let included = directory.join("src").join("main.rs");

        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("package.json"), "ignored").unwrap();
        fs::write(&included, "included").unwrap();

        let mut output = String::new();
        append_directory_entries(
            &directory,
            &directory,
            &mut output,
            &["node_modules".to_string()],
        )
        .unwrap();

        assert!(!output.contains("node_modules"));
        assert!(output.contains("src\tdir\t-"));
        assert!(output.contains("src/main.rs\tfile\t8"));

        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn directory_inventory_excludes_ignored_directories() {
        let directory = test_directory("ignore");
        let ignored = directory.join(".git");
        let included = directory.join("src");

        fs::create_dir_all(&ignored).unwrap();
        fs::write(ignored.join("config"), "ignored").unwrap();

        fs::create_dir_all(&included).unwrap();
        fs::write(included.join("main.rs"), "included").unwrap();

        let mut output = String::new();
        append_directory_entries(&directory, &directory, &mut output, &[".git".to_string()])
            .unwrap();

        assert!(!output.contains(".git"));
        assert!(output.contains("src\tdir\t-"));
        assert!(output.contains("src/main.rs\tfile\t8"));

        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn multiple_files_are_combined_with_headers() {
        let directory = test_directory("multiple-files");
        let first = directory.join("first.txt");
        let second = directory.join("second.txt");

        fs::write(&first, "first contents\n").unwrap();
        fs::write(&second, "second contents\n").unwrap();

        let output = build_paths_output(&[first.clone(), second.clone()]).unwrap();

        assert_eq!(
            output,
            format!(
                "--- {} ---\nfirst contents\n\n--- {} ---\nsecond contents\n",
                first.display(),
                second.display()
            )
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn single_file_output_is_unchanged() {
        let directory = test_directory("single-file");
        let file = directory.join("test.txt");

        fs::write(&file, "file contents\n").unwrap();

        let output = build_paths_output(std::slice::from_ref(&file)).unwrap();

        assert_eq!(
            output,
            format!("--- {} ---\nfile contents\n", file.display())
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_and_directory_are_combined() {
        let directory = test_directory("file-and-directory");
        let file = directory.join("test.txt");
        let subdirectory = directory.join("subdir");
        let nested_file = subdirectory.join("nested.txt");

        fs::write(&file, "file contents\n").unwrap();
        fs::create_dir(&subdirectory).unwrap();
        fs::write(&nested_file, "nested contents\n").unwrap();

        let output = build_paths_output(&[file.clone(), subdirectory.clone()]).unwrap();

        assert_eq!(
            output,
            format!(
                "--- {} ---\nfile contents\n\n--- {} ---\nnested.txt\tfile\t16\n",
                file.display(),
                subdirectory.display()
            )
        );

        fs::remove_dir_all(directory).unwrap();
    }
}
