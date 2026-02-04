use std::env;
use std::path::PathBuf;

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("cargo:warning=non-linux platform is not supported yet");
}

#[cfg(target_os = "linux")]
fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("libunwind_build");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let libunwind_dir = std::path::Path::new(&manifest_dir).join("libunwind");

    std::fs::create_dir_all(&build_dir).unwrap();

    std::process::Command::new("sh")
        .current_dir(&libunwind_dir)
        .args(["-c", "autoreconf -i"])
        .status()
        .expect("Install autotools: apt install autoconf automake libtool");

    std::process::Command::new("sh")
        .current_dir(&build_dir)
        .args(["-c", 
        &format!("{}/configure --disable-shared --enable-static --disable-minidebuginfo --disable-zlibdebuginfo --disable-tests && make -j$(nproc)",
        libunwind_dir.display())])
        .status()
        .expect("Install autotools: apt install autoconf automake libtool");

    let lib_path = build_dir.join("src/.libs");
    println!("cargo:rustc-link-search=native={}", lib_path.display());
    println!("cargo:rustc-link-lib=static=unwind");
    println!("cargo:rerun-if-changed={}", libunwind_dir.display());
}
