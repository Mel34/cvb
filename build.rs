fn main() {
    println!("cargo:rustc-link-lib=input");
    println!("cargo:rustc-link-lib=udev");

    pkg_config::probe_library("libcanberra").expect("libcanberra is required to build cvb");
}
