// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::future::Map;
use futures::FutureExt;
use libdd_trace_utils::trace_utils::SendData;
use manual_future::ManualFutureCompleter;
use tokio::sync::mpsc::Sender;
use tokio::task::{JoinError, JoinHandle};
use tracing::debug;

#[derive(Default)]
pub(crate) struct TraceSendData {
    pub send_data: Vec<SendData>,
    pub send_data_size: usize,
    pub force_flush: Option<ManualFutureCompleter<Option<Sender<()>>>>,
}

impl TraceSendData {
    /// Flush the traces. This method does not return any value. It triggers a flush of the traces
    /// and completes the future. If there is no future to complete, it does nothing.
    pub(crate) fn flush(&mut self) {
        self.do_flush(None);
    }

    /// Flush the traces. It returns a future which can be awaited to determine when data has
    /// actually been sent.
    #[allow(clippy::type_complexity)]
    pub(crate) fn await_flush(&mut self) -> Map<JoinHandle<()>, fn(Result<(), JoinError>)> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        self.do_flush(Some(sender));
        tokio::spawn(async move {
            receiver.recv().await;
        })
        .map(|_| ())
    }

    fn do_flush(&mut self, sender: Option<Sender<()>>) {
        if let Some(force_flush) = self.force_flush.take() {
            debug!(
                "Emitted flush for traces with {} bytes in send_data buffer",
                self.send_data_size
            );
            tokio::spawn(force_flush.complete(sender));
        }
    }
}
