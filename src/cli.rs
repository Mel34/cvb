use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start,
    Target(Vec<String>),
    Pull(Vec<String>),
    Surgery(SurgeryArgs),
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurgeryArgs {
    pub dry_run: bool,
    pub stdin: bool,
    pub root: Option<std::path::PathBuf>,
    pub spec: bool,
}

pub fn parse() -> Command {
    let mut args = env::args().skip(1);

    match args.next().as_deref() {
        Some("--help") | Some("-h") => Command::Help,
        Some("--version") | Some("-V") => Command::Version,
        None => Command::Start,
        Some("pull") => Command::Pull(args.collect()),
        Some("surgery") => {
            let surgery_args = parse_surgery_args(args)
                .unwrap_or_else(|error| {
                    eprintln!("CVB: {error}");
                    std::process::exit(2);
                });
            Command::Surgery(surgery_args)
        }
        Some(first) => Command::Target(std::iter::once(first.to_string()).chain(args).collect()),
    }
}

fn parse_surgery_args<I>(args: I) -> Result<SurgeryArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut result = SurgeryArgs {
        dry_run: false,
        stdin: false,
        root: None,
        spec: false,
    };

    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--dry-run" => result.dry_run = true,
            "--stdin" => result.stdin = true,
            "--spec" => result.spec = true,
            "--root" => {
                let value = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| "missing argument for --root".to_string())?;

                result.root = Some(std::path::PathBuf::from(value));
            }
            _ if argument.starts_with("--root=") => {
                let value = &argument["--root=".len()..];

                if value.is_empty() {
                    return Err("missing argument for --root".to_string());
                }

                result.root = Some(std::path::PathBuf::from(value));
            }
            _ => return Err(format!("unknown surgery argument: {argument}")),
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{parse_surgery_args, SurgeryArgs};
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<SurgeryArgs, String> {
        parse_surgery_args(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn surgery_args_default_to_disabled() {
        assert_eq!(
            parse(&[]).unwrap(),
            SurgeryArgs {
                dry_run: false,
                stdin: false,
                root: None,
                spec: false,
            }
        );
    }

    #[test]
    fn parses_boolean_flags() {
        assert_eq!(
            parse(&["--dry-run", "--stdin", "--spec"]).unwrap(),
            SurgeryArgs {
                dry_run: true,
                stdin: true,
                root: None,
                spec: true,
            }
        );
    }

    #[test]
    fn parses_root_as_separate_argument() {
        assert_eq!(
            parse(&["--root", "/tmp/project"]).unwrap().root,
            Some(PathBuf::from("/tmp/project"))
        );
    }

    #[test]
    fn parses_root_with_equals() {
        assert_eq!(
            parse(&["--root=/tmp/project"]).unwrap().root,
            Some(PathBuf::from("/tmp/project"))
        );
    }

    #[test]
    fn rejects_missing_root_value() {
        assert_eq!(
            parse(&["--root"]).unwrap_err(),
            "missing argument for --root"
        );
    }

    #[test]
    fn rejects_root_value_that_is_an_option() {
        assert_eq!(
            parse(&["--root", "--stdin"]).unwrap_err(),
            "missing argument for --root"
        );
    }

    #[test]
    fn rejects_empty_root_equals_value() {
        assert_eq!(
            parse(&["--root="]).unwrap_err(),
            "missing argument for --root"
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert_eq!(
            parse(&["--unknown"]).unwrap_err(),
            "unknown surgery argument: --unknown"
        );
    }
}
