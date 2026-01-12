use std::{fmt::Debug, future::Future, io, time::Duration};

use hyper_util::client::legacy::connect::Connect;

pub trait Runtime: Debug + Send + Sync + 'static {
    type JoinError;
    type JoinHandle<R: Send + 'static>: FutureHandle<R, Self::JoinError> + Unpin;

    fn new() -> io::Result<Self>
    where
        Self: Sized;

    fn spawn_ref<Fut: Future<Output = R> + Send + 'static, R: Send + 'static>(
        &self,
        f: Fut,
    ) -> Self::JoinHandle<R>;

    fn sleep(time: Duration) -> impl Future<Output = ()> + Send;

    type HttpClient: HttpClient;

    fn http_client() -> Self::HttpClient;
}

pub trait HttpClient: Send + Sync + Clone + Debug {
    fn request(
        &self,
        req: http::Request<crate::hyper_migration::Body>,
    ) -> impl Future<Output = io::Result<http::Response<crate::hyper_migration::Body>>> + Send + 'static;
}

impl<C: Connect + Send + Sync + Clone + 'static> HttpClient for crate::GenericHttpClient<C> {
    fn request(
        &self,
        req: http::Request<crate::hyper_migration::Body>,
    ) -> impl Future<Output = io::Result<http::Response<crate::hyper_migration::Body>>> + Send + 'static
    {
        let res = self.request(req);
        async {
            res.await
                .map_err(io::Error::other)
                .map(|b| b.map(crate::hyper_migration::Body::incoming))
        }
    }
}

pub trait FutureHandle<Ok, Err>: Future<Output = Result<Ok, Err>> {}

impl<Ok> FutureHandle<Ok, tokio::task::JoinError> for tokio::task::JoinHandle<Ok> {}

impl Runtime for tokio::runtime::Runtime {
    type JoinError = tokio::task::JoinError;
    type JoinHandle<R: Send + 'static> = tokio::task::JoinHandle<R>;

    fn new() -> io::Result<Self> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }

    fn spawn_ref<Fut: Future<Output = R> + Send + 'static, R: Send + 'static>(
        &self,
        f: Fut,
    ) -> Self::JoinHandle<R> {
        self.spawn(f)
    }

    fn sleep(time: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(time)
    }

    type HttpClient = crate::HttpClient;

    fn http_client() -> Self::HttpClient {
        crate::hyper_migration::new_default_client()
    }
}
