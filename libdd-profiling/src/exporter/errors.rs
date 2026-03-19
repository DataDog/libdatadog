// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SendError {
    #[error("Failed to build request")]
    BuildFailed(#[from] anyhow::Error),

    #[error("Failed to send HTTP request")]
    RequestFailed(#[source] anyhow::Error),
}
