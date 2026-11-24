// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::HttpCancellationToken;
use crate::catch_panic;
use crate::error::{HttpClientError, HttpClientErrorCode};
use crate::gen_error;
use crate::request::{
    HttpHeader, HttpMethod, HttpRequest, bytes_arg, headers_arg, merge_headers, parts_arg, str_arg,
};
use crate::response::HttpResponse;
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client::{
    HttpClient as InnerClient, HttpClientError as UpstreamError, HttpMethod as InnerMethod,
    HttpRequest as InnerRequest, RetryConfig,
};
use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Read by [`crate::ddog_http_client_new`] and not retained. `base_url` and `timeout_ms` are
/// required; for every other field, zero or empty selects the default.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpClientConfig<'a> {
    /// Relative request URLs are appended to it, e.g. `http://localhost:8126`. The host is ignored
    /// when `unix_socket_path` is set.
    pub base_url: CharSlice<'a>,
    /// Default timeout of a request.
    pub timeout_ms: u64,
    /// Connect through this Unix socket instead of TCP (Unix only).
    pub unix_socket_path: CharSlice<'a>,
    /// Added to every request. A request header of the same name (case-insensitive) replaces it.
    pub default_headers: Slice<'a, HttpHeader<'a>>,
    /// For clients used for periodic flushes: pooled connections expire quickly.
    pub periodic: bool,
    /// Retries of a failed send (never of an HTTP status), with backoff. `0` disables them.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub retry_initial_delay_ms: u64,
}

#[derive(Debug)]
pub struct HttpClient {
    inner: InnerClient,
    runtime: Arc<ForkSafeRuntime>,
    base: String,
    default_headers: Vec<(String, String)>,
}

impl HttpClient {
    /// # Safety
    ///
    /// `runtime` must be a live `Arc::into_raw` pointer, or `NULL` (rejected).
    unsafe fn from_config(
        config: &HttpClientConfig<'_>,
        runtime: *const ForkSafeRuntime,
    ) -> Result<Self, Option<Box<HttpClientError>>> {
        if runtime.is_null() {
            return Err(gen_error!(HttpClientErrorCode::InvalidArgument));
        }
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
        // reqwest builds a default rustls config, and panics, if the process has no crypto
        // provider; `libdd-http-client` only covers https base URLs. Feature unification can
        // select reqwest even when this crate did not ask for it.
        // `install_default` fails when one is already installed, and the host's is kept.
        #[cfg(feature = "https")]
        {
            static INSTALL_PROVIDER: std::sync::Once = std::sync::Once::new();
            INSTALL_PROVIDER.call_once(|| {
                let _ = rustls::crypto::ring::default_provider().install_default();
            });
        }
        let inner = builder
            .build()
            .map_err(|e| Some(Box::new(HttpClientError::from(e))))?;

        // SAFETY: `runtime` is non-null and live; takes its own strong reference.
        let runtime = unsafe {
            Arc::increment_strong_count(runtime);
            Arc::from_raw(runtime)
        };

        Ok(Self {
            inner,
            runtime,
            base,
            default_headers,
        })
    }

    fn build_request(
        &self,
        request: &HttpRequest<'_>,
    ) -> Result<InnerRequest, Option<Box<HttpClientError>>> {
        let url = full_url(&self.base, str_arg(request.url)?)?;
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
                    // An already cancelled token must not start a request. Older `tokio-util`
                    // versions of `run_until_cancelled` poll the future before checking it.
                    Some(token) if token.is_cancelled() => None,
                    Some(token) => {
                        token
                            .run_until_cancelled(self.inner.send(inner_request))
                            .await
                    }
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

fn full_url(base: &str, url: &str) -> Result<String, Option<Box<HttpClientError>>> {
    let is_https = |s| has_prefix_ignore_case(s, "https://");
    if is_https(url) || has_prefix_ignore_case(url, "http://") {
        // The reqwest backend cannot send a scheme other than its base URL's, so every backend
        // gets the same restriction.
        if is_https(url) == is_https(base) {
            Ok(url.to_owned())
        } else {
            Err(gen_error!(HttpClientErrorCode::InvalidArgument))
        }
    } else if url.starts_with('/') && !url.starts_with("//") {
        Ok(format!("{base}{url}"))
    } else {
        Err(gen_error!(HttpClientErrorCode::InvalidArgument))
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

/// Creates a client and writes it to `*out`. Returns `NULL` on success, and the caller then owns
/// `*out` and releases it with [`ddog_http_client_drop`]. On failure `*out` is untouched.
///
/// No connection is opened until the first request.
///
/// `runtime` is required (`NULL` is rejected with `InvalidArgument`): a `ForkSafeRuntime` pointer
/// from `ddog_shared_runtime_new`. The client takes its own reference, so the caller may free its
/// own right away.
///
/// If the process forks, the host must run the fork protocol on `runtime`
/// (`ddog_shared_runtime_before_fork`, then `ddog_shared_runtime_after_fork_parent` in the parent
/// or `ddog_shared_runtime_after_fork_child` in the child) before using the client again.
///
/// # Safety
///
/// * `config` must be `NULL` (rejected with `InvalidArgument`) or point to a valid
///   [`HttpClientConfig`] whose slices are valid for reads.
/// * `runtime` must be `NULL` (rejected with `InvalidArgument`) or a pointer obtained from
///   `ddog_shared_runtime_new` that has not been freed.
/// * `out` must be `NULL` (rejected with `InvalidArgument`) or valid for writes of one pointer.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_http_client_new(
    config: Option<&HttpClientConfig<'_>>,
    runtime: *const ForkSafeRuntime,
    out: Option<NonNull<Box<HttpClient>>>,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        match (config, out) {
            (Some(config), Some(out)) => {
                match unsafe { HttpClient::from_config(config, runtime) } {
                    Ok(client) => {
                        unsafe { out.as_ptr().write(Box::new(client)) };
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
    client: Option<&HttpClient>,
    request: Option<&HttpRequest<'_>>,
    cancel: Option<&HttpCancellationToken>,
    out: Option<NonNull<Box<HttpResponse>>>,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        match (client, request, out) {
            (Some(client), Some(request), Some(out)) => {
                match client.send(request, cancel) {
                    Ok(response) => {
                        unsafe { out.as_ptr().write(Box::new(response)) };
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

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common_ffi::{CharSlice, Slice};

    fn runtime() -> Arc<ForkSafeRuntime> {
        Arc::new(ForkSafeRuntime::with_worker_threads(1).expect("runtime"))
    }

    fn config(base_url: CharSlice<'_>, timeout_ms: u64) -> HttpClientConfig<'_> {
        HttpClientConfig {
            base_url,
            timeout_ms,
            unix_socket_path: CharSlice::default(),
            default_headers: Slice::default(),
            periodic: false,
            max_retries: 0,
            retry_initial_delay_ms: 0,
        }
    }

    fn code<T: std::fmt::Debug>(
        result: Result<T, Option<Box<HttpClientError>>>,
    ) -> HttpClientErrorCode {
        result.expect_err("an error").expect("a boxed error").code
    }

    fn rejected(base_url: &str, timeout_ms: u64) -> HttpClientErrorCode {
        let config = config(CharSlice::from(base_url), timeout_ms);
        let runtime = runtime();
        code(unsafe { HttpClient::from_config(&config, Arc::as_ptr(&runtime)) })
    }

    // Exhaustive on purpose: a new upstream method stops this compiling until someone
    // decides whether to expose it.
    fn outer_method(method: InnerMethod) -> HttpMethod {
        match method {
            InnerMethod::Get => HttpMethod::Get,
            InnerMethod::Post => HttpMethod::Post,
            InnerMethod::Put => HttpMethod::Put,
            InnerMethod::Delete => HttpMethod::Delete,
            InnerMethod::Head => HttpMethod::Head,
            InnerMethod::Patch => HttpMethod::Patch,
            InnerMethod::Options => HttpMethod::Options,
        }
    }

    #[test]
    fn every_method_maps_to_the_same_upstream_method_and_back() {
        for method in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Delete,
            HttpMethod::Head,
            HttpMethod::Patch,
            HttpMethod::Options,
        ] {
            assert_eq!(outer_method(inner_method(method)), method);
        }
    }

    #[test]
    fn a_config_without_a_base_url_is_rejected() {
        assert_eq!(rejected("", 1_000), HttpClientErrorCode::InvalidConfig);
    }

    #[test]
    fn a_config_with_a_zero_timeout_is_rejected() {
        assert_eq!(
            rejected("http://localhost", 0),
            HttpClientErrorCode::InvalidConfig
        );
    }

    #[test]
    fn a_base_url_that_is_not_a_url_is_rejected() {
        assert_eq!(
            rejected("not a url", 1_000),
            HttpClientErrorCode::InvalidConfig
        );
    }

    #[test]
    fn a_base_url_with_an_unsupported_scheme_is_rejected() {
        assert_eq!(
            rejected("ftp://localhost", 1_000),
            HttpClientErrorCode::InvalidConfig
        );
    }

    #[test]
    fn a_base_url_that_is_not_utf8_is_an_invalid_argument() {
        let invalid = [b'h', b't', b't', b'p', b':', b'/', b'/', 0xff, 0xfe];
        let config = config(CharSlice::from_bytes(&invalid), 1_000);

        let runtime = runtime();

        let result = unsafe { HttpClient::from_config(&config, Arc::as_ptr(&runtime)) };

        assert_eq!(code(result), HttpClientErrorCode::InvalidArgument);
    }

    #[test]
    fn new_rejects_a_null_config_and_a_null_out_pointer() {
        let mut out: Option<Box<HttpClient>> = None;
        let config = config(CharSlice::from("http://localhost"), 1_000);

        let runtime = runtime();
        let rt = Arc::as_ptr(&runtime);

        let no_config =
            unsafe { ddog_http_client_new(None, rt, Some(NonNull::from(&mut out).cast())) };
        let no_out = unsafe { ddog_http_client_new(Some(&config), rt, None) };

        assert_eq!(
            no_config.expect("an error").code,
            HttpClientErrorCode::InvalidArgument
        );
        assert_eq!(
            no_out.expect("an error").code,
            HttpClientErrorCode::InvalidArgument
        );
    }

    #[test]
    fn new_leaves_the_out_pointer_untouched_on_error() {
        let config = config(CharSlice::from(""), 1_000);
        let mut out: Option<Box<HttpClient>> = None;

        let runtime = runtime();

        let error = unsafe {
            ddog_http_client_new(
                Some(&config),
                Arc::as_ptr(&runtime),
                Some(NonNull::from(&mut out).cast()),
            )
        };

        assert!(error.is_some());
        assert!(out.is_none());
    }

    #[test]
    fn dropping_a_null_client_is_a_no_op() {
        unsafe { ddog_http_client_drop(None) };
    }

    #[test]
    fn base_urls_are_reduced_to_scheme_authority_and_path() {
        for (input, expected) in [
            ("http://localhost:8126", "http://localhost:8126"),
            ("http://localhost:8126/", "http://localhost:8126"),
            ("https://example.com/prefix/", "https://example.com/prefix"),
            ("http://localhost?a=1#frag", "http://localhost"),
        ] {
            assert_eq!(
                normalize_base_url(input).expect("a valid base url"),
                expected
            );
        }
    }

    #[test]
    fn absolute_urls_are_used_as_they_are() {
        for (base, url) in [
            ("http://base", "http://other:1/x?a=1"),
            ("http://base", "HTTP://Other/x"),
            ("https://base", "HTTPS://Other/x"),
        ] {
            assert_eq!(full_url(base, url).expect("an absolute url"), url);
        }
    }

    #[test]
    fn an_absolute_url_with_another_scheme_than_the_base_is_an_invalid_argument() {
        for (base, url) in [
            ("http://base", "https://other/x"),
            ("https://base", "http://other/x"),
        ] {
            assert_eq!(
                code(full_url(base, url)),
                HttpClientErrorCode::InvalidArgument,
                "{base} {url}"
            );
        }
    }

    #[test]
    fn paths_are_appended_to_the_base_url() {
        assert_eq!(
            full_url("http://base:1", "/v0.4/traces?a=1").expect("a path"),
            "http://base:1/v0.4/traces?a=1"
        );
        let base = normalize_base_url("http://base/prefix/").expect("a valid base url");
        assert_eq!(
            full_url(&base, "/p").expect("a path"),
            "http://base/prefix/p"
        );
    }

    #[test]
    fn a_url_that_is_neither_absolute_nor_a_path_is_an_invalid_argument() {
        for url in ["v0.4/traces", "", "ftp://base/x", "//base/x"] {
            assert_eq!(
                code(full_url("http://base", url)),
                HttpClientErrorCode::InvalidArgument,
                "{url:?}"
            );
        }
    }
}
