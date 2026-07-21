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
/// `ProfileStatus: From<E>`. On caught panic the panic payload is routed
/// through the globally registered handler (see `ddog_prof_set_panic_handler`)
/// and the returned status carries a static `c"libdatadog panicked"` sentinel.
///
/// The enclosing function must carry `#[function_name::named]` so the panic
/// callback receives the function name.
#[macro_export]
macro_rules! wrap_with_profile_status {
    ($body:block) => {{
        use std::panic::{catch_unwind, AssertUnwindSafe};
        match catch_unwind(AssertUnwindSafe(|| $body)) {
            Ok(result) => $crate::ProfileStatus::from(result),
            Err(payload) => {
                $crate::panic_handler::fire_panic_handler(function_name!(), &*payload);
                $crate::ProfileStatus::from(c"libdatadog panicked")
            }
        }
    }};
}

pub(crate) use {ensure_non_null_insert, ensure_non_null_out_parameter, wrap_with_profile_status};
