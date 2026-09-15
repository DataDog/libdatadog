// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) exposure events
//! through the session's shared EVP transport.
//!
//! Protocol matches dd-trace-go / dd-trace-rb / dd-trace-py / dd-trace-js /
//! dd-trace-dotnet: `POST /evp_proxy/v2/api/v2/exposures` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. No agent capability gate.

use crate::service::ffe_evp_proxy::FfeEvpTransport;
#[cfg(test)]
use crate::service::ffe_evp_proxy::{EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE};
use crate::service::FfeExposureBatch;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_ffe::telemetry::exposures::encode_exposure_batch_in_scope;
pub(crate) use libdd_ffe::telemetry::exposures::ExposureDeduplicator;
use tracing::debug;

/// EVP proxy path for FFE exposure intake.
#[cfg(test)]
pub(crate) const EVP_EXPOSURES_PATH: &str = "/evp_proxy/v2/api/v2/exposures";
const EXPOSURES_INTAKE_PATH: &str = "/api/v2/exposures";

const LOG_PREFIX: &str = "ffe_exposures_flusher";

/// POST a structured FFE exposure batch through the session's selected EVP route.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
pub(crate) async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    transport: &FfeEvpTransport,
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) {
    let deduplication_scope = transport.deduplication_scope();
    let payload = match encode_exposure_batch_in_scope(deduplicator, &deduplication_scope, batch) {
        Ok(Some(payload)) => payload,
        Ok(None) => return,
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to encode exposure payload: {e:?}");
            return;
        }
    };
    transport
        .send_payload(
            client,
            EXPOSURES_INTAKE_PATH,
            payload,
            LOG_PREFIX,
            "exposure batch",
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeExposure, FfeTelemetryContext};
    use httpmock::MockServer;
    use libdd_capabilities::{Bytes, HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_common::Endpoint;
    use std::future;
    use std::time::Duration;

    fn endpoint_for(server: &MockServer) -> Endpoint {
        Endpoint {
            url: server.url("/").parse().unwrap(),
            ..Endpoint::default()
        }
    }

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn exposure(subject_id: &str, allocation_key: &str, variant: &str) -> FfeExposure {
        FfeExposure {
            timestamp_ms: 123,
            flag_key: "flag".to_owned(),
            subject_id: subject_id.to_owned(),
            subject_attributes_json: r#"{"tier":"premium"}"#.to_owned(),
            allocation_key: allocation_key.to_owned(),
            variant: variant.to_owned(),
            serial_id: None,
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_EXPOSURES_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let transport = FfeEvpTransport::agent_only(base);
        let client = NativeCapabilities::new_client();

        send_batch(
            &client,
            &transport,
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path(EVP_EXPOSURES_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let base = endpoint_for(&server);
        let transport = FfeEvpTransport::agent_only(base);
        let client = NativeCapabilities::new_client();
        send_batch(
            &client,
            &transport,
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;
    }

    #[cfg_attr(miri, ignore)] // tokio executor park/wake overhead is prohibitively slow under Miri
    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let endpoint = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(
            &HangingCapabilities,
            &FfeEvpTransport::agent_only(endpoint),
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
            Self
        }

        fn new_without_connection_pooling() -> Self {
            Self
        }

        fn request(
            &self,
            _req: http::Request<Bytes>,
        ) -> impl future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
        {
            future::pending()
        }
    }

    impl SleepCapability for HangingCapabilities {
        fn new() -> Self {
            Self
        }

        async fn sleep(&self, _duration: Duration) {}
    }
}
