use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::PathBuf;

fn sha256(path: &str) -> String {
    let contents = fs::read(path)
        .unwrap_or_else(|error| panic!("unable to read {path}: {error}"));

    Sha256::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=data/cvb.bash");
    println!("cargo:rerun-if-changed=data/cvb-control.bash");

    cc::Build::new()
        .no_default_flags(true)
        .flag("-fno-lto")
        .file("src/libinput_shim.c")
        .compile("cvb-libinput-shim");

    pkg_config::probe_library("libinput").unwrap();
    pkg_config::probe_library("libudev").unwrap();
    pkg_config::probe_library("libcanberra").unwrap();

    let cvb_bash_sha256 = sha256("data/cvb.bash");
    let cvb_control_bash_sha256 = sha256("data/cvb-control.bash");

    let generated = format!(
        "pub const CVB_BASH_SHA256: &str = \"{cvb_bash_sha256}\";\n\
         pub const CVB_CONTROL_BASH_SHA256: &str = \"{cvb_control_bash_sha256}\";\n"
    );

    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"),
    );

    fs::write(out_dir.join("cvb_script_hashes.rs"), generated)
        .expect("unable to write generated CVB script fingerprints");
}
