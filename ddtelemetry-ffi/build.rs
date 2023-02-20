fn main() {
    // ensure symbols are properly exported for dlsym to be able to look them up
    // https://github.com/rust-lang/cargo/issues/10937
    // TODO: only apply this setting in tests
    println!("cargo:rustc-link-arg=-rdynamic")
}
