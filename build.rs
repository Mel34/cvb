fn main() {
    cc::Build::new()
        .no_default_flags(true)
        .flag("-fno-lto")
        .file("src/libinput_shim.c")
        .compile("cvb-libinput-shim");

    pkg_config::probe_library("libinput").unwrap();
    pkg_config::probe_library("libudev").unwrap();
    pkg_config::probe_library("libcanberra").unwrap();
}
