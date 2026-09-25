use std::env;

#[derive(Debug)]
pub enum Command {
    On,
    Off,
    Target(Vec<String>),
    Help,
    Version,
}

pub fn parse() -> Command {
    let mut args = env::args().skip(1);

    match args.next().as_deref() {
        Some("on") => Command::On,
        Some("off") => Command::Off,
        Some("--help") | Some("-h") | None => Command::Help,
        Some("--version") | Some("-V") => Command::Version,
        Some(first) => Command::Target(std::iter::once(first.to_string()).chain(args).collect()),
    }
}
