fn main() {
    pkg_config::probe_library("libinput").unwrap();
    pkg_config::probe_library("libudev").unwrap();

    cc::Build::new()
        .file("src/libinput_shim.c")
        .compile("cvb-libinput-shim");
}
