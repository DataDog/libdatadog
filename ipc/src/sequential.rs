// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, Future, Stream};
use tarpc::{
    self,
    server::{Channel, InFlightRequest, Requests, Serve},
};

/// Replaces tarpc::server::Channel::execute which spawns one task per message with an executor
/// that spawns a single worker and queues requests for this task.
///
/// If the queue is full, the requests is dropped and will be cancelled by tarpc.
pub fn execute_sequential<C, S>(
    reqs: Requests<C>,
    serve: S,
    max_requests: usize,
) -> SequentialExecutor<C, S>
where
    C: Channel,
    S: Serve<C::Req, Resp = C::Resp> + Send + 'static,
    C::Req: Send + 'static,
    C::Resp: Send + 'static,
    S::Fut: Send,
{
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<(S, InFlightRequest<C::Req, C::Resp>)>(max_requests);

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
    tx: tokio::sync::mpsc::Sender<(S, InFlightRequest<C::Req, C::Resp>)>,
}

impl<C, S> Future for SequentialExecutor<C, S>
where
    C: Channel + 'static,
    C::Req: Send + 'static,
    C::Resp: Send + 'static,
    S: Serve<C::Req, Resp = C::Resp> + Send + 'static + Clone,
    S::Fut: Send,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        while let Some(response_handler) = ready!(self.as_mut().project().inner.poll_next(cx)) {
            match response_handler {
                Ok(resp) => {
                    let server = self.serve.clone();
                    if let Err(err) = self.as_ref().tx.try_send((server, resp)) {
                        // TODO: should we log something in case we drop the request on the floor?
                    }
                }
                Err(e) => {
                    // TODO: should we log something in case we drop the request on the floor?
                    break;
                }
            }
        }
        Poll::Ready(())
    }
}
