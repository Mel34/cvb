use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start,
    Target(Vec<String>),
    Pull(Vec<String>),
    Context,
    Repaste,
    Help,
    Version,
}

pub fn parse() -> Command {
    parse_args(env::args().skip(1))
}

pub fn parse_args<I>(mut args: I) -> Command
where
    I: Iterator<Item = String>,
{
    let first = args.next();
    let rest: Vec<String> = args.collect();

    match first.as_deref() {
        Some("--help") | Some("-h") => Command::Help,
        Some("--version") | Some("-V") => Command::Version,
        None => Command::Start,
        Some("context") => Command::Context,
        Some("repaste") if rest.is_empty() => Command::Repaste,
        Some("pull") => Command::Pull(rest),
        Some(first) => Command::Target(std::iter::once(first.to_string()).chain(rest).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Command};

    #[test]
    fn parses_repaste_subcommand() {
        assert_eq!(
            parse_args(["repaste".to_string()].into_iter()),
            Command::Repaste
        );
    }

    #[test]
    fn repaste_with_arguments_is_an_ordinary_target() {
        assert_eq!(
            parse_args(["repaste".to_string(), "x".to_string()].into_iter()),
            Command::Target(vec!["repaste".to_string(), "x".to_string()])
        );
    }

    #[test]
    fn parses_context_subcommand() {
        assert_eq!(
            parse_args(["context".to_string()].into_iter()),
            Command::Context
        );
    }
}
