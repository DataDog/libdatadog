// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_agent_client::AgentClient`] and its builder.
//!
//! Both the `AgentClientBuilder` and `AgentClient` are owned by C as
//! opaque `Box<...>` pointers. C never reaches into them; all
//! manipulation goes through the `ddog_agent_client_*` and
//! `ddog_agent_client_builder_*` functions exported here.
//!
//! This crate intentionally does **not** redeclare a `RetryConfig`
//! handle: callers configure a `Box<RetryConfig>` via the
//! `ddog_retry_config_*` functions in `libdd-http-client-ffi` and pass
//! the resulting handle into
//! [`ddog_agent_client_builder_set_retry`].

use crate::error::DdogAgentClientError;
use libdd_agent_client::{AgentClient, AgentClientBuilder, LanguageMetadata};
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::CharSlice;
use libdd_http_client::RetryConfig;
use std::ptr::NonNull;
use std::time::Duration;

// -----------------------------------------------------------------------------
// AgentClientBuilder
// -----------------------------------------------------------------------------

/// Allocate a new [`AgentClientBuilder`] with default settings.
///
/// Writes a `Box<AgentClientBuilder>` into `*out_handle`. The caller
/// owns the handle and must eventually pass it to either
/// [`ddog_agent_client_builder_build`] (which consumes it) or
/// [`ddog_agent_client_builder_drop`].
///
/// Before calling this for the first time in a process, the caller
/// must install a rustls crypto provider via
/// `ddog_http_client_install_default_crypto_provider` (non-FIPS) or
/// `ddog_http_client_init_fips` (FIPS) from `libdd-http-client-ffi`.
///
/// # Safety
/// `out_handle` must be a valid, writable pointer to an
/// uninitialised `*mut ddog_AgentClientBuilder`.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_new(
    out_handle: NonNull<Box<AgentClientBuilder>>,
) {
    out_handle
        .as_ptr()
        .write(Box::new(AgentClientBuilder::new()));
}

/// Configure the agent client to connect over HTTP to the given host
/// and port (e.g. `("localhost", 8126)`).
///
/// # Safety
/// `builder` must be valid; `host` must point to valid UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_http_endpoint(
    builder: Option<&mut AgentClientBuilder>,
    host: CharSlice,
    port: u16,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let host_str = match host.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "host is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.http(host_str, port);
    None
}

/// Configure the agent client to connect over HTTPS to the given host
/// and port.
///
/// # Safety
/// `builder` must be valid; `host` must point to valid UTF-8 memory.
#[cfg(feature = "https")]
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_https_endpoint(
    builder: Option<&mut AgentClientBuilder>,
    host: CharSlice,
    port: u16,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let host_str = match host.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "host is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.https(host_str, port);
    None
}

/// Route all connections through the given Unix Domain Socket path.
///
/// # Safety
/// `builder` must be valid; `path` must point to valid UTF-8 memory.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_unix_socket(
    builder: Option<&mut AgentClientBuilder>,
    path: CharSlice,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let path_str = match path.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "unix socket path is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.unix_socket(std::path::PathBuf::from(path_str));
    None
}

/// Route all connections through the given Windows Named Pipe.
///
/// # Safety
/// `builder` must be valid; `path` must point to valid UTF-8 memory.
#[cfg(windows)]
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_named_pipe(
    builder: Option<&mut AgentClientBuilder>,
    path: CharSlice,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let path_str = match path.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "named pipe is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.windows_named_pipe(std::ffi::OsString::from(path_str));
    None
}

/// Set the test session token (`x-datadog-test-session-token` header).
///
/// # Safety
/// `builder` must be valid; `token` must point to valid UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_test_session_token(
    builder: Option<&mut AgentClientBuilder>,
    token: CharSlice,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let token_str = match token.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "token is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.test_agent_session_token(token_str);
    None
}

/// Set the request timeout in milliseconds. Defaults to 2 000 ms when
/// not set.
///
/// # Safety
/// `builder` must be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_timeout_millis(
    builder: Option<&mut AgentClientBuilder>,
    timeout_ms: u64,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.timeout(Duration::from_millis(timeout_ms));
    None
}

/// Override the default retry configuration. Takes ownership of the
/// `RetryConfig`: the caller must not reuse or free it.
///
/// The `RetryConfig` is the same handle used by `libdd-http-client-ffi`
/// — build it via `ddog_retry_config_new` and the
/// `ddog_retry_config_set_*` family from that crate.
///
/// # Safety
/// `builder` must be valid; `cfg` must be `None` or a config produced
/// by `ddog_retry_config_new` (in `libdd-http-client-ffi`) and not yet
/// consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_retry(
    builder: Option<&mut AgentClientBuilder>,
    cfg: Option<Box<RetryConfig>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let Some(cfg) = cfg else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "retry config is null",
        )));
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.retry(*cfg);
    None
}

/// Set the language/runtime metadata. Required before
/// [`ddog_agent_client_builder_build`]. Takes ownership of the metadata
/// handle: the caller must not reuse or free it.
///
/// # Safety
/// `builder` must be valid; `metadata` must be `None` or a handle
/// produced by [`crate::ddog_language_metadata_new`] and not yet
/// consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_language_metadata(
    builder: Option<&mut AgentClientBuilder>,
    metadata: Option<Box<LanguageMetadata>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let Some(metadata) = metadata else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "metadata is null",
        )));
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.language_metadata(*metadata);
    None
}

/// Allow connection pooling. Defaults to `false`.
///
/// The Datadog agent has a low keep-alive timeout that causes "pipe
/// closed" errors on every second connection — `false` is correct for
/// all periodic-flush writers (traces, stats, data streams). Set to
/// `true` only for high-frequency continuous senders.
///
/// # Safety
/// `builder` must be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_set_allow_connection_pooling(
    builder: Option<&mut AgentClientBuilder>,
    allow: bool,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.allow_connection_pooling(allow);
    None
}

/// Append an extra custom header that will be injected on every
/// outgoing request.
///
/// **Single-set semantics:** the underlying Rust builder's
/// `extra_headers` setter replaces the entire vector and exposes no
/// getter, so each call to this function REPLACES the previously-set
/// extra headers. Callers that need multiple headers should set them
/// in a single call by repeating this function once per header — but
/// only the last call's header survives until the Rust crate exposes
/// an additive setter. (Tracked separately; out of scope for the FFI
/// task. The five `Datadog-Meta-*`, `User-Agent`,
/// container/entity-ID, and test-token headers are injected
/// automatically and don't go through this path.)
///
/// # Safety
/// `builder` must be valid; `name` and `value` must point to valid
/// UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_add_extra_header(
    builder: Option<&mut AgentClientBuilder>,
    name: CharSlice,
    value: CharSlice,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    let name_str = match name.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "header name is not valid UTF-8: {e}"
            ))))
        }
    };
    let value_str = match value.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "header value is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(b, AgentClientBuilder::new());
    *b = taken.extra_headers(vec![(name_str, value_str)]);
    None
}

/// Consume the builder and produce an [`AgentClient`].
///
/// On success writes a `Box<AgentClient>` into `*out_handle` and
/// returns `None`. On failure, leaves `*out_handle` unchanged and
/// returns an error. The builder is consumed in either case.
///
/// # Safety
/// `builder` must have been produced by
/// [`ddog_agent_client_builder_new`]. `out_handle` must be a valid,
/// writable pointer to an uninitialised `*mut ddog_AgentClient`.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_build(
    builder: Option<Box<AgentClientBuilder>>,
    out_handle: NonNull<Box<AgentClient>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(b) = builder else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "builder is null",
        )));
    };
    match (*b).build() {
        Ok(client) => {
            out_handle.as_ptr().write(Box::new(client));
            None
        }
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Drop a builder without building. Use when an error occurs partway
/// through configuration and you wish to abandon the builder.
///
/// # Safety
/// `builder` must be `None` or a builder produced by
/// [`ddog_agent_client_builder_new`] and not yet consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_builder_drop(
    builder: Option<Box<AgentClientBuilder>>,
) {
    drop(builder)
}

// -----------------------------------------------------------------------------
// AgentClient
// -----------------------------------------------------------------------------

/// Drop an [`AgentClient`].
///
/// # Safety
/// `client` must be `None` or a client produced by
/// [`ddog_agent_client_builder_build`] and not yet dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_drop(client: Option<Box<AgentClient>>) {
    drop(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn ensure_crypto_provider() {
        // The agent-client crate's tests install the provider directly via
        // rustls; mirror that here so the build-time check passes for tests
        // that exercise build().
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    fn make_metadata() -> Box<LanguageMetadata> {
        Box::new(LanguageMetadata::new("python", "3.12", "CPython", "2.0"))
    }

    #[test]
    fn builder_lifecycle_drop_only() {
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let builder = builder.assume_init();
            ddog_agent_client_builder_drop(Some(builder));
        }
    }

    #[test]
    fn builder_build_success() {
        ensure_crypto_provider();
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let err = ddog_agent_client_builder_set_http_endpoint(
                builder.as_deref_mut(),
                cs("localhost"),
                8126,
            );
            assert!(err.is_none());

            let err = ddog_agent_client_builder_set_timeout_millis(builder.as_deref_mut(), 1000);
            assert!(err.is_none());

            let err = ddog_agent_client_builder_set_language_metadata(
                builder.as_deref_mut(),
                Some(make_metadata()),
            );
            assert!(err.is_none());

            let err = ddog_agent_client_builder_set_allow_connection_pooling(
                builder.as_deref_mut(),
                false,
            );
            assert!(err.is_none());

            let mut client_handle: MaybeUninit<Box<AgentClient>> = MaybeUninit::uninit();
            let err = ddog_agent_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            let client = client_handle.assume_init();
            ddog_agent_client_drop(Some(client));
        }
    }

    #[test]
    fn builder_missing_transport_returns_error() {
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let _ = ddog_agent_client_builder_set_language_metadata(
                builder.as_deref_mut(),
                Some(make_metadata()),
            );

            let mut client_handle: MaybeUninit<Box<AgentClient>> = MaybeUninit::uninit();
            let err = ddog_agent_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogAgentClientErrorCode::MissingTransport
            );
        }
    }

    #[test]
    fn builder_missing_language_metadata_returns_error() {
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let _ = ddog_agent_client_builder_set_http_endpoint(
                builder.as_deref_mut(),
                cs("localhost"),
                8126,
            );

            let mut client_handle: MaybeUninit<Box<AgentClient>> = MaybeUninit::uninit();
            let err = ddog_agent_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogAgentClientErrorCode::MissingLanguageMetadata
            );
        }
    }

    #[test]
    fn builder_null_arg() {
        unsafe {
            let err = ddog_agent_client_builder_set_timeout_millis(None, 1000);
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogAgentClientErrorCode::InvalidArgument
            );
        }
    }

    #[test]
    fn builder_set_retry_attaches_config() {
        ensure_crypto_provider();
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());

            let _ = ddog_agent_client_builder_set_http_endpoint(
                builder.as_deref_mut(),
                cs("localhost"),
                8126,
            );
            let _ = ddog_agent_client_builder_set_language_metadata(
                builder.as_deref_mut(),
                Some(make_metadata()),
            );

            let cfg = Box::new(RetryConfig::new().max_retries(2));
            let err = ddog_agent_client_builder_set_retry(builder.as_deref_mut(), Some(cfg));
            assert!(err.is_none());

            let mut client_handle: MaybeUninit<Box<AgentClient>> = MaybeUninit::uninit();
            let err = ddog_agent_client_builder_build(
                builder.take(),
                NonNull::new_unchecked(client_handle.as_mut_ptr()),
            );
            assert!(err.is_none());

            ddog_agent_client_drop(Some(client_handle.assume_init()));
        }
    }

    #[test]
    fn builder_set_test_token_no_op() {
        unsafe {
            let mut builder: MaybeUninit<Box<AgentClientBuilder>> = MaybeUninit::uninit();
            ddog_agent_client_builder_new(NonNull::new_unchecked(builder.as_mut_ptr()));
            let mut builder = Some(builder.assume_init());
            let err = ddog_agent_client_builder_set_test_session_token(
                builder.as_deref_mut(),
                cs("tok"),
            );
            assert!(err.is_none());
            ddog_agent_client_builder_drop(builder.take());
        }
    }
}
