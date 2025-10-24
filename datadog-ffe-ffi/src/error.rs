// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::Error;

const NULL_POINTER_ERROR: &str = "null pointer provided";

pub fn ffe_error(msg: &str) -> Error {
    Error::from(msg)
}
