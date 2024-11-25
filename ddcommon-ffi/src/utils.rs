/// Wraps a C-FFI function in standard form
/// Expects the function to return a result type that implements into
/// and to be decorated with #[named].
#[macro_export]
macro_rules! wrap_with_ffi_result {
    ($body:block) => {
        (|| $body)()
            .context(concat!(function_name!(), " failed"))
            .into()
    };
}
