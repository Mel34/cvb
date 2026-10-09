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
            print!("{}", String::from_utf8_lossy(&output.stdout));
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
