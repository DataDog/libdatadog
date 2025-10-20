// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus;

#[repr(C)]
pub struct ProfileResult<T> {
    status: ProfileStatus,
    ok: T,
}

impl<T: Default, E: core::error::Error> From<Result<T, E>>
    for ProfileResult<T>
{
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(ok) => ProfileResult { status: ProfileStatus::OK, ok },
            Err(err) => ProfileResult {
                status: ProfileStatus::from_error(err),
                ok: Default::default(),
            },
        }
    }
}
