// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReapError {
    #[error("Timeout waiting for child process to exit")]
    Timeout,
    #[error("Error waiting for child process to exit: {0}")]
    WaitError(#[from] nix::Error),
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum PollError {
    #[error("Poll failed with errno: {0}")]
    PollError(i32),
    #[error("Poll returned unexpected result: revents = {0}")]
    UnexpectedResult(i16),
}
