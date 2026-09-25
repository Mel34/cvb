use std::fmt::Write as FmtWrite;
use std::io::Write as IoWrite;
use std::path::Path;
mod cli;
mod control;
mod pty;
mod shell;
mod shell_state;

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
            if let Some(path) = target_path(&args) {
                                if path.is_file() {
                    match bridge_file(&path) {
                        Ok(()) => {}
                        Err(error) => {
                            eprintln!("{error}");
                            std::process::exit(1);
                        }
                    }
                    return;
                }

                if path.is_dir() {
                    match bridge_directory(&path) {
                        Ok(()) => {}
                        Err(error) => {
                            eprintln!("{error}");
                            std::process::exit(1);
                        }
                    }
                    return;
                }

                println!("CVB path target: {}", path.display());
                return;
            }

            let mut command = std::process::Command::new(&args[0]);
            command.args(&args[1..]);

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

fn target_path(args: &[String]) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(&args[0]);

    if path.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn bridge_file(path: &std::path::Path) -> Result<(), String> {
    let contents = std::fs::read(path).map_err(|error| {
        format!(
            "CVB: unable to read {}: {error}",
            path.display()
        )
    })?;

    let mut child = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&contents)
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

fn bridge_directory(path: &Path) -> Result<(), String> {
    let mut output = String::from("path\ttype\tsize\n");

    append_directory_entries(path, path, &mut output)?;

    let mut child = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(output.as_bytes())
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

fn append_directory_entries(
    root: &Path,
    directory: &Path,
    output: &mut String,
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
            writeln!(output, "{relative_path}\tdir\t-")
                .map_err(|error| format!("CVB: unable to build directory inventory: {error}"))?;

            append_directory_entries(root, &entry_path, output)?;
        } else if entry_path.is_file() {
            let size = entry.metadata().map_err(|error| {
                format!("CVB: unable to read metadata for {}: {error}", entry_path.display())
            })?.len();

            writeln!(output, "{relative_path}\tfile\t{size}")
                .map_err(|error| format!("CVB: unable to build directory inventory: {error}"))?;
        }
    }

    Ok(())
}