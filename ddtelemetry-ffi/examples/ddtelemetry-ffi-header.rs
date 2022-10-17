fn main() {
    println!(
        "{}",
        include_str!(concat!(env!("OUT_DIR"), "/ddtelemetry.h"))
    );
}
