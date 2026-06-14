// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) flag evaluation
//! batches to the Datadog Agent's EVP proxy.
//!
//! Protocol: `POST /evp_proxy/v2/api/v2/flagevaluations` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. Fire-and-forget: non-2xx
//! responses are logged at `warn`, network errors at `debug`, and dropped
//! (matches dd-trace-go behaviour). No agent capability gate.

use crate::service::FfeFlagEvaluationBatch;
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

/// EVP proxy path for FFE flag evaluation intake.
pub(crate) const EVP_FLAGEVALUATIONS_PATH: &str = "/evp_proxy/v2/api/v2/flagevaluations";

/// EVP subdomain that routes requests to event-platform intake.
pub(crate) const EVP_SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";
pub(crate) const EVP_SUBDOMAIN_VALUE: &str = "event-platform-intake";

const USER_AGENT: &str = concat!("ddtrace-sidecar/", env!("CARGO_PKG_VERSION"));

/// Build the FFE flagevaluation endpoint from a session's agent base endpoint.
/// Overrides only the path (`/evp_proxy/v2/api/v2/flagevaluations`), preserving
/// scheme, authority, timeout, and test_token.
/// Returns `None` for agentless mode because EVP proxy routing is agent-only.
pub(crate) fn flagevaluation_endpoint(base: &Endpoint) -> Option<Endpoint> {
    if base.api_key.is_some() {
        return None;
    }

    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(EVP_FLAGEVALUATIONS_PATH));
    let url = http::Uri::from_parts(parts).ok()?;
    Some(Endpoint {
        url,
        ..base.clone()
    })
}

/// POST a structured FFE flag evaluation batch to the agent EVP proxy.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
pub(crate) async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
) {
    let payload = match build_payload(&batch) {
        Ok(p) => p,
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: failed to encode batch payload: {e:?}");
            return;
        }
    };
    send_payload(client, endpoint, payload).await;
}

/// Build the EVP POST body from a batch.
///
/// The flagevaluation types are serialized over the sidecar's **bincode** IPC
/// wire, which is non-self-describing: a field omitted by `skip_serializing_if`
/// would misalign the derived `Deserialize` and cause the sidecar to drop the
/// whole batch. The types therefore carry **no** `skip_serializing_if` and emit
/// every field (optional ones as `null`/`false`/`""`). The flageval-worker EVP
/// schema, however, rejects those null/empty placeholders (especially for
/// degraded-tier events), so this helper strips them here, on the outbound POST
/// only — reproducing the old skip-serialization semantics without breaking the
/// bincode wire.
///
/// Two transforms happen, in order, on each `flagEvaluations` element:
///   1. `context.evaluation` is carried as a JSON-object **string** (bincode
///      cannot encode `serde_json::Value`); it is re-expanded back into a JSON
///      **object** in place. An unparseable string drops the field gracefully
///      (never panics), matching the best-effort telemetry contract.
///   2. The whole value is recursively cleaned (`strip_placeholders`) so the
///      POST contains no `null`, `false`, empty-string, empty-object, or
///      empty-array placeholder entries. Numeric zeros (timestamps/counts) are
///      preserved — they are real data.
fn build_payload(batch: &FfeFlagEvaluationBatch) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(batch)?;

    if let Some(events) = value
        .get_mut("flagEvaluations")
        .and_then(serde_json::Value::as_array_mut)
    {
        for event in events {
            let Some(context) = event.get_mut("context") else {
                continue;
            };
            let Some(evaluation) = context.get_mut("evaluation") else {
                continue;
            };
            if let Some(s) = evaluation.as_str() {
                match serde_json::from_str::<serde_json::Value>(s) {
                    // Re-expand the JSON-object string into an object in place.
                    Ok(parsed) => *evaluation = parsed,
                    // Unparseable string → drop the field gracefully (never panic).
                    Err(_) => {
                        if let Some(obj) = context.as_object_mut() {
                            obj.remove("evaluation");
                        }
                    }
                }
            }
        }
    }

    // Strip null/empty placeholders so the EVP POST matches the flageval-worker
    // schema (which rejects null placeholders) — see the function doc comment.
    strip_placeholders(&mut value);

    serde_json::to_string(&value)
}

/// Recursively remove placeholder entries from a JSON value so the EVP POST
/// carries no null/empty fields, reproducing the old `skip_serializing_if`
/// semantics on the outbound wire only.
///
/// An object entry (or array element) is dropped when its value, after the
/// children have themselves been cleaned (bottom-up), is one of:
///   - `null`            (was `Option::is_none`)
///   - `false`           (was the `runtime_default_used` bool skip)
///   - `""`              (was `String::is_empty`, e.g. `ContextDD::service`)
///   - `{}`              (an object that became empty after cleaning, e.g. a
///                        `context.dd` whose only field `service` was stripped)
///   - `[]`              (an array that became empty after cleaning)
///
/// Numeric values (including `0`) are NEVER removed — timestamps and counts are
/// real data. Non-zero bools (`true`) and non-empty strings/collections are
/// kept.
fn strip_placeholders(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Clean children first (bottom-up), then drop entries that are now
            // placeholders, so a container emptied by cleaning is itself removed.
            for child in map.values_mut() {
                strip_placeholders(child);
            }
            map.retain(|_, v| !is_placeholder(v));
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_placeholders(item);
            }
            items.retain(|v| !is_placeholder(v));
        }
        _ => {}
    }
}

/// Whether a (already-cleaned) JSON value is an empty/null placeholder that
/// should be dropped from the POST. Numeric zeros are NOT placeholders.
fn is_placeholder(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => !b,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        // Numbers (incl. 0) are real data — never placeholders.
        serde_json::Value::Number(_) => false,
    }
}

async fn send_payload<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    payload: String,
) {
    let builder = match endpoint.to_request_builder(USER_AGENT) {
        Ok(b) => b,
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: failed to build request: {e:?}");
            return;
        }
    };

    let req = match builder
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
        .body(Bytes::from(payload))
    {
        Ok(r) => r,
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: failed to construct request body: {e:?}");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms);
    let response = tokio::select! {
        biased;
        result = client.request(req) => result,
        _ = client.sleep(timeout) => {
            debug!("ffe_flagevaluation_flusher: request timed out after {timeout:?}");
            return;
        }
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body_preview = truncate(resp.body().as_ref(), 256);
                warn!("ffe_flagevaluation_flusher: non-2xx response {status}: {body_preview}");
            } else {
                debug!("ffe_flagevaluation_flusher: sent flag evaluation batch, status={status}");
            }
        }
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: request failed: {e:?}");
        }
    }
}

fn truncate(bytes: &[u8], cap: usize) -> String {
    let take = bytes.len().min(cap);
    String::from_utf8_lossy(&bytes[..take]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
    use datadog_ffe::telemetry::flagevaluation::{
        AllocationKey, ContextDD, EvalError, FfeFlagEvaluationEvent, FlagEvalEventContext, FlagKey,
        VariantKey,
    };
    use httpmock::MockServer;
    use libdd_capabilities::{HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use std::collections::BTreeMap;
    use std::future;

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

    fn eval_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 5,
            variant: None,
            allocation: None,
            targeting_key: None,
            // `evaluation` is carried as a JSON-object STRING on the wire (bincode
            // can't carry serde_json::Value); the flusher re-expands it to an object.
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("country".to_owned(), serde_json::json!("US"));
                        m
                    })
                    .unwrap(),
                ),
                dd: None,
            }),
            error: None,
            runtime_default_used: false,
        }
    }

    fn batch() -> FfeFlagEvaluationBatch {
        FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![eval_event()],
        }
    }

    // A full-tier event with every optional field populated.
    fn full_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 42,
            variant: Some(VariantKey {
                key: "on".to_owned(),
            }),
            allocation: Some(AllocationKey {
                key: "alloc-a".to_owned(),
            }),
            targeting_key: Some("user-123".to_owned()),
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("plan".to_owned(), serde_json::json!("premium"));
                        m
                    })
                    .unwrap(),
                ),
                dd: Some(ContextDD {
                    service: "frontend".to_owned(),
                }),
            }),
            error: Some(EvalError {
                message: "boom".to_owned(),
            }),
            runtime_default_used: true,
        }
    }

    // A degraded-tier event: all optional fields are None/false. On the bincode
    // wire these serialize as null/false placeholders; build_payload must strip
    // them so the flageval-worker schema sees no null placeholders.
    fn degraded_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "flag-b".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 7,
            variant: None,
            allocation: None,
            targeting_key: None,
            context: None,
            error: None,
            runtime_default_used: false,
        }
    }

    // Test: a degraded-tier event (all optional fields None/false) serializes to
    // a POST object with NO null/empty placeholder keys. Required numeric fields
    // (timestamps, counts — including any zeros) are preserved.
    #[test]
    fn build_payload_strips_degraded_tier_placeholders() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![degraded_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let ev = &v["flagEvaluations"][0];

        // Required fields present.
        assert_eq!(ev["flag"]["key"], "flag-b");
        assert_eq!(ev["evaluation_count"], 7);
        assert!(ev["first_evaluation"].is_number());
        assert!(ev["last_evaluation"].is_number());
        assert!(ev["timestamp"].is_number());

        // No null/empty placeholder keys.
        assert!(ev.get("variant").is_none(), "variant must be stripped");
        assert!(
            ev.get("allocation").is_none(),
            "allocation must be stripped"
        );
        assert!(
            ev.get("targeting_key").is_none(),
            "targeting_key must be stripped"
        );
        assert!(ev.get("context").is_none(), "context must be stripped");
        assert!(ev.get("error").is_none(), "error must be stripped");
        assert!(
            ev.get("runtime_default_used").is_none(),
            "runtime_default_used=false must be stripped"
        );
    }

    // Test: a full-tier event keeps all populated optional fields, with
    // context.evaluation expanded to an OBJECT and context.dd preserved.
    #[test]
    fn build_payload_keeps_full_tier_fields() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let ev = &v["flagEvaluations"][0];

        assert_eq!(ev["variant"]["key"], "on", "variant must be kept");
        assert_eq!(
            ev["allocation"]["key"], "alloc-a",
            "allocation must be kept"
        );
        assert_eq!(
            ev["targeting_key"], "user-123",
            "targeting_key must be kept"
        );
        assert_eq!(ev["error"]["message"], "boom", "error must be kept");
        assert_eq!(
            ev["runtime_default_used"], true,
            "runtime_default_used=true must be kept"
        );

        // context.evaluation is expanded to an OBJECT (not a string), and dd is kept.
        let ctx = &ev["context"];
        assert!(
            ctx["evaluation"].is_object(),
            "context.evaluation must be an object: {}",
            ctx["evaluation"]
        );
        assert_eq!(ctx["evaluation"]["plan"], "premium");
        assert_eq!(
            ctx["dd"]["service"], "frontend",
            "context.dd.service must be kept"
        );
    }

    // Test: a context whose only dd field (service) is empty collapses entirely —
    // empty service is stripped, the now-empty dd object is stripped, and if
    // evaluation is also absent the whole context object is removed.
    #[test]
    fn build_payload_collapses_empty_nested_context() {
        let mut ev = degraded_event();
        ev.context = Some(FlagEvalEventContext {
            evaluation: None,
            dd: Some(ContextDD {
                service: String::new(),
            }),
        });
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![ev],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0].get("context").is_none(),
            "a context that becomes empty after cleaning must be removed entirely"
        );
    }

    // Test: build_payload re-expands the wire-side JSON-object STRING in
    // `context.evaluation` into a JSON OBJECT in the POST body (EVP schema shape).
    #[test]
    fn build_payload_expands_evaluation_string_into_object() {
        let payload = build_payload(&batch()).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let evaluation = &v["flagEvaluations"][0]["context"]["evaluation"];
        assert!(
            evaluation.is_object(),
            "context.evaluation must be a JSON object in the POST body, not a string: {evaluation}"
        );
        assert_eq!(
            evaluation["country"], "US",
            "the expanded object must preserve the original key/value"
        );
        assert!(
            !evaluation.is_string(),
            "context.evaluation must not remain a quoted string"
        );
    }

    // Test: an unparseable `evaluation` string is dropped gracefully (no panic,
    // no malformed body) rather than left as a raw string in the POST body.
    #[test]
    fn build_payload_drops_unparseable_evaluation_gracefully() {
        let mut batch = batch();
        batch.flag_evaluations[0].context = Some(FlagEvalEventContext {
            evaluation: Some("this is not json".to_owned()),
            dd: None,
        });

        let payload = build_payload(&batch).expect("build_payload must not fail on bad input");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0]["context"]
                .get("evaluation")
                .is_none(),
            "unparseable evaluation must be dropped from the body"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATIONS_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();

        send_batch(&client, &ep, batch()).await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATIONS_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        send_batch(&client, &ep, batch()).await;
        // Test passes if no panic occurs.
    }

    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(&HangingCapabilities, &ep, batch()).await;
        // Test passes if function returns before the pending HTTP future resolves.
    }

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_endpoint(&base).unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), EVP_FLAGEVALUATIONS_PATH);
    }

    #[test]
    fn endpoint_rejects_agentless() {
        let base = Endpoint {
            url: "https://trace.agent.datadoghq.com/v0.4/traces"
                .parse()
                .unwrap(),
            api_key: Some("api-key".into()),
            ..Endpoint::default()
        };
        assert!(flagevaluation_endpoint(&base).is_none());
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
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
