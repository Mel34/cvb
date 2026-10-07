// Test helper: prints the path of the Bash rcfile that `cvb on` would generate.
//
// Built by scripts/test-history.sh with plain rustc. It compiles the real
// src/shell.rs, so the tests exercise the exact rcfile (and the embedded
// data/cvb-control.bash) that the shipped binary produces.

#[path = "../src/shell.rs"]
mod shell;

fn main() {
    match shell::create_rcfile() {
        Ok(path) => println!("{}", path.display()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
