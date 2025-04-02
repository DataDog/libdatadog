// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Error {
    InvalidUrl,
    OperationTimedOut,
    UserRequestedCancellation,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "invalid url",
            Self::OperationTimedOut => "operation timed out",
            Self::UserRequestedCancellation => "operation cancelled by user",
        })
    }
}

impl error::Error for Error {}
