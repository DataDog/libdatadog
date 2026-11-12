// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::HttpCancellationToken;
use crate::catch_panic;
use crate::error::{HttpClientError, HttpClientErrorCode};
use crate::gen_error;
use crate::request::{
    HttpClientConfig, HttpMethod, HttpRequest, bytes_arg, headers_arg, merge_headers, parts_arg,
    str_arg,
};
use crate::response::HttpResponse;
use libdd_http_client::{
    HttpClient as InnerClient, HttpClientError as UpstreamError, HttpMethod as InnerMethod,
    HttpRequest as InnerRequest, RetryConfig,
};
use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime};
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Can be used from several threads at once.
#[derive(Debug)]
pub struct HttpClient {
    // Declared first so it is dropped before the runtime it runs on.
    inner: InnerClient,
    runtime: Arc<ForkSafeRuntime>,
    base: String,
    default_headers: Vec<(String, String)>,
}

impl HttpClient {
    /// # Safety
    ///
    /// `runtime` must be `NULL` or a live `Arc::into_raw` pointer.
    unsafe fn from_config(
        config: &HttpClientConfig<'_>,
        runtime: *const ForkSafeRuntime,
    ) -> Result<Self, Option<Box<HttpClientError>>> {
        let base_url = str_arg(config.base_url)?;
        if base_url.is_empty() || config.timeout_ms == 0 {
            return Err(gen_error!(HttpClientErrorCode::InvalidConfig));
        }
        let base = normalize_base_url(base_url)?;
        let default_headers = headers_arg(config.default_headers)?;
        let unix_socket_path = str_arg(config.unix_socket_path)?;

        // The inner client requires a base URL but routes on the request URL.
        let mut builder = InnerClient::builder()
            .base_url(base.clone())
            .timeout(Duration::from_millis(config.timeout_ms))
            // HTTP errors are returned as responses.
            .treat_http_errors_as_errors(false)
            .periodic(config.periodic);
        if config.max_retries > 0 {
            let mut retry = RetryConfig::new().max_retries(config.max_retries);
            if config.retry_initial_delay_ms > 0 {
                retry = retry.initial_delay(Duration::from_millis(config.retry_initial_delay_ms));
            }
            builder = builder.retry(retry);
        }
        if !unix_socket_path.is_empty() {
            #[cfg(unix)]
            {
                builder = builder.unix_socket(std::path::PathBuf::from(unix_socket_path));
            }
            #[cfg(not(unix))]
            {
                return Err(gen_error!(HttpClientErrorCode::InvalidConfig));
            }
        }
        let inner = builder
            .build()
            .map_err(|e| Some(Box::new(HttpClientError::from(e))))?;

        let runtime = if runtime.is_null() {
            Arc::new(ForkSafeRuntime::with_worker_threads(1).map_err(|e| {
                Some(Box::new(HttpClientError::new(
                    HttpClientErrorCode::Runtime,
                    &e.to_string(),
                )))
            })?)
        } else {
            // SAFETY: takes its own strong reference.
            unsafe {
                Arc::increment_strong_count(runtime);
                Arc::from_raw(runtime)
            }
        };

        Ok(Self {
            inner,
            runtime,
            base,
            default_headers,
        })
    }

    fn full_url(&self, url: &str) -> Result<String, Option<Box<HttpClientError>>> {
        if has_prefix_ignore_case(url, "http://") || has_prefix_ignore_case(url, "https://") {
            Ok(url.to_owned())
        } else if url.starts_with('/') && !url.starts_with("//") {
            Ok(format!("{}{url}", self.base))
        } else {
            Err(gen_error!(HttpClientErrorCode::InvalidArgument))
        }
    }

    fn build_request(
        &self,
        request: &HttpRequest<'_>,
    ) -> Result<InnerRequest, Option<Box<HttpClientError>>> {
        let url = self.full_url(str_arg(request.url)?)?;
        let headers = merge_headers(&self.default_headers, headers_arg(request.headers)?);
        let body = bytes_arg(request.body)?;
        let parts = parts_arg(request.multipart)?;
        if !body.is_empty() && !parts.is_empty() {
            return Err(gen_error!(HttpClientErrorCode::InvalidArgument));
        }

        let mut inner = InnerRequest::new(inner_method(request.method), url).with_headers(headers);
        if !body.is_empty() {
            inner = inner.with_body(body.to_vec());
        }
        for part in parts {
            inner = inner.with_multipart_part(part);
        }
        if request.timeout_ms > 0 {
            inner = inner.with_timeout(Duration::from_millis(request.timeout_ms));
        }
        Ok(inner)
    }

    fn send(
        &self,
        request: &HttpRequest<'_>,
        cancel: Option<&HttpCancellationToken>,
    ) -> Result<HttpResponse, Option<Box<HttpClientError>>> {
        let cancel = cancel.map(HttpCancellationToken::cancellation_token);
        let inner_request = self.build_request(request)?;
        let outcome = self
            .runtime
            .block_on(async {
                match cancel {
                    Some(token) => until_cancelled(token, self.inner.send(inner_request)).await,
                    None => Some(self.inner.send(inner_request).await),
                }
            })
            .map_err(|e| {
                Some(Box::new(HttpClientError::new(
                    HttpClientErrorCode::Runtime,
                    &e.to_string(),
                )))
            })?;

        match outcome {
            None => Err(gen_error!(HttpClientErrorCode::Cancelled)),
            // When sending, an invalid configuration can only come from the request.
            Some(Err(UpstreamError::InvalidConfig(message))) => Err(Some(Box::new(
                HttpClientError::new(HttpClientErrorCode::InvalidArgument, &message),
            ))),
            Some(Err(error)) => Err(Some(Box::new(HttpClientError::from(error)))),
            Some(Ok(response)) => Ok(HttpResponse::new(response)),
        }
    }
}

fn inner_method(method: HttpMethod) -> InnerMethod {
    match method {
        HttpMethod::Get => InnerMethod::Get,
        HttpMethod::Post => InnerMethod::Post,
        HttpMethod::Put => InnerMethod::Put,
        HttpMethod::Delete => InnerMethod::Delete,
        HttpMethod::Head => InnerMethod::Head,
        HttpMethod::Patch => InnerMethod::Patch,
        HttpMethod::Options => InnerMethod::Options,
    }
}

fn has_prefix_ignore_case(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// Reduces `base_url` to `scheme://authority[/path]`.
fn normalize_base_url(base_url: &str) -> Result<String, Option<Box<HttpClientError>>> {
    let parsed =
        Url::parse(base_url).map_err(|_| gen_error!(HttpClientErrorCode::InvalidConfig))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(gen_error!(HttpClientErrorCode::InvalidConfig));
    }
    Ok(format!(
        "{}://{}{}",
        parsed.scheme(),
        parsed.authority(),
        parsed.path().trim_end_matches('/')
    ))
}

/// Runs `future` unless `token` is cancelled first. The token is checked before polling, so a
/// cancelled token starts nothing.
async fn until_cancelled<F: Future>(token: &CancellationToken, future: F) -> Option<F::Output> {
    let mut cancelled = std::pin::pin!(token.cancelled());
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(move |cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        future.as_mut().poll(cx).map(Some)
    })
    .await
}

/// Creates a client and writes it to `*out`. Returns `NULL` on success, and the caller then owns
/// `*out` and releases it with [`ddog_http_client_drop`]. On failure `*out` is untouched.
///
/// No connection is opened until the first request.
///
/// `runtime` is `NULL` (the client owns a private runtime) or a `ForkSafeRuntime` pointer from
/// `ddog_shared_runtime_new`. The client takes its own reference, so the caller may free its own
/// right away.
///
/// A client must not be used in a forked child unless its runtime follows the fork protocol of
/// `ForkSafeRuntime`.
///
/// # Safety
///
/// * `config` must be `NULL` (rejected with `InvalidArgument`) or point to a valid
///   [`HttpClientConfig`] whose slices are valid for reads.
/// * `runtime` must be `NULL` or a pointer obtained from `ddog_shared_runtime_new`.
/// * `out` must be `NULL` (rejected with `InvalidArgument`) or valid for writes of one pointer.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_http_client_new(
    config: *const HttpClientConfig<'_>,
    runtime: *const ForkSafeRuntime,
    out: *mut *mut HttpClient,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        match unsafe { config.as_ref() } {
            Some(config) if !out.is_null() => {
                match unsafe { HttpClient::from_config(config, runtime) } {
                    Ok(client) => {
                        unsafe { out.write(Box::into_raw(Box::new(client))) };
                        None
                    }
                    Err(error) => error,
                }
            }
            _ => gen_error!(HttpClientErrorCode::InvalidArgument),
        },
        gen_error!(HttpClientErrorCode::Panic)
    )
}

/// Releases a client. Passing `NULL` is a no-op. Its responses remain valid.
///
/// # Safety
///
/// `client` must be `NULL` or a pointer returned by [`ddog_http_client_new`] that has not been
/// dropped, and no send may be in flight on it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_drop(client: Option<Box<HttpClient>>) {
    catch_panic!(drop(client), ())
}

/// Sends `request`, blocking the calling thread until it completes. Returns `NULL` on success and
/// writes the response to `*out`, which the caller releases with
/// [`crate::ddog_http_response_drop`]. On failure `*out` is untouched.
///
/// Any HTTP status, including 4xx and 5xx, is a success. Redirects are not followed.
///
/// If `cancel` is not `NULL`, cancelling it from another thread makes this return `Cancelled`, and
/// an already cancelled token makes it return without sending anything.
///
/// # Safety
///
/// * `client` must be `NULL` (rejected with `InvalidArgument`) or a live pointer returned by
///   [`ddog_http_client_new`].
/// * `request` must be `NULL` (rejected with `InvalidArgument`) or point to a valid [`HttpRequest`]
///   whose slices are valid for reads.
/// * `cancel` must be `NULL` or a live token pointer that stays valid until this call returns.
/// * `out` must be `NULL` (rejected with `InvalidArgument`) or valid for writes of one pointer.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_http_client_send(
    client: *const HttpClient,
    request: *const HttpRequest<'_>,
    cancel: *const HttpCancellationToken,
    out: *mut *mut HttpResponse,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        match (unsafe { client.as_ref() }, unsafe { request.as_ref() }) {
            (Some(client), Some(request)) if !out.is_null() => {
                match client.send(request, unsafe { cancel.as_ref() }) {
                    Ok(response) => {
                        unsafe { out.write(Box::into_raw(Box::new(response))) };
                        None
                    }
                    Err(error) => error,
                }
            }
            _ => gen_error!(HttpClientErrorCode::InvalidArgument),
        },
        gen_error!(HttpClientErrorCode::Panic)
    )
}
