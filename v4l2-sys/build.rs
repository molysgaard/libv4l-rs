extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-link-lib=v4l2");

    let crate_extra_includes =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("include");
    let bindings = bindgen::Builder::default()
        // Path to third_party/libv4l-rs/v4l2-sys/include/v4l2_nv_extensions.h
        .clang_arg(format!("-I{}", crate_extra_includes.display()))
        .header("wrapper.h")
        .generate()
        .expect("Failed to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("v4l2_bindings.rs"))
        .expect("Failed to write bindings");
}
