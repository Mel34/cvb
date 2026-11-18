use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start,
    Target(Vec<String>),
    Pull(Vec<String>),
    Context,
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
    match args.next().as_deref() {
        Some("--help") | Some("-h") => Command::Help,
        Some("--version") | Some("-V") => Command::Version,
        None => Command::Start,
        Some("context") => Command::Context,
        Some("pull") => Command::Pull(args.collect()),
        Some(first) => Command::Target(std::iter::once(first.to_string()).chain(args).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Command};

    #[test]
    fn parses_context_subcommand() {
        assert_eq!(
            parse_args(["context".to_string()].into_iter()),
            Command::Context
        );
    }
}
