// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::trace_utils::SendData;
use manual_future::ManualFutureCompleter;
use tracing::debug;

#[derive(Default)]
pub(crate) struct TraceSendData {
    pub send_data: Vec<SendData>,
    pub send_data_size: usize,
    pub force_flush: Option<ManualFutureCompleter<()>>,
}

impl TraceSendData {
    /// Flush the traces. This method does not return any value. It triggers a flush of the traces
    /// and completes the future. If there is no future to complete, it does nothing.
    pub(crate) fn flush(&mut self) {
        if let Some(force_flush) = self.force_flush.take() {
            debug!(
                "Emitted flush for traces with {} bytes in send_data buffer",
                self.send_data_size
            );
            tokio::spawn(async move {
                force_flush.complete(()).await;
            });
        }
    }
}
