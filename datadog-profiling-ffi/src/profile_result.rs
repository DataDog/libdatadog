// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileStatus2;

#[repr(C)]
pub struct ProfileResult<T> {
    status: ProfileStatus2,
    ok: T,
}

impl<T: Default, E: core::error::Error> From<Result<T, E>> for ProfileResult<T> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(ok) => ProfileResult {
                status: ProfileStatus2::OK,
                ok,
            },
            Err(err) => ProfileResult {
                status: ProfileStatus2::from_error(err),
                ok: Default::default(),
            },
        }
    }
}
