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

pub(crate) use {ensure_non_null_insert, ensure_non_null_out_parameter};
