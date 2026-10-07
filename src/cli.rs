use std::env;

#[derive(Debug)]
pub enum Command {
    Start,
    Target(Vec<String>),
    Help,
    Version,
}

pub fn parse() -> Command {
    let mut args = env::args().skip(1);

    match args.next().as_deref() {
        Some("--help") | Some("-h") => Command::Help,
        Some("--version") | Some("-V") => Command::Version,
        None => Command::Start,
        Some(first) => Command::Target(std::iter::once(first.to_string()).chain(args).collect()),
    }
}
