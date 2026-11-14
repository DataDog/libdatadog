// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Request-building extensions for [`libdd_common::Endpoint`].

#[cfg(feature = "reqwest")]
use anyhow::Context;
use libdd_common::Endpoint;

/// Extension trait holding the request-handling logic for an [`Endpoint`]: standard headers,
/// request builders and (with the `reqwest` feature) a configured reqwest client builder.
pub trait EndpointExt {
    /// Apply standard headers (user-agent, api-key, test-token) to an
    /// [`http::request::Builder`].
    fn set_standard_headers(
        &self,
        builder: http::request::Builder,
        user_agent: &str,
    ) -> http::request::Builder;

    /// Return a request builder with the following headers:
    /// - User agent
    /// - Api key
    fn to_request_builder(&self, user_agent: &str) -> anyhow::Result<http::request::Builder>;

    /// Creates a reqwest ClientBuilder configured for this endpoint.
    ///
    /// This method handles various endpoint schemes:
    /// - `http`/`https`: Standard HTTP(S) endpoints. HTTPS requests use the same TLS configuration
    ///   as the hyper connector, including the compromised-key distrust check.
    /// - `unix`: Unix domain sockets (Unix only)
    /// - `windows`: Windows named pipes (Windows only)
    /// - `file`: File dump endpoints for debugging (spawns a local server to capture requests)
    ///
    /// The default in-process resolver is used for DNS (fork-safe). To use the system DNS resolver
    /// instead (less fork-safe), set
    /// [`Endpoint::use_system_resolver`](libdd_common::Endpoint::use_system_resolver) to true via
    /// [`Endpoint::with_system_resolver`](libdd_common::Endpoint::with_system_resolver).
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
        builder
    }

    fn to_request_builder(&self, user_agent: &str) -> anyhow::Result<http::request::Builder> {
        let mut builder = http::Request::builder()
            .uri(self.url.clone())
            .header(http::header::USER_AGENT, user_agent);

        // Add optional endpoint headers (api-key, test-token)
        for (name, value) in self.get_optional_headers() {
            builder = builder.header(name, value);
        }

        Ok(builder)
    }

    #[cfg(feature = "reqwest")]
    fn to_reqwest_client_builder(&self) -> anyhow::Result<(reqwest::ClientBuilder, String)> {
        // Don't use proxies, as this calls `getenv` which is unsafe and not
        // just in theory. It can cause crashes with PHP where php-fpm's env
        // configuration will mutate the system environment (it doesn't pass
        // it as part of the SAPI env, it changes the actual system env).
        let mut builder = reqwest::Client::builder()
            .timeout(core::time::Duration::from_millis(self.timeout_ms))
            .hickory_dns(!self.use_system_resolver)
            .no_proxy();

        let request_url = match self.url.scheme_str() {
            // HTTPS endpoints: route reqwest through the same TLS configuration
            // as the hyper connector, so the compromised-key distrust check and
            // the selected crypto provider apply to both clients. Without this,
            // reqwest would build its own platform verifier, bypassing the
            // distrust wrapper.
            Some("https") => {
                #[cfg(feature = "tls-core")]
                {
                    builder = builder
                        .tls_backend_preconfigured(crate::connector::https::build_tls_config()?);
                }
                self.url.to_string()
            }
            // Plain-HTTP endpoints never negotiate TLS, so don't preconfigure a
            // TLS backend: building the configuration eagerly loads the system
            // CA roots, which fails in minimal container environments that have
            // no CA certificates installed.
            Some("http") => self.url.to_string(),

            // File dump endpoint (debugging) - uses platform-specific local transport
            Some("file") => {
                let output_path = libdd_common::decode_uri_path_in_authority(&self.url)
                    .context("Failed to decode file path from URI")?;
                let socket_or_pipe_path = crate::dump_server::spawn_dump_server(output_path)?;

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
                use crate::connector::uds::socket_path_from_uri;
                let socket_path = socket_path_from_uri(&self.url)?;
                builder = builder.unix_socket(socket_path);
                format!("http://localhost{}", self.url.path())
            }

            // Windows named pipes
            #[cfg(windows)]
            Some("windows") => {
                use crate::connector::named_pipe::named_pipe_path_from_uri;
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
