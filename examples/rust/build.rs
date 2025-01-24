fn main() {
    // Tell Cargo to link with libunwind
    // todo: static is better, though not for tests
    // println!("cargo:rustc-link-lib=static=unwind");
    // dynamic is fine for tests
    println!("cargo:rustc-link-lib=unwind");
    println!("cargo:rustc-link-lib=dylib=gcc_s"); // Link libgcc_s for _Unwind symbols
    // println!("cargo:rustc-link-lib=libgcc_s.so.1"); // Link libgcc_s for _Unwind symbols

    println!("cargo:rustc-link-search=native=/usr/lib/local;/usr/lib");
    // /usr/lib/libgcc_s.so.1
    // println!("cargo:rustc-link-search=native=/usr/lib");
    // with rust bindings there is a .a, though for now I'm not sure what to use
}
