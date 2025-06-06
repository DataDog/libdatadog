// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Wraps a C-FFI function in standard form
/// Expects the function to return a result type that implements into and to be decorated with
/// #[named].
#[macro_export]
macro_rules! wrap_with_ffi_result {
    ($body:block) => {{
        use anyhow::Context;
        (|| $body)()
            .context(concat!(function_name!(), " failed"))
            .into()
    }};
}

/// Wraps a C-FFI function in standard form.
/// Expects the function to return a VoidResult and to be decorated with #[named].
#[macro_export]
macro_rules! wrap_with_void_ffi_result {
    ($body:block) => {{
        use anyhow::Context;
        (|| {
            $body;
            anyhow::Ok(())
        })()
        .context(concat!(function_name!(), " failed"))
        .into()
    }};
}

pub trait ToHexStr {
    fn to_hex_str(&self) -> String;
}

impl ToHexStr for usize {
    fn to_hex_str(&self) -> String {
        format!("0x{self:X}")
    }
}
