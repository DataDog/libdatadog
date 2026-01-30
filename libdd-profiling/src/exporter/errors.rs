// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error;
use std::fmt;
use thiserror::Error;

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

#[derive(Debug, Error)]
pub enum SendError {
    #[error("Failed to build request")]
    BuildFailed(#[from] anyhow::Error),

    #[error("Operation cancelled by user")]
    Cancelled,

    #[error("Failed to send HTTP request")]
    RequestFailed(#[from] reqwest::Error),
}
