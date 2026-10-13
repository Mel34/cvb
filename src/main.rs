use std::fmt::Write as FmtWrite;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
mod cli;
pub(crate) mod clipboard;
mod control;
mod keyboard;
mod keybind;
mod pty;
mod pull;
mod shell;
mod sound;
///mod surgery;

const DEFAULT_IGNORE_LIST: &str = "\
# This file contains directory names that CVB excludes from directory inventories.
# One directory name per line. Blank lines and lines starting with # are ignored.

.git
.hg
.svn
.bzr
.jj
";

const INVENTORY_HEADER: &str =
    "path\tname\ttype\tsize\tmodified\tcreated\tsymlink\ttarget\tpermissions\towner\tgroup\n";

fn main() {
    match cli::parse() {
        cli::Command::Start => match shell::create_rcfile() {
            Ok(rcfile) => {
                let result = pty::run(&rcfile);
                let _ = std::fs::remove_file(&rcfile);

                if let Err(error) = result {
                    eprintln!("{error}");
                }
            }
            Err(error) => eprintln!("{error}"),
        },
        cli::Command::Help => println!(
            "CVB — Capture → View → Bridge

Capture your terminal session and bridge command output to the clipboard.

Usage:
    cvb
    cvb <PATH>...
    cvb pull [file:PATH] <search-term>...
    cvb --help
    cvb --version

Modes:
    cvb                      Start a terminal capture session
    cvb <PATH>...            Capture files or directory inventories
    cvb pull [file:PATH] <search-term>... Search Git-tracked source files

Options:
    -h, --help       Show this help message
    -V, --version    Show version information

A capture session ends with:
    exit"
        ),
        cli::Command::Version => println!("cvb {}", env!("CARGO_PKG_VERSION")),
        cli::Command::Pull(args) => {
            if args.is_empty() {
                eprintln!("Usage: cvb pull [file:PATH] <search-term>...");
                std::process::exit(2);
            }

            std::process::exit(pull::run(&args));
        }
        cli::Command::Surgery => {
            println!("Paste CSL block");
        }
        cli::Command::Target(args) => {
            if std::env::var_os("CVB_INTERNAL_TARGET").is_some() {
                run_internal_target(&args);
            }

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

fn emit_pathop_marker() {
    if let Ok(mut tty) = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
    {
        let _ = std::io::Write::write_all(
            &mut tty,
            b"\x1b]777;CVB;PATHOP\x07",
        );
    }
}

fn run_internal_target(args: &[String]) -> ! {
    if args.is_empty() {
        eprintln!("CVB: internal target request has no arguments");
        std::process::exit(1);
    }

    let fd = unsafe { OwnedFd::from_raw_fd(3) };
    let mut control = control::ControlChannel::new(fd);
    let id = std::process::id() as u64;

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd.to_string_lossy().into_owned(),
        Err(error) => {
            eprintln!("CVB: unable to determine working directory: {error}");
            std::process::exit(1);
        }
    };

    let request = control::ControlMessage::TargetRequest {
        id,
        cwd,
        args: args.to_vec(),
    };

    emit_pathop_marker();
    if let Err(error) = control.send(&request) {
        eprintln!("{error}");
        std::process::exit(1);
    }

    loop {
        match control.receive_one() {
            Ok(control::ControlMessage::TargetResponse {
                id: response_id,
                status,
                message,
            }) if response_id == id => {
                if !message.is_empty() {
                    eprintln!("{message}");
                }
                std::process::exit(status);
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
}

fn target_paths(args: &[String]) -> Option<Vec<std::path::PathBuf>> {
    let paths = args
        .iter()
        .map(std::path::Path::new)
        .map(|path| {
            if path.exists() {
                Some(path.to_path_buf())
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()?;

    Some(paths)
}

pub(crate) fn bridge_paths(
    paths: &[std::path::PathBuf],
) -> Result<(), String> {
    let (contents, _message) = build_bridge_output(paths)?;
    let buffer = clipboard::ClipboardBuffer::new(contents);
    clipboard::process(&buffer)?;
    Ok(())
}

pub(crate) fn build_bridge_output(
    paths: &[std::path::PathBuf],
) -> Result<(Vec<u8>, String), String> {
    if paths.len() == 1 {
        let path = &paths[0];

        if path.is_file() {
            let contents = std::fs::read(path)
                .map_err(|error| {
                    format!("CVB: unable to read {}: {error}", path.display())
                })?;
            let size = std::fs::metadata(path)
                .map_err(|error| {
                    format!(
                        "CVB: unable to read metadata for '{}': {error}",
                        path.display()
                    )
                })?
                .len();

            return Ok((
                contents,
                format!(
                    "CVB: captured file '{}' ({} bytes)",
                    path.display(),
                    size
                ),
            ));
        }

        if path.is_dir() {
            let ignore = load_ignore_list()?;
            let mut output = String::from(INVENTORY_HEADER);
            append_directory_entries(path, path, &mut output, &ignore)?;
            let entry_count = output.lines().skip(1).count();
            let message = format!(
                "CVB: captured directory inventory '{}' ({} {})",
                path.display(),
                entry_count,
                if entry_count == 1 { "entry" } else { "entries" }
            );

            return Ok((output.into_bytes(), message));
        }
    }

    let output = build_paths_output(paths)?;
    Ok((
        output.into_bytes(),
        format!("CVB: captured {} paths", paths.len()),
    ))
}

fn build_path_leaves(
    paths: &[std::path::PathBuf],
    base_dir: &std::path::Path,
) -> Vec<(String, Vec<u8>, bool)> {
    paths
        .iter()
        .map(|path| {
            let label = format!("[{}]", path.display());
            let resolved_path = if path.is_absolute() {
                path.clone()
            } else {
                base_dir.join(path)
            };

            let result: Result<Vec<u8>, String> = if resolved_path.is_file() {
                std::fs::read(&resolved_path)
                    .map_err(|error| format!("Error reading the file: {error}"))
            } else if resolved_path.is_dir() {
                (|| {
                    let ignore = load_ignore_list()?;
                    let mut output = String::from(INVENTORY_HEADER);
                    append_directory_entries(
                        &resolved_path,
                        &resolved_path,
                        &mut output,
                        &ignore,
                    )?;
                    Ok(output.into_bytes())
                })()
                .map_err(|error: String| {
                    format!("Error reading the directory inventory: {error}")
                })
            } else {
                Err(format!("Error: unsupported path: {}", path.display()))
            };

            let (contents, succeeded) = match result {
                Ok(contents) => (contents, true),
                Err(error) => (error.into_bytes(), false),
            };
            (label, contents, succeeded)
        })
        .collect()
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

            output.push_str(INVENTORY_HEADER);

            let ignore = load_ignore_list()?;
            append_directory_entries(path, path, &mut output, &ignore)?;
        } else {
            return Err(format!("CVB: unsupported path: {}", path.display()));
        }
    }

    Ok(output)
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

        std::fs::write(&ignore_file, DEFAULT_IGNORE_LIST).map_err(|error| {
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
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().into_owned();

        let relative_path = entry_path
            .strip_prefix(root)
            .map_err(|error| format!("CVB: unable to determine relative path: {error}"))?;

        let relative_path = relative_path.to_string_lossy();

        let metadata = std::fs::symlink_metadata(&entry_path).map_err(|error| {
            format!(
                "CVB: unable to read metadata for {}: {error}",
                entry_path.display()
            )
        })?;

        let is_symlink = metadata.file_type().is_symlink();

        if is_symlink {
            let target = std::fs::read_link(&entry_path)
                .map_err(|error| {
                    format!(
                        "CVB: unable to read symlink target for {}: {error}",
                        entry_path.display()
                    )
                })?
                .to_string_lossy()
                .into_owned();

            append_inventory_entry(
                output,
                &relative_path,
                &name,
                "symlink",
                "-",
                &metadata,
                true,
                &target,
            )?;

            continue;
        }

        if metadata.is_dir() {
            if ignore.iter().any(|ignored| ignored == &name) {
                continue;
            }

            append_inventory_entry(
                output,
                &relative_path,
                &name,
                "dir",
                "-",
                &metadata,
                false,
                "-",
            )?;

            append_directory_entries(root, &entry_path, output, ignore)?;
        } else if metadata.is_file() {
            append_inventory_entry(
                output,
                &relative_path,
                &name,
                "file",
                &metadata.len().to_string(),
                &metadata,
                false,
                "-",
            )?;
        } else {
            append_inventory_entry(
                output,
                &relative_path,
                &name,
                "other",
                "-",
                &metadata,
                false,
                "-",
            )?;
        }
    }

    Ok(())
}

fn append_inventory_entry(
    output: &mut String,
    path: &str,
    name: &str,
    entry_type: &str,
    size: &str,
    metadata: &std::fs::Metadata,
    symlink: bool,
    target: &str,
) -> Result<(), String> {
    let modified = format_system_time(
        metadata
            .modified()
            .map_err(|error| format!("CVB: unable to read modification time: {error}"))?,
    )?;

    let created = match metadata.created() {
        Ok(time) => format_system_time(time)?,
        Err(_) => "-".to_string(),
    };

    let permissions = format!("{:04o}", metadata.permissions().mode() & 0o7777);
    let owner = user_name(metadata.uid());
    let group = group_name(metadata.gid());

    writeln!(
        output,
        "{path}\t{name}\t{entry_type}\t{size}\t{modified}\t{created}\t{}\t{target}\t{permissions}\t{owner}\t{group}",
        if symlink { "yes" } else { "no" }
    )
    .map_err(|error| format!("CVB: unable to build directory inventory: {error}"))
}

fn format_system_time(time: std::time::SystemTime) -> Result<String, String> {
    let duration = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("CVB: unable to format timestamp: {error}"))?;

    format_unix_timestamp(duration.as_secs() as i64)
}

fn format_unix_timestamp(timestamp: i64) -> Result<String, String> {
    let days = timestamp.div_euclid(86_400);
    let seconds = timestamp.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days)?;

    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;

    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn civil_from_days(days: i64) -> Result<(i64, i64, i64), String> {
    let z = days + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    if !(1..=9999).contains(&year) {
        return Err(format!("CVB: timestamp year is out of range: {year}"));
    }

    Ok((year, month, day))
}

fn user_name(uid: u32) -> String {
    unsafe {
        let passwd = libc::getpwuid(uid);

        if passwd.is_null() || (*passwd).pw_name.is_null() {
            return uid.to_string();
        }

        std::ffi::CStr::from_ptr((*passwd).pw_name)
            .to_string_lossy()
            .into_owned()
    }
}

fn group_name(gid: u32) -> String {
    unsafe {
        let group = libc::getgrgid(gid);

        if group.is_null() || (*group).gr_name.is_null() {
            return gid.to_string();
        }

        std::ffi::CStr::from_ptr((*group).gr_name)
            .to_string_lossy()
            .into_owned()
    }
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
    fn target_paths_requires_every_path_to_exist() {
        let directory = test_directory("target-paths");
        let existing = directory.join("existing.txt");
        let missing = directory.join("missing.txt");

        fs::write(&existing, "contents").unwrap();

        let args = vec![existing.display().to_string()];
        assert_eq!(
            target_paths(&args),
            Some(vec![existing.clone()])
        );

        let args = vec![
            existing.display().to_string(),
            missing.display().to_string(),
        ];
        assert_eq!(target_paths(&args), None);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn format_system_time_rejects_time_before_epoch() {
        let time = std::time::UNIX_EPOCH - std::time::Duration::from_secs(1);

        assert_eq!(
            format_system_time(time).unwrap_err(),
            "CVB: unable to format timestamp: second time provided was later than self"
        );
    }

    #[test]
    fn format_unix_timestamp_formats_epoch() {
        assert_eq!(
            format_unix_timestamp(0).unwrap(),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn civil_from_days_rejects_year_before_one() {
        let result = civil_from_days(-719_469);

        assert_eq!(
            result.unwrap_err(),
            "CVB: timestamp year is out of range: 0"
        );
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

        let mut output = String::from(INVENTORY_HEADER);

        append_directory_entries(
            &directory,
            &directory,
            &mut output,
            &["node_modules".to_string()],
        )
        .unwrap();

        assert!(!output.contains("node_modules"));
        assert!(output.contains("src\tsrc\tdir\t-"));
        assert!(output.contains("src/main.rs\tmain.rs\tfile\t8"));

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

        let mut output = String::from(INVENTORY_HEADER);

        append_directory_entries(
            &directory,
            &directory,
            &mut output,
            &[".git".to_string()],
        )
        .unwrap();

        assert!(!output.contains(".git"));
        assert!(output.contains("src\tsrc\tdir\t-"));
        assert!(output.contains("src/main.rs\tmain.rs\tfile\t8"));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn directory_inventory_contains_metadata() {
        let directory = test_directory("metadata");
        let file = directory.join("test.txt");

        fs::write(&file, "contents").unwrap();

        let mut output = String::from(INVENTORY_HEADER);

        append_directory_entries(&directory, &directory, &mut output, &[]).unwrap();

        let line = output
            .lines()
            .find(|line| line.starts_with("test.txt\t"))
            .unwrap();

        let fields = line.split('\t').collect::<Vec<_>>();

        assert_eq!(fields.len(), 11);
        assert_eq!(fields[0], "test.txt");
        assert_eq!(fields[1], "test.txt");
        assert_eq!(fields[2], "file");
        assert_eq!(fields[3], "8");
        assert!(fields[4] != "-");
        assert!(fields[5] != "-");
        assert_eq!(fields[6], "no");
        assert_eq!(fields[7], "-");
        assert!(fields[8].starts_with("0"));
        assert!(!fields[9].is_empty());
        assert!(!fields[10].is_empty());

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directory_inventory_reports_symlinks() {
        let directory = test_directory("symlink");
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");

        fs::write(&target, "target").unwrap();
        std::os::unix::fs::symlink("target.txt", &link).unwrap();

        let mut output = String::from(INVENTORY_HEADER);

        append_directory_entries(&directory, &directory, &mut output, &[]).unwrap();

        let line = output
            .lines()
            .find(|line| line.starts_with("link.txt\t"))
            .unwrap();

        let fields = line.split('\t').collect::<Vec<_>>();

        assert_eq!(fields[0], "link.txt");
        assert_eq!(fields[1], "link.txt");
        assert_eq!(fields[2], "symlink");
        assert_eq!(fields[3], "-");
        assert_eq!(fields[6], "yes");
        assert_eq!(fields[7], "target.txt");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bridge_output_builds_single_file_without_writing_clipboard() {
        let directory = test_directory("bridge-output-single");
        let file = directory.join("test.txt");
        fs::write(&file, b"file contents\n").unwrap();

        let (contents, message) =
            build_bridge_output(std::slice::from_ref(&file)).unwrap();

        assert_eq!(contents, b"file contents\n");
        assert_eq!(
            message,
            format!(
                "CVB: captured file '{}' ({} bytes)",
                file.display(),
                b"file contents\n".len()
            )
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bridge_output_builds_multiple_files_without_writing_clipboard() {
        let directory = test_directory("bridge-output-multiple");
        let first = directory.join("first.txt");
        let second = directory.join("second.txt");
        fs::write(&first, "first contents\n").unwrap();
        fs::write(&second, "second contents\n").unwrap();

        let (contents, message) =
            build_bridge_output(&[first.clone(), second.clone()]).unwrap();

        assert_eq!(
            String::from_utf8(contents).unwrap(),
            format!(
                "--- {} ---\nfirst contents\n\n--- {} ---\nsecond contents\n",
                first.display(),
                second.display()
            )
        );
        assert_eq!(message, "CVB: captured 2 paths");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bridge_output_builds_directory_inventory_without_writing_clipboard() {
        let directory = test_directory("bridge-output-directory");
        fs::write(directory.join("file.txt"), "contents").unwrap();

        let (contents, message) =
            build_bridge_output(std::slice::from_ref(&directory)).unwrap();
        let output = String::from_utf8(contents).unwrap();

        assert!(output.starts_with(INVENTORY_HEADER));
        assert!(output.contains("file.txt\tfile.txt\tfile\t8"));
        assert!(message.starts_with(&format!(
            "CVB: captured directory inventory '{}'",
            directory.display()
        )));

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
    fn unsupported_path_returns_error() {
        let directory = test_directory("unsupported-path");
        let unsupported = directory.join("unsupported");

        let output = build_paths_output(&[unsupported.clone()]);

        assert_eq!(
            output.unwrap_err(),
            format!("CVB: unsupported path: {}", unsupported.display())
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn path_leaves_preserve_order_and_continue_after_failure() {
        let directory = test_directory("path-leaves-order");
        let missing = directory.join("missing.txt");
        let file = directory.join("second.txt");

        fs::write(&file, "second contents\n").unwrap();

        let leaves = build_path_leaves(&[missing.clone(), file.clone()], std::path::Path::new("."));

        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].0, format!("[{}]", missing.display()));
        assert_eq!(
            String::from_utf8(leaves[0].1.clone()).unwrap(),
            format!("Error: unsupported path: {}", missing.display())
        );
        assert!(!leaves[0].2);
        assert_eq!(leaves[1].0, format!("[{}]", file.display()));
        assert_eq!(leaves[1].1, b"second contents\n");
        assert!(leaves[1].2);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn path_leaves_preserve_file_bytes_literally() {
        let directory = test_directory("path-leaves-literal");
        let file = directory.join("binary.txt");
        let contents = vec![b'a', 0xff, b'\n'];

        fs::write(&file, &contents).unwrap();

        let leaves = build_path_leaves(std::slice::from_ref(&file), std::path::Path::new("."));

        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].0, format!("[{}]", file.display()));
        assert_eq!(leaves[0].1, contents);
        assert!(leaves[0].2);

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

        let expected_prefix = format!(
            "--- {} ---\nfile contents\n\n--- {} ---\n{}",
            file.display(),
            subdirectory.display(),
            INVENTORY_HEADER
        );

        assert!(output.starts_with(&expected_prefix));

        let line = output
            .lines()
            .find(|line| line.starts_with("nested.txt\t"))
            .unwrap();

        let fields = line.split('\t').collect::<Vec<_>>();

        assert_eq!(fields.len(), 11);
        assert_eq!(fields[0], "nested.txt");
        assert_eq!(fields[1], "nested.txt");
        assert_eq!(fields[2], "file");
        assert_eq!(fields[3], "16");
        assert!(fields[4] != "-");
        assert!(fields[5] != "-");
        assert_eq!(fields[6], "no");
        assert_eq!(fields[7], "-");
        assert!(fields[8].starts_with("0"));
        assert!(!fields[9].is_empty());
        assert!(!fields[10].is_empty());

        fs::remove_dir_all(directory).unwrap();
    }
}
