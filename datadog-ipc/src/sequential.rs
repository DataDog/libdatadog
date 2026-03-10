// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::{Future, Stream};
use std::fmt::Debug;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tarpc::server::{Channel, InFlightRequest, Requests, Serve};
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::OwnedPermit;

#[allow(type_alias_bounds)]
type Request<S, C: Channel> = (S, InFlightRequest<C::Req, C::Resp>);

type PendingPermit<S, C> = Pin<
    Box<dyn Future<Output = Result<OwnedPermit<Request<S, C>>, SendError<()>>> + Send + 'static>,
>;

/// Replaces tarpc::server::Channel::execute which spawns one task per message with an executor
/// that spawns a single worker and queues requests for this task.
///
/// If the queue is full, the request is dropped and will be cancelled by tarpc unless
/// `with_backpressure` is configured for that request type.
pub fn execute_sequential<C, S>(
    reqs: Requests<C>,
    serve: S,
    max_requests: usize,
) -> SequentialExecutor<C, S>
where
    C: Channel,
    S: Serve<C::Req, Resp = C::Resp> + Send + 'static,
    C::Req: Send + Debug + 'static,
    C::Resp: Send + 'static,
    S::Fut: Send,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Request<S, C>>(max_requests);

    tokio::spawn(async move {
        loop {
            let (serve, req) = match rx.recv().await {
                None => return,
                Some(s) => s,
            };
            req.execute(serve).await;
        }
    });
    SequentialExecutor {
        inner: reqs,
        serve,
        tx,
        backpressure: |_| false,
        pending: None,
    }
}

#[pin_project::pin_project]
pub struct SequentialExecutor<C, S>
where
    C: Channel + 'static,
{
    #[pin]
    inner: Requests<C>,
    serve: S,
    tx: tokio::sync::mpsc::Sender<Request<S, C>>,
    /// Returns true for requests that must not be dropped when the queue is full.
    /// The executor will pause reading new requests and wait for channel space instead.
    backpressure: fn(&C::Req) -> bool,
    /// Pending channel-space reservation for a backpressure request.
    pending: Option<(PendingPermit<S, C>, Request<S, C>)>,
}

impl<C, S> Future for SequentialExecutor<C, S>
where
    C: Channel + 'static,
    C::Req: Send + Debug + 'static,
    C::Resp: Send + 'static,
    S: Serve<C::Req, Resp = C::Resp> + Send + 'static + Clone,
    S::Fut: Send,
{
    type Output = anyhow::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            // First flush any pending backpressure send before reading new requests.
            {
                let this = self.as_mut().project();
                if let Some((fut, _)) = this.pending.as_mut() {
                    match fut.as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(_)) => return Poll::Ready(Ok(())), // worker dropped
                        Poll::Ready(Ok(permit)) => {
                            #[allow(clippy::unwrap_used)] // we've just checked this
                            let (_, item) = this.pending.take().unwrap();
                            permit.send(item);
                            // fall through to poll next request
                        }
                    }
                }
            }

            // Read the next request off the transport.
            match self.as_mut().project().inner.poll_next(cx) {
                Poll::Ready(Some(Ok(resp))) => {
                    let backpressured = (self.backpressure)(&resp.get().message);
                    match self.as_ref().tx.try_send((self.serve.clone(), resp)) {
                        Ok(()) => {} // loop to pick up the next request
                        Err(err) => {
                            let (_, resp) = err.into_inner();
                            if backpressured {
                                let fut = Box::pin(self.as_ref().tx.clone().reserve_owned());
                                *self.as_mut().project().pending =
                                    Some((fut, (self.serve.clone(), resp)));
                            } else {
                                tracing::warn!(
                                    "Dropping {:?}: sequential executor queue is full",
                                    resp.get().message
                                );
                            }
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e.into())),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<C, S> SequentialExecutor<C, S>
where
    C: Channel + 'static,
{
    pub fn swap_sender(
        &mut self,
        mut sender: tokio::sync::mpsc::Sender<Request<S, C>>,
    ) -> tokio::sync::mpsc::Sender<Request<S, C>> {
        std::mem::swap(&mut self.tx, &mut sender);
        sender
    }

    /// Configures a predicate that identifies requests which must not be dropped when the queue
    /// is full. For those requests the executor will pause reading and wait for channel space.
    pub fn with_backpressure(mut self, backpressure: fn(&C::Req) -> bool) -> Self {
        self.backpressure = backpressure;
        self
    }
}
