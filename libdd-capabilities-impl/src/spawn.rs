// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native spawn implementation backed by `tokio::runtime::Handle::spawn`.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use libdd_capabilities::maybe_send::MaybeSend;
use libdd_capabilities::spawn::SpawnCapability;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct NativeSpawnCapability {
    handle: tokio::runtime::Handle,
}

impl NativeSpawnCapability {
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    pub fn from_current() -> Self {
        Self {
            handle: tokio::runtime::Handle::current(),
        }
    }
}

impl SpawnCapability for NativeSpawnCapability {
    type JoinHandle<T: MaybeSend + 'static> = NativeJoinHandle<T>;

    fn spawn<F, T>(&self, future: F) -> NativeJoinHandle<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static,
    {
        NativeJoinHandle(self.handle.spawn(future))
    }
}

/// Newtype wrapping `tokio::task::JoinHandle<T>` that normalises the output to
/// `T` instead of `Result<T, JoinError>`.
///
/// A `JoinError` means the spawned task panicked or was aborted. Workers use
/// `CancellationToken` for graceful shutdown, so `JoinError` indicates a bug.
pub struct NativeJoinHandle<T>(JoinHandle<T>);

impl<T> Future for NativeJoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // JoinHandle<T>: Unpin, so Pin::new is safe.
        match Pin::new(&mut self.get_mut().0).poll(cx) {
            Poll::Ready(Ok(val)) => Poll::Ready(val),
            Poll::Ready(Err(e)) => panic!("spawned task failed: {e}"),
            Poll::Pending => Poll::Pending,
        }
    }
}
