// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_http_client::HttpClient`] and its builder.
//!
//! Both the `HttpClientBuilder` and `HttpClient` are owned by C as opaque
//! `Box<...>` pointers. C never reaches into them; all manipulation goes
//! through the `ddog_http_client_*` and `ddog_http_client_builder_*`
//! functions exported here.

use crate::error::DdogHttpClientError;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::CharSlice;
use libdd_http_client::{HttpClient, HttpClientBuilder, RetryConfig};
use std::ptr::NonNull;
use std::sync::Once;
use std::time::Duration;

// -----------------------------------------------------------------------------
// Process-wide init
// -----------------------------------------------------------------------------

/// Install the rustls `ring` crypto provider as the process-wide default.
///
/// This MUST be called exactly once per process, before
/// [`ddog_http_client_builder_new`], unless the caller installs a
/// different provider themselves (for example via
/// [`ddog_http_client_init_fips`]).
///
/// Idempotent: if a provider is already installed, the second call is a
/// no-op and the first install wins. This means it's safe to call this
/// after a FIPS init has already run; the FIPS provider is preserved.
///
/// **Mutually exclusive with [`ddog_http_client_init_fips`].** Calling
/// both is a contract violation; whichever wins, wins. If FIPS users
/// also call this by mistake, the second call is a no-op (rustls's
/// install is "first wins").
///
/// # Safety
/// Safe to call from any thread, but ordering relative to first
/// `ddog_http_client_builder_new` matters: install the provider first.
#[no_mangle]
pub extern "C" fn ddog_http_client_install_default_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Install the FIPS-compliant rustls crypto provider as the process-wide
/// default.
///
/// This MUST be called exactly once per process, before
/// [`ddog_http_client_builder_new`]. It is the FIPS counterpart to
/// [`ddog_http_client_install_default_crypto_provider`] and is **mutually
/// exclusive** with it: calling both is a contract violation. The
/// underlying rustls install is "first wins", so whichever runs first
/// determines the provider; the second is a no-op.
///
/// Idempotent: re-calling this entry point in the same process is a
/// no-op once a provider is installed (regardless of which one).
///
/// Returns `None` on success or when the call was a no-op (a provider
/// was already installed). Currently never returns an error in this
/// build (the underlying init function is only available when the
/// `fips` feature is enabled on `libdd-http-client`); when that feature
/// is *not* enabled this entry point returns
/// [`crate::DdogHttpClientErrorCode::InvalidConfig`] with a message
/// indicating the FFI was built without FIPS support.
///
/// # Safety
/// Safe to call from any thread, but ordering relative to first
/// `ddog_http_client_builder_new` matters: install the provider first.
#[no_mangle]
pub extern "C" fn ddog_http_client_init_fips() -> Option<Box<DdogHttpClientError>> {
    #[cfg(feature = "fips")]
    {
        // Idempotent across the FFI: rustls's CryptoProvider::install_default
        // is itself "first wins", but it returns Err on subsequent calls. We
        // wrap with a Once so the FFI surface stays clean regardless of which
        // entry point ran first.
        static INIT: Once = Once::new();
        let mut inner_result: Result<(), libdd_http_client::HttpClientError> = Ok(());
        INIT.call_once(|| {
            inner_result = libdd_http_client::init_fips_crypto();
        });
        match inner_result {
            Ok(()) => None,
            Err(err) => Some(Box::new(DdogHttpClientError::from(err))),
        }
    }
    #[cfg(not(feature = "fips"))]
    {
        Some(Box::new(DdogHttpClientError::new(
            crate::DdogHttpClientErrorCode::InvalidConfig,
            "libdd-http-client-ffi was built without the `fips` feature",
        )))
    }
}

// -----------------------------------------------------------------------------
// HttpClientBuilder
// -----------------------------------------------------------------------------

/// Allocate a new [`HttpClientBuilder`] with default settings.
///
/// Writes a `Box<HttpClientBuilder>` into `*out_handle`. The caller owns
/// the handle and must eventually pass it to either
/// [`ddog_http_client_builder_build`] (which consumes it) or
/// [`ddog_http_client_builder_drop`].
///
/// Before calling this for the first time in a process, the caller must
/// install a rustls crypto provider via either
/// [`ddog_http_client_install_default_crypto_provider`] (non-FIPS) or
/// `ddog_http_client_init_fips` (FIPS, Task 9b).
///
/// # Safety
/// `out_handle` must be a valid, writable pointer to an
/// uninitialised `*mut ddog_HttpClientBuilder`.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_new(
    out_handle: NonNull<Box<HttpClientBuilder>>,
) {
    out_handle
        .as_ptr()
        .write(Box::new(HttpClientBuilder::new()));
}

/// Set the default request timeout in milliseconds.
///
/// Required before [`ddog_http_client_builder_build`].
///
/// # Safety
/// `builder` must be `None` or a valid mutable reference to a builder
/// previously produced by [`ddog_http_client_builder_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_timeout(
    builder: Option<&mut HttpClientBuilder>,
    timeout_ms: u64,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    // Move out, mutate, swap back. HttpClientBuilder's setters take `self`
    // so we go through std::mem::replace.
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.timeout(Duration::from_millis(timeout_ms));
    None
}

/// Set the base URL for the client.
///
/// `url` must be valid UTF-8. Required before
/// [`ddog_http_client_builder_build`].
///
/// # Safety
/// `builder` must be valid; `url` must point to valid memory for its
/// declared length.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_base_url(
    builder: Option<&mut HttpClientBuilder>,
    url: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    let url_str = match url.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "url is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.base_url(url_str);
    None
}

/// Route all connections through the given Unix Domain Socket path.
///
/// The host portion of any URL is ignored once a socket is set.
///
/// # Safety
/// `builder` must be valid; `path` must point to valid memory for its
/// declared length and contain valid UTF-8.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_unix_socket(
    builder: Option<&mut HttpClientBuilder>,
    path: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    let path_str = match path.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "unix socket path is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.unix_socket(std::path::PathBuf::from(path_str));
    None
}

/// Route all connections through the given Windows Named Pipe.
///
/// # Safety
/// `builder` must be valid; `pipe` must point to valid memory for its
/// declared length and contain valid UTF-8.
#[cfg(windows)]
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_named_pipe(
    builder: Option<&mut HttpClientBuilder>,
    pipe: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    let pipe_str = match pipe.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "named pipe is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.windows_named_pipe(std::ffi::OsString::from(pipe_str));
    None
}

/// Configure connection pooling. Defaults to `true`.
///
/// # Safety
/// `builder` must be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_allow_connection_pooling(
    builder: Option<&mut HttpClientBuilder>,
    allow: bool,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.allow_connection_pooling(allow);
    None
}

/// Enable automatic retries on the resulting client using the given
/// configuration. Takes ownership of the config: the caller must not
/// reuse or free it.
///
/// All errors are retried except
/// [`crate::DdogHttpClientErrorCode::InvalidConfig`].
///
/// # Safety
/// `builder` must be `None` or a valid mutable reference to a builder
/// produced by [`ddog_http_client_builder_new`]. `cfg` must be `None` or
/// a config produced by [`crate::ddog_retry_config_new`] and not yet
/// consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_set_retry(
    builder: Option<&mut HttpClientBuilder>,
    cfg: Option<Box<RetryConfig>>,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    let Some(cfg) = cfg else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "retry config is null",
        )));
    };
    let taken = std::mem::replace(b, HttpClientBuilder::new());
    *b = taken.retry(*cfg);
    None
}

/// Consume the builder and produce an [`HttpClient`].
///
/// On success writes a `Box<HttpClient>` into `*out_handle` and returns
/// `None`. On failure, leaves `*out_handle` unchanged and returns an
/// error. The builder is consumed in either case.
///
/// # Safety
/// `builder` must have been produced by
/// [`ddog_http_client_builder_new`]. `out_handle` must be a valid,
/// writable pointer to an uninitialised `*mut ddog_HttpClient`.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_build(
    builder: Option<Box<HttpClientBuilder>>,
    out_handle: NonNull<Box<HttpClient>>,
) -> Option<Box<DdogHttpClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "builder is null",
        )));
    };
    match (*b).build() {
        Ok(client) => {
            out_handle.as_ptr().write(Box::new(client));
            None
        }
        Err(err) => Some(Box::new(DdogHttpClientError::from(err))),
    }
}

/// Drop a builder without building. Use when an error occurs partway
/// through configuration and you wish to abandon the builder.
///
/// # Safety
/// `builder` must be `None` or a builder produced by
/// [`ddog_http_client_builder_new`] and not yet consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_builder_drop(
    builder: Option<Box<HttpClientBuilder>>,
) {
    drop(builder)
}

// -----------------------------------------------------------------------------
// HttpClient
// -----------------------------------------------------------------------------

/// Drop an [`HttpClient`].
///
/// # Safety
/// `client` must be `None` or a client produced by
/// [`ddog_http_client_builder_build`] and not yet dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_drop(client: Option<Box<HttpClient>>) {
    drop(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn ensure_crypto_provider() {
        ddog_http_client_install_default_crypto_provider();
    }

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn builder_lifecycle_drop_only() {
        unsafe {
            let mut builder: MaybeUninit<Box<HttpClientBuilder>> = MaybeUninit::uninit();
            ddog_http_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let builder = builder.assume_init();
            ddog_http_client_builder_drop(Some(builder));
        }
    }

    #[test]
    fn builder_build_success() {
        ensure_crypto_provider();
        unsafe {
            let mut builder: MaybeUninit<Box<HttpClientBuilder>> = MaybeUninit::uninit();
            ddog_http_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let err = ddog_http_client_builder_set_base_url(
                builder.as_deref_mut(),
                cs("http://localhost:8126"),
            );
            assert!(err.is_none());

            let err = ddog_http_client_builder_set_timeout(builder.as_deref_mut(), 1000);
            assert!(err.is_none());

            let err = ddog_http_client_builder_set_allow_connection_pooling(
                builder.as_deref_mut(),
                false,
            );
            assert!(err.is_none());

            let mut client_handle: MaybeUninit<Box<HttpClient>> = MaybeUninit::uninit();
            let err = ddog_http_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            let client = client_handle.assume_init();
            ddog_http_client_drop(Some(client));
        }
    }

    #[test]
    fn builder_missing_base_url_returns_error() {
        unsafe {
            let mut builder: MaybeUninit<Box<HttpClientBuilder>> = MaybeUninit::uninit();
            ddog_http_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let _ = ddog_http_client_builder_set_timeout(builder.as_deref_mut(), 1000);

            let mut client_handle: MaybeUninit<Box<HttpClient>> = MaybeUninit::uninit();
            let err = ddog_http_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_some());
            let err = err.unwrap();
            assert_eq!(err.code, crate::DdogHttpClientErrorCode::InvalidConfig);
        }
    }

    #[test]
    fn builder_null_arg() {
        unsafe {
            let err = ddog_http_client_builder_set_timeout(None, 1000);
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
        }
    }

    #[test]
    fn builder_set_retry_attaches_config() {
        ensure_crypto_provider();
        unsafe {
            let mut builder: MaybeUninit<Box<HttpClientBuilder>> = MaybeUninit::uninit();
            ddog_http_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let _ = ddog_http_client_builder_set_base_url(
                builder.as_deref_mut(),
                cs("http://localhost:8126"),
            );
            let _ = ddog_http_client_builder_set_timeout(builder.as_deref_mut(), 1000);

            let cfg = Box::new(RetryConfig::new().max_retries(2));
            let err = ddog_http_client_builder_set_retry(builder.as_deref_mut(), Some(cfg));
            assert!(err.is_none());

            let mut client_handle: MaybeUninit<Box<HttpClient>> = MaybeUninit::uninit();
            let err = ddog_http_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_none());

            ddog_http_client_drop(Some(client_handle.assume_init()));
        }
    }

    #[test]
    fn builder_set_retry_null_arg() {
        unsafe {
            let cfg = Box::new(RetryConfig::new());
            let err = ddog_http_client_builder_set_retry(None, Some(cfg));
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
        }
    }

    #[test]
    fn builder_set_retry_null_cfg() {
        unsafe {
            let mut builder: MaybeUninit<Box<HttpClientBuilder>> = MaybeUninit::uninit();
            ddog_http_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());
            let err = ddog_http_client_builder_set_retry(builder.as_deref_mut(), None);
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
            ddog_http_client_builder_drop(builder.take());
        }
    }

    #[test]
    fn init_fips_returns_invalid_config_when_feature_disabled() {
        // Without the `fips` feature, the entry point is expected to
        // return an InvalidConfig error rather than panicking. With the
        // feature enabled, it should succeed (or be a no-op if the
        // provider is already installed). We assert only on the
        // discriminant to keep this test feature-independent.
        let err = ddog_http_client_init_fips();
        if cfg!(feature = "fips") {
            // Either Ok or InvalidConfig from rustls already-installed.
            if let Some(e) = err {
                assert_eq!(e.code, crate::DdogHttpClientErrorCode::InvalidConfig);
            }
        } else {
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidConfig
            );
        }
    }
}
