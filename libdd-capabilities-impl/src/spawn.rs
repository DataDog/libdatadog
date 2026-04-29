// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native spawn implementation backed by `tokio::runtime::Handle::spawn`.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use libdd_capabilities::maybe_send::MaybeSend;
use libdd_capabilities::spawn::{SpawnCapability, SpawnError};
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct NativeSpawnCapability;

impl SpawnCapability for NativeSpawnCapability {
    type RuntimeContext = tokio::runtime::Handle;
    type JoinHandle<T: MaybeSend + 'static> = NativeJoinHandle<T>;

    fn spawn<F, T>(&self, future: F, ctx: &tokio::runtime::Handle) -> NativeJoinHandle<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static,
    {
        NativeJoinHandle(ctx.spawn(future))
    }
}

/// Newtype wrapping `tokio::task::JoinHandle<T>` that surfaces
/// `Result<T, SpawnError>`, mapping tokio's `JoinError` (panic / abort)
/// into the executor-agnostic [`SpawnError`] from `libdd-capabilities`.
pub struct NativeJoinHandle<T>(JoinHandle<T>);

impl<T> Future for NativeJoinHandle<T> {
    type Output = Result<T, SpawnError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<T, SpawnError>> {
        match Pin::new(&mut self.get_mut().0).poll(cx) {
            Poll::Ready(Ok(val)) => Poll::Ready(Ok(val)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(SpawnError::new(e.to_string()))),
            Poll::Pending => Poll::Pending,
        }
    }
}
