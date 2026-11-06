// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::HttpCancellationToken;
use crate::catch_panic;
use crate::error::{HttpClientError, HttpClientErrorCode};
use crate::request::{
    HttpClientConfig, HttpMethod, HttpRequest, bytes_arg, headers_arg, merge_headers, parts_arg,
    str_arg,
};
use crate::response::HttpResponse;
use libdd_http_client::{
    HttpClient as InnerClient, HttpMethod as InnerMethod, HttpRequest as InnerRequest, RetryConfig,
};
use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime};
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(feature = "catch_panic")]
use crate::error::panic_error;

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
    ) -> Result<Self, HttpClientError> {
        let base_url = str_arg(config.base_url, "base_url")?;
        if base_url.is_empty() {
            return Err(HttpClientError::invalid_config("base_url is required"));
        }
        if config.timeout_ms == 0 {
            return Err(HttpClientError::invalid_config(
                "timeout_ms must be greater than zero",
            ));
        }
        let base = normalize_base_url(base_url)?;
        let default_headers = headers_arg(config.default_headers, "default_headers")?;
        let unix_socket_path = str_arg(config.unix_socket_path, "unix_socket_path")?;

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
                return Err(HttpClientError::invalid_config(
                    "unix_socket_path is not supported on this platform",
                ));
            }
        }
        let inner = builder.build().map_err(HttpClientError::from_build)?;

        let runtime =
            if runtime.is_null() {
                Arc::new(ForkSafeRuntime::with_worker_threads(1).map_err(|e| {
                    HttpClientError::new(HttpClientErrorCode::Runtime, e.to_string())
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

    fn full_url(&self, url: &str) -> Result<String, HttpClientError> {
        if has_prefix_ignore_case(url, "http://") || has_prefix_ignore_case(url, "https://") {
            Ok(url.to_owned())
        } else if url.starts_with('/') && !url.starts_with("//") {
            Ok(format!("{}{url}", self.base))
        } else {
            Err(HttpClientError::invalid_argument(format!(
                "url {url:?} must be an absolute http(s) URL or a path starting with '/'"
            )))
        }
    }

    fn build_request(&self, request: &HttpRequest<'_>) -> Result<InnerRequest, HttpClientError> {
        let url = self.full_url(str_arg(request.url, "url")?)?;
        let headers = merge_headers(
            &self.default_headers,
            headers_arg(request.headers, "headers")?,
        );
        let body = bytes_arg(request.body, "body")?;
        let parts = parts_arg(request.multipart)?;
        if !body.is_empty() && !parts.is_empty() {
            return Err(HttpClientError::invalid_argument(
                "a request cannot have both a body and multipart parts",
            ));
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
fn normalize_base_url(base_url: &str) -> Result<String, HttpClientError> {
    let parsed = Url::parse(base_url).map_err(|e| {
        HttpClientError::invalid_config(format!("invalid base_url {base_url:?}: {e}"))
    })?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(HttpClientError::invalid_config(format!(
                "unsupported scheme {other:?} in base_url {base_url:?} (use http or https)"
            )));
        }
    }
    if parsed.host().is_none() {
        return Err(HttpClientError::invalid_config(format!(
            "base_url {base_url:?} has no host"
        )));
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

unsafe fn new_client(
    config: *const HttpClientConfig<'_>,
    runtime: *const ForkSafeRuntime,
    out: *mut *mut HttpClient,
) -> Result<(), HttpClientError> {
    let config = unsafe { config.as_ref() }
        .ok_or_else(|| HttpClientError::invalid_argument("config is NULL"))?;
    if out.is_null() {
        return Err(HttpClientError::invalid_argument("out is NULL"));
    }
    let client = unsafe { HttpClient::from_config(config, runtime) }?;
    unsafe { out.write(Box::into_raw(Box::new(client))) };
    Ok(())
}

unsafe fn send_request(
    client: *const HttpClient,
    request: *const HttpRequest<'_>,
    cancel: *const HttpCancellationToken,
    out: *mut *mut HttpResponse,
) -> Result<(), HttpClientError> {
    let client = unsafe { client.as_ref() }
        .ok_or_else(|| HttpClientError::invalid_argument("client is NULL"))?;
    let request = unsafe { request.as_ref() }
        .ok_or_else(|| HttpClientError::invalid_argument("request is NULL"))?;
    if out.is_null() {
        return Err(HttpClientError::invalid_argument("out is NULL"));
    }
    let cancel = unsafe { cancel.as_ref() }.map(HttpCancellationToken::cancellation_token);

    let inner_request = client.build_request(request)?;
    let outcome = client
        .runtime
        .block_on(async {
            match cancel {
                Some(token) => until_cancelled(token, client.inner.send(inner_request)).await,
                None => Some(client.inner.send(inner_request).await),
            }
        })
        .map_err(|e| {
            HttpClientError::new(
                HttpClientErrorCode::Runtime,
                format!("the runtime could not run the request: {e}"),
            )
        })?;

    match outcome {
        None => Err(HttpClientError::new(
            HttpClientErrorCode::Cancelled,
            "request cancelled",
        )),
        Some(Err(error)) => Err(HttpClientError::from_send(error)),
        Some(Ok(response)) => {
            unsafe { out.write(Box::into_raw(Box::new(HttpResponse::new(response)))) };
            Ok(())
        }
    }
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
        unsafe { new_client(config, runtime, out) }
            .err()
            .map(Box::new),
        panic_error()
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
        unsafe { send_request(client, request, cancel, out) }
            .err()
            .map(Box::new),
        panic_error()
    )
}
