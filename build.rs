fn main() {
    pkg_config::probe_library("libinput").unwrap();
    pkg_config::probe_library("libudev").unwrap();
}
