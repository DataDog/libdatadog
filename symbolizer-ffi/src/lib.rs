#[cfg(not(target_os = "windows"))]
#[allow(non_camel_case_types)]
// C types follow a different naming convention than Rust types, so we need to
// allow snake_case here. Example: blaze_symbolizer
mod symbolize;

#[cfg(not(target_os = "windows"))]
pub use symbolize::*;
