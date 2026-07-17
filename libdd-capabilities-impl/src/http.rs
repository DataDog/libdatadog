// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native HTTP client implementation backed by hyper.

mod native {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::{Arc, OnceLock};

    use libdd_capabilities::http::{HttpClientCapability, HttpError};
    use libdd_capabilities::maybe_send::MaybeSend;
    use libdd_common::connector::Connector;
    use libdd_common::http_common::{new_default_client, Body, GenericHttpClient};

    use http_body_util::BodyExt;

    #[derive(Clone)]
    pub struct NativeHttpClient {
        client: Arc<OnceLock<GenericHttpClient<Connector>>>,
    }

    impl std::fmt::Debug for NativeHttpClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NativeHttpClient")
                .field("initialized", &self.client.get().is_some())
                .finish()
        }
    }

    /// Write `body` as a newline-terminated record to the file referenced by `uri` (which must
    /// have a `file://` scheme), then return a synthetic 202 response.
    fn write_to_file_endpoint(
        uri: &http::Uri,
        body: bytes::Bytes,
    ) -> Result<http::Response<bytes::Bytes>, HttpError> {
        let path = libdd_common::decode_uri_path_in_authority(uri)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("invalid file:// URI: {e}")))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("opening {path:?}: {e}")))?;
        let mut record = body.to_vec();
        record.push(b'\n');
        file.write_all(&record)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("writing {path:?}: {e}")))?;

        http::Response::builder()
            .status(http::StatusCode::ACCEPTED)
            .body(bytes::Bytes::new())
            .map_err(|e| HttpError::Other(e.into()))
    }

    impl HttpClientCapability for NativeHttpClient {
        fn new_client() -> Self {
            Self {
                client: Arc::new(OnceLock::new()),
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn request(
            &self,
            req: http::Request<bytes::Bytes>,
        ) -> impl std::future::Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend
        {
            let client_lock = self.client.clone();
            async move {
                // ===== TEMP DEBUG PROBE (branch: ekump/telemetry-wasm-debug) ===============
                // Diagnosing the aarch64-only crashtracker `panic_hook_unknown_type` hang/OOM.
                // Log the request scheme/URI + which branch runs. By default, BAIL on any
                // non-file request instead of driving hyper, so the crashtracker receiver
                // returns quickly, the test completes, and the receiver's stderr (surfaced by
                // `validate_std_outputs` as "Unexpected stderr") reaches CI. Set
                // LIBDD_HTTP_PROBE_ALLOW_HYPER=1 to restore the real network path.
                let probe_scheme = req.uri().scheme_str().map(str::to_owned);
                let probe_uri = req.uri().to_string();
                eprintln!("[HTTP-PROBE] request scheme={probe_scheme:?} uri={probe_uri}");
                // ===========================================================================

                // file:// URIs short-circuit to the on-disk recorder used by tests.
                if req.uri().scheme_str() == Some("file") {
                    eprintln!("[HTTP-PROBE] -> file:// short-circuit (write_to_file_endpoint)");
                    let (parts, body) = req.into_parts();
                    let r = write_to_file_endpoint(&parts.uri, body);
                    eprintln!("[HTTP-PROBE] <- file write ok={}", r.is_ok());
                    return r;
                }

                // ===== TEMP DEBUG PROBE ====================================================
                if std::env::var_os("LIBDD_HTTP_PROBE_ALLOW_HYPER").is_none() {
                    eprintln!(
                        "[HTTP-PROBE] -> NON-file path (would build hyper new_default_client); \
                         bailing before hyper. scheme={probe_scheme:?} uri={probe_uri}"
                    );
                    return Err(HttpError::Other(anyhow::anyhow!(
                        "[HTTP-PROBE] non-file request bailed before hyper: \
                         scheme={probe_scheme:?} uri={probe_uri}"
                    )));
                }
                eprintln!("[HTTP-PROBE] -> NON-file path: driving hyper (ALLOW_HYPER set)");
                // ===========================================================================

                let client = client_lock.get_or_init(new_default_client).clone();
                let hyper_req = req.map(Body::from_bytes);

                let response = client
                    .request(hyper_req)
                    .await
                    .map_err(|e| HttpError::Network(e.into()))?;

                let (parts, body) = response.into_parts();
                let collected = body
                    .collect()
                    .await
                    .map_err(|e| HttpError::ResponseBody(e.into()))?
                    .to_bytes();

                Ok(http::Response::from_parts(parts, collected))
            }
        }
    }
}

pub use native::NativeHttpClient;
