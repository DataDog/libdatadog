// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod datatypes;
mod interning_api;
mod profiles_dictionary;
mod utf8;

#[macro_export]
macro_rules! ensure_non_null_out_parameter {
    ($expr:expr) => {
        if $expr.is_null() {
            return $crate::ProfileStatus::from(c"null pointer used as out parameter");
        }
    };
}

#[macro_export]
macro_rules! ensure_non_null_insert {
    ($expr:expr) => {
        if $expr.is_null() {
            return $crate::ProfileStatus::from(c"tried to insert a null pointer");
        }
    };
}

/// Wraps the body of a `ProfileStatus`-returning FFI function in
/// `catch_unwind`. The body must evaluate to a `Result<(), E>` where
/// `ProfileStatus: From<E>`. On caught panic the returned status has its
/// `IS_PANIC_MASK` bit set; callers can check via `ddog_prof_Status_is_panic`.
///
/// The enclosing function must carry `#[function_name::named]` so that the
/// caught-panic message is automatically prefixed with the function name.
#[macro_export]
macro_rules! wrap_with_profile_status {
    ($body:block) => {{
        use std::panic::{catch_unwind, AssertUnwindSafe};
        match catch_unwind(AssertUnwindSafe(|| $body)) {
            Ok(result) => $crate::ProfileStatus::from(result),
            Err(payload) => $crate::ProfileStatus::from_panic(payload, function_name!()),
        }
    }};
}

pub(crate) use {ensure_non_null_insert, ensure_non_null_out_parameter, wrap_with_profile_status};
