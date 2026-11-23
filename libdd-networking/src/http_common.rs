// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::convert::Infallible;
use core::fmt;
use core::time::Duration;

use thiserror::Error;

#[cfg(not(target_arch = "wasm32"))]
pub type HttpClient = GenericHttpClient<crate::connector::Connector>;

// --- Portable types (available on all platforms including wasm) ---

#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    Parse,
    Closed,
    Canceled,
    Incomplete,
    WriteAborted,
    ParseStatus,
    Timeout,
    Other,
}

#[derive(Debug, Error)]
pub struct ClientError {
    source: anyhow::Error,
    kind: ErrorKind,
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.source.fmt(f)
    }
}

impl ClientError {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

#[derive(Debug)]
pub enum Error {
    Client(ClientError),
    Other(anyhow::Error),
    Infallible(Infallible),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Client(e) => write!(f, "client error: {e}"),
            Error::Infallible(e) => match *e {},
            Error::Other(e) => write!(f, "other error: {e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Other(value.into())
    }
}

impl From<http::Error> for Error {
    fn from(value: http::Error) -> Self {
        Self::Other(value.into())
    }
}

impl core::error::Error for Error {}

// --- Native-only code (hyper, Body, client builders, etc.) ---

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use core::error::Error as _;
    use core::task::Poll;

    use crate::connector::Connector;
    use http_body_util::BodyExt;
    use hyper::body::Incoming;
    use hyper_util::rt::TokioTimer;
    use libdd_common::weak_waker::WeakWakerFuture;
    use pin_project::pin_project;

    /// Hyper executor that spawns tasks on the current tokio runtime and gives them a weak waker.
    ///
    /// The client spawns its connection tasks and pool tasks with this executor. Pool state
    /// outlives the runtime, because it lives in the client. When a connection task waits on the
    /// request channel, the channel keeps a clone of the task waker. With a plain tokio waker,
    /// that clone keeps the task cell alive after the runtime drops. The task cell holds the
    /// scheduler handle, which holds the I/O driver and its epoll fd and eventfd.
    ///
    /// Each task is wrapped in a [`WeakWakerFuture`]. Shared state then only sees the weak waker,
    /// and the real waker is released when the runtime drops the task.
    ///
    /// Unlike [`hyper_util::rt::TokioExecutor`], this executor does not panic outside a tokio
    /// runtime. It drops the task instead. Hyper then reports the connection as closed, or
    /// skips pool cleanup for the idle task.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WeakWakerExecutor;

    impl<F> hyper::rt::Executor<F> for WeakWakerExecutor
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // The task is detached, like with `tokio::spawn`.
                drop(handle.spawn(WeakWakerFuture::new(fut)));
            }
        }
    }

    impl From<hyper::Error> for ClientError {
        fn from(source: hyper::Error) -> Self {
            use ErrorKind::*;
            let kind = if source.is_canceled() {
                Canceled
            } else if source.is_parse() {
                Parse
            } else if source.is_parse_status() {
                ParseStatus
            } else if source.is_incomplete_message() {
                Incomplete
            } else if source.is_body_write_aborted() {
                WriteAborted
            } else if source.is_timeout() {
                Timeout
            } else if source.is_closed() {
                Closed
            } else {
                Other
            };
            Self {
                kind,
                source: source.into(),
            }
        }
    }

    pub type HttpResponse = http::Response<Body>;
    pub type HttpRequest = http::Request<Body>;
    pub type HttpRequestError = hyper_util::client::legacy::Error;

    pub type ResponseFuture = hyper_util::client::legacy::ResponseFuture;

    /// Idle timeout for connections kept alive by a client configured for periodic use (see
    /// [`new_client_periodic`]).
    ///
    /// Kept much smaller than typical keep-alive timeouts on the receiving end (e.g. the Datadog
    /// agent), so that an idle pooled connection is dropped by our side before the receiver closes
    /// it.
    pub(crate) const PERIODIC_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
    /// Max number of idle connections in a client's connection pool. This is a safety resource
    /// bound that we don't really expect to hit in practice.
    pub(crate) const POOL_MAX_IDLE: usize = 20;

    /// Create a new default configuration hyper client for fixed interval sending.
    ///
    /// This client pools connections with a small timeout (smaller than any potential keep-alive on
    /// the receiver side), because otherwise we would get a pipe closed every second connection
    /// because of the keep alive in the agent or the backend.
    ///
    /// This is in general not a problem if we use the client once every tens of seconds.
    pub fn new_client_periodic() -> GenericHttpClient<Connector> {
        client_builder()
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(PERIODIC_POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE)
            .build(Connector::default())
    }

    /// Create a new default configuration hyper client.
    ///
    /// It will keep connections open for a longer time and reuse them.
    pub fn new_default_client() -> GenericHttpClient<Connector> {
        client_builder()
            .pool_max_idle_per_host(POOL_MAX_IDLE)
            .build(Connector::default())
    }

    pub fn into_response(response: hyper::Response<Incoming>) -> HttpResponse {
        response.map(Body::Incoming)
    }

    impl From<HttpRequestError> for ClientError {
        fn from(err: HttpRequestError) -> Self {
            let kind = if let Some(source) = err.source().and_then(|s| s.downcast_ref::<Error>()) {
                match source {
                    Error::Client(client_error) => client_error.kind,
                    Error::Other(_) => ErrorKind::Other,
                    Error::Infallible(infallible) => match *infallible {},
                }
            } else if err.is_connect() {
                ErrorKind::Closed
            } else {
                ErrorKind::Other
            };
            Self {
                source: err.into(),
                kind,
            }
        }
    }

    pub async fn collect_response_bytes(response: HttpResponse) -> Result<bytes::Bytes, Error> {
        Ok(response.into_body().collect().await?.to_bytes())
    }

    pub fn mock_response(
        builder: http::response::Builder,
        body: hyper::body::Bytes,
    ) -> anyhow::Result<HttpResponse> {
        Ok(builder.body(Body::from_bytes(body))?)
    }

    pub fn empty_response(builder: http::response::Builder) -> Result<HttpResponse, Error> {
        Ok(builder.body(Body::empty())?)
    }

    #[pin_project(project=BodyProj)]
    #[derive(Debug)]
    pub enum Body {
        Single(#[pin] http_body_util::Full<hyper::body::Bytes>),
        Empty(#[pin] http_body_util::Empty<hyper::body::Bytes>),
        Boxed(#[pin] http_body_util::combinators::BoxBody<hyper::body::Bytes, anyhow::Error>),
        Channel(#[pin] tokio::sync::mpsc::Receiver<hyper::body::Bytes>),
        Incoming(#[pin] hyper::body::Incoming),
    }

    pub struct Sender {
        tx: tokio::sync::mpsc::Sender<hyper::body::Bytes>,
    }

    impl Sender {
        pub async fn send_data(&self, data: hyper::body::Bytes) -> anyhow::Result<()> {
            self.tx.send(data).await?;
            Ok(())
        }
    }

    impl Body {
        pub fn empty() -> Self {
            Body::Empty(http_body_util::Empty::new())
        }

        pub fn from_bytes(bytes: hyper::body::Bytes) -> Self {
            Body::Single(http_body_util::Full::new(bytes))
        }

        pub fn boxed<
            E: core::error::Error + Sync + Send + 'static,
            T: hyper::body::Body<Data = hyper::body::Bytes, Error = E> + Sync + Send + 'static,
        >(
            body: T,
        ) -> Self {
            Body::Boxed(body.map_err(anyhow::Error::from).boxed())
        }

        pub fn channel() -> (Sender, Self) {
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            (Sender { tx }, Body::Channel(rx))
        }

        pub fn incoming(incoming: Incoming) -> Self {
            Body::Incoming(incoming)
        }
    }

    impl Default for Body {
        fn default() -> Self {
            Body::empty()
        }
    }

    impl From<&'static str> for Body {
        fn from(s: &'static str) -> Self {
            Body::from_bytes(hyper::body::Bytes::from_static(s.as_bytes()))
        }
    }

    impl From<Vec<u8>> for Body {
        fn from(s: Vec<u8>) -> Self {
            Body::from_bytes(hyper::body::Bytes::from(s))
        }
    }

    impl From<String> for Body {
        fn from(s: String) -> Self {
            Body::from_bytes(hyper::body::Bytes::from(s))
        }
    }

    impl hyper::body::Body for Body {
        type Data = hyper::body::Bytes;

        type Error = Error;

        fn poll_frame(
            self: core::pin::Pin<&mut Self>,
            cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            match self.project() {
                BodyProj::Single(pin) => pin.poll_frame(cx).map_err(Error::Infallible),
                BodyProj::Empty(pin) => pin.poll_frame(cx).map_err(Error::Infallible),
                BodyProj::Boxed(pin) => pin.poll_frame(cx).map_err(Error::Other),
                BodyProj::Channel(pin) => {
                    let data = match pin.get_mut().poll_recv(cx) {
                        Poll::Ready(Some(data)) => data,
                        Poll::Ready(None) => return Poll::Ready(None),
                        Poll::Pending => return Poll::Pending,
                    };
                    Poll::Ready(Some(Ok(hyper::body::Frame::data(data))))
                }
                BodyProj::Incoming(pin) => pin
                    .poll_frame(cx)
                    .map_err(|e| Error::Client(ClientError::from(e))),
            }
        }

        fn is_end_stream(&self) -> bool {
            match self {
                Body::Single(body) => body.is_end_stream(),
                Body::Empty(body) => body.is_end_stream(),
                Body::Boxed(body) => body.is_end_stream(),
                Body::Channel(body) => body.is_closed() && body.is_empty(),
                Body::Incoming(body) => body.is_end_stream(),
            }
        }

        fn size_hint(&self) -> http_body::SizeHint {
            match self {
                Body::Single(body) => body.size_hint(),
                Body::Empty(body) => body.size_hint(),
                Body::Boxed(body) => body.size_hint(),
                Body::Channel(_) => http_body::SizeHint::default(),
                Body::Incoming(body) => body.size_hint(),
            }
        }
    }

    pub type GenericHttpClient<C> = hyper_util::client::legacy::Client<C, Body>;

    /// Create a hyper client builder that spawns its tasks with [`WeakWakerExecutor`].
    pub fn client_builder() -> hyper_util::client::legacy::Builder {
        hyper_util::client::legacy::Client::builder(WeakWakerExecutor)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod weak_waker_executor_tests {
    use super::WeakWakerExecutor;
    use alloc::sync::Arc;
    use core::time::Duration;
    use hyper::rt::Executor;

    /// Build a runtime whose handle owns `sentinel`, so the sentinel count shows whether
    /// anything still holds the runtime handle.
    fn runtime_owning(sentinel: &Arc<()>) -> tokio::runtime::Runtime {
        let sentinel = sentinel.clone();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .on_thread_start(move || {
                let _ = &sentinel;
            })
            .build()
            .unwrap()
    }

    /// Spawn a task that waits on a channel whose sender stays outside the runtime, drop the
    /// runtime, then return the sentinel count seen while the sender still lives.
    fn count_after_runtime_drop<E: Executor<futures::future::BoxFuture<'static, ()>>>(
        executor: E,
    ) -> usize {
        let sentinel = Arc::new(());
        let rt = runtime_owning(&sentinel);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let _guard = rt.enter();
            executor.execute(Box::pin(async move {
                let _ = rx.recv().await;
            }));
        }
        // Let the task run until it waits on the channel.
        rt.block_on(async {
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        });
        drop(rt);
        let count = Arc::strong_count(&sentinel);
        drop(tx);
        assert_eq!(Arc::strong_count(&sentinel), 1);
        count
    }

    #[test]
    fn runtime_drop_releases_handle() {
        assert_eq!(count_after_runtime_drop(WeakWakerExecutor), 1);
    }

    #[test]
    fn tokio_executor_keeps_handle_after_runtime_drop() {
        // Control case: with a plain tokio waker, the channel keeps the runtime handle alive.
        assert!(count_after_runtime_drop(hyper_util::rt::TokioExecutor::new()) > 1);
    }

    #[test]
    fn spawned_task_runs_to_completion() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let _guard = rt.enter();
            WeakWakerExecutor.execute(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = tx.send(42);
            });
        }
        assert_eq!(rt.block_on(rx).unwrap(), 42);
    }

    #[test]
    fn execute_outside_runtime_drops_the_task() {
        let sentinel = Arc::new(());
        let held = sentinel.clone();
        WeakWakerExecutor.execute(async move {
            let _ = &held;
        });
        assert_eq!(Arc::strong_count(&sentinel), 1);
    }
}
