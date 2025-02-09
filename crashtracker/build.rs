fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Link the system `libunwind`
    println!("cargo:rustc-link-lib=unwind");

    // Link `libgcc_s` (needed for `_Unwind` symbols)
    println!("cargo:rustc-link-lib=dylib=gcc_s");
    // todo: avoid hard coding these paths
    // Specify library search paths
    println!("cargo:rustc-link-search=native=/usr/local/lib");
    println!("cargo:rustc-link-search=native=/usr/lib");
}
