// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus;

#[repr(C)]
pub struct Result<T> {
    status: ProfileStatus,
    ok: T,
}

impl<T: Default, E: core::error::Error> From<std::result::Result<T, E>>
    for Result<T>
{
    fn from(result: std::result::Result<T, E>) -> Self {
        match result {
            Ok(ok) => Result { status: ProfileStatus::OK, ok },
            Err(err) => Result {
                status: ProfileStatus::from_error(err),
                ok: Default::default(),
            },
        }
    }
}
