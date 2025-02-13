use std::env;

// inspired from libunwind-sys

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Choose build.
    let target = env::var("TARGET").unwrap();
    let link_lib_arch = match target.as_str() {
        "x86_64-unknown-linux-gnu"| "x86_64-unknown-linux-musl" => "x86_64",
        "i686-unknown-linux-gnu"|"i586-unknown-linux-gnu"  => "x86",
        "arm-unknown-linux-gnueabihf" => "arm",
        _ => ""
    };
    if link_lib_arch.is_empty() {
        println!("cargo:warning=target {} is unsupported",target);
        return;
    }
    
    println!("cargo:rustc-link-search=native=/usr/local/lib");
    println!("cargo:rustc-link-search=native=/usr/lib");
    #[cfg(target_arch = "x86_64")]
    println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu");
    if target.contains("musl") {
        // possibly lzma compressed symbol tables. Do we really need it ?
        println!("cargo:rustc-link-lib=static=lzma");
        println!("cargo:rustc-link-lib=static=unwind");
    }
    else {
        println!("cargo:rustc-link-lib=static=unwind-{}", link_lib_arch);
        println!("cargo:rustc-link-lib=static=unwind");
    }
}
