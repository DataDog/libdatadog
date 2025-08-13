// SPDX-License-Identifier: Apache-2.0
// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/

use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt},
};

pub async fn receiver_entry_point(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
    crate::receiver::entry_points::receiver_entry_point(timeout, stream).await
}
