fn main() {
    // ensure symbols are properly exported for dlsym to be able to look them up
    println!("cargo:rustc-link-arg-tests=-rdynamic")
}
