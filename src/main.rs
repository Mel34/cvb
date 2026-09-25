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
            println!("CVB target: {}", args.join(" "));
        }
    }
}
