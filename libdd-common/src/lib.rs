// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

use libdd_types::Endpoint;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub mod azure_app_services;
#[cfg(not(target_arch = "wasm32"))]
pub mod cc_utils;
#[cfg(not(target_arch = "wasm32"))]
pub mod connector;
#[cfg(feature = "reqwest")]
#[cfg(feature = "http-client")]
pub mod dump_server;
pub mod entity_id;
pub mod machine_id;
pub mod regex_engine;
#[macro_use]
pub mod cstr;
#[cfg(feature = "bench-utils")]
pub mod bench_utils;
pub mod config;
pub mod error;
#[cfg(feature = "http-client")]
pub mod http_common;
pub mod multipart;
#[cfg(not(target_arch = "wasm32"))]
pub mod rate_limiter;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
#[cfg(not(target_arch = "wasm32"))]
pub mod threading;
#[cfg(not(target_arch = "wasm32"))]
pub mod timeout;
pub mod unix_utils;

/// Extension trait for `Mutex` to provide a method that acquires a lock, panicking if the lock is
/// poisoned.
///
/// This helper function is intended to be used to avoid having to add many
/// `#[allow(clippy::unwrap_used)]` annotations if there are a lot of usages of `Mutex`.
///
/// # Arguments
///
/// * `self` - A reference to the `Mutex` to lock.
///
/// # Returns
///
/// A `MutexGuard` that provides access to the locked data.
///
/// # Panics
///
/// This function will panic if the `Mutex` is poisoned.
///
/// # Examples
///
/// ```
/// use libdd_common::MutexExt;
/// use std::sync::{Arc, Mutex};
///
/// let data = Arc::new(Mutex::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.lock_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.lock_or_panic(), 6);
/// ```
pub trait MutexExt<T> {
    fn lock_or_panic(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    #[inline(always)]
    #[track_caller]
    fn lock_or_panic(&self) -> MutexGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.lock().unwrap()
    }
}

/// Extension trait for `RwLock` to provide methods that acquire read/write locks, panicking if
/// the lock is poisoned.
///
/// Mirrors [`MutexExt`] for `RwLock` so callers avoid `#[allow(clippy::unwrap_used)]` at each
/// lock site.
///
/// # Examples
///
/// ```
/// use libdd_common::RwLockExt;
/// use std::sync::{Arc, RwLock};
///
/// let data = Arc::new(RwLock::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.write_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.read_or_panic(), 6);
/// ```
pub trait RwLockExt<T> {
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T>;
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    #[inline(always)]
    #[track_caller]
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.read().unwrap()
    }

    #[inline(always)]
    #[track_caller]
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.write().unwrap()
    }
}

/// Extension trait that extracts the value from a `Result` whose error type is uninhabited.
///
/// The signature constrains callers at compile time: the method is only available when the
/// error type is [`core::convert::Infallible`]. No panics — the compiler proves the `Err`
/// arm unreachable from the type.
///
/// # Examples
///
/// ```
/// use libdd_common::ResultInfallibleExt;
/// use std::convert::Infallible;
///
/// let result: Result<i32, Infallible> = Ok(42);
/// assert_eq!(result.unwrap_infallible(), 42);
/// ```
pub trait ResultInfallibleExt<T>: sealed::Sealed {
    fn unwrap_infallible(self) -> T;
}

impl<T> ResultInfallibleExt<T> for Result<T, core::convert::Infallible> {
    #[inline(always)]
    fn unwrap_infallible(self) -> T {
        match self {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for Result<T, core::convert::Infallible> {}
}

#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub type HttpResponse = http_common::HttpResponse;
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub trait Connect:
    hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static
{
}
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
impl<C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static> Connect
    for C
{
}

/// Extension trait providing the networking-dependent [`Endpoint`] behavior that this crate's
/// `entity_id`, `connector`, and `dump_server` modules make possible. The [`Endpoint`] type
/// itself lives in `libdd-types` and stays free of those dependencies; these methods live here
/// until that networking code is split into its own crate.
pub trait EndpointExt {
    /// Apply standard headers (user-agent, api-key, test-token, entity headers) to an
    /// [`http::request::Builder`].
    fn set_standard_headers(
        &self,
        builder: http::request::Builder,
        user_agent: &str,
    ) -> http::request::Builder;

    /// Return a request builder with the following headers:
    /// - User agent
    /// - Api key
    /// - Container Id/Entity Id
    fn to_request_builder(
        &self,
        user_agent: &str,
    ) -> anyhow::Result<libdd_types::HttpRequestBuilder>;

    /// Creates a reqwest ClientBuilder configured for this endpoint.
    ///
    /// This method handles various endpoint schemes:
    /// - `http`/`https`: Standard HTTP(S) endpoints
    /// - `unix`: Unix domain sockets (Unix only)
    /// - `windows`: Windows named pipes (Windows only)
    /// - `file`: File dump endpoints for debugging (spawns a local server to capture requests)
    ///
    /// The default in-process resolver is used for DNS (fork-safe). To use the system DNS resolver
    /// instead (less fork-safe), set [`Endpoint::use_system_resolver`] to true via
    /// [`Endpoint::with_system_resolver`].
    ///
    /// # Returns
    /// A tuple of (ClientBuilder, request_url) where:
    /// - ClientBuilder is configured with the appropriate transport and timeout
    /// - request_url is the URL string to use for HTTP requests
    ///
    /// # Errors
    /// Returns an error if:
    /// - The endpoint scheme is unsupported
    /// - Path decoding fails
    /// - The dump server fails to start (for file:// scheme)
    #[cfg(feature = "reqwest")]
    fn to_reqwest_client_builder(&self) -> anyhow::Result<(reqwest::ClientBuilder, String)>;
}

impl EndpointExt for Endpoint {
    fn set_standard_headers(
        &self,
        mut builder: http::request::Builder,
        user_agent: &str,
    ) -> http::request::Builder {
        builder = builder.header("user-agent", user_agent);
        for (name, value) in self.get_optional_headers() {
            builder = builder.header(name, value);
        }
        for (name, value) in entity_id::get_entity_headers() {
            builder = builder.header(name, value);
        }
        builder
    }

    fn to_request_builder(
        &self,
        user_agent: &str,
    ) -> anyhow::Result<libdd_types::HttpRequestBuilder> {
        let mut builder = http::Request::builder()
            .uri(self.url.clone())
            .header(http::header::USER_AGENT, user_agent);

        // Add optional endpoint headers (api-key, test-token)
        for (name, value) in self.get_optional_headers() {
            builder = builder.header(name, value);
        }

        // Add entity-related headers (container-id, entity-id, external-env)
        for (name, value) in entity_id::get_entity_headers() {
            builder = builder.header(name, value);
        }

        Ok(builder)
    }

    #[cfg(feature = "reqwest")]
    fn to_reqwest_client_builder(&self) -> anyhow::Result<(reqwest::ClientBuilder, String)> {
        use anyhow::Context;

        // Don't use proxies, as this calls `getenv` which is unsafe and not
        // just in theory. It can cause crashes with PHP where php-fpm's env
        // configuration will mutate the system environment (it doesn't pass
        // it as part of the SAPI env, it changes the actual system env).
        let mut builder = reqwest::Client::builder()
            .timeout(core::time::Duration::from_millis(self.timeout_ms))
            .hickory_dns(!self.use_system_resolver)
            .no_proxy();

        let request_url = match self.url.scheme_str() {
            // HTTP/HTTPS endpoints
            Some("http") | Some("https") => self.url.to_string(),

            // File dump endpoint (debugging) - uses platform-specific local transport
            Some("file") => {
                let output_path = libdd_types::decode_uri_path_in_authority(&self.url)
                    .context("Failed to decode file path from URI")?;
                let socket_or_pipe_path = dump_server::spawn_dump_server(output_path)?;

                // Configure the client to use the local socket/pipe
                #[cfg(unix)]
                {
                    builder = builder.unix_socket(socket_or_pipe_path);
                }
                #[cfg(windows)]
                {
                    builder = builder
                        .windows_named_pipe(socket_or_pipe_path.to_string_lossy().to_string());
                }

                "http://localhost/".to_string()
            }

            // Unix domain sockets
            #[cfg(unix)]
            Some("unix") => {
                use connector::uds::socket_path_from_uri;
                let socket_path = socket_path_from_uri(&self.url)?;
                builder = builder.unix_socket(socket_path);
                format!("http://localhost{}", self.url.path())
            }

            // Windows named pipes
            #[cfg(windows)]
            Some("windows") => {
                use connector::named_pipe::named_pipe_path_from_uri;
                let pipe_path = named_pipe_path_from_uri(&self.url)?;
                builder = builder.windows_named_pipe(pipe_path.to_string_lossy().to_string());
                format!("http://localhost{}", self.url.path())
            }

            // Unsupported schemes
            scheme => anyhow::bail!("Unsupported endpoint scheme: {:?}", scheme),
        };

        Ok((builder, request_url))
    }
}
