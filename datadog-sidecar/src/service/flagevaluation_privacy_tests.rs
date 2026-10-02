// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Matching-build IPC regression tests. Sender and receiver ship together;
//! these do not claim compatibility between different bincode layouts.

use super::*;
use crate::service::sidecar_interface::{SidecarInterfaceChannel, SidecarInterfaceRequest};
use crate::service::{
    EvalError, FfeFlagEvaluationBatch, FfeFlagEvaluationEvent, FfeTelemetryContext,
    FlagEvalEventContext, FlagKey,
};
use httpmock::{Method::POST, MockServer};
use libdd_ffe::telemetry::exposures::{FfeExposure, FfeExposureBatch};
use libdd_ffe::telemetry::flagevaluation::{ContextTruncationReason, EVP_FLAGEVALUATION_PATH};
use libdd_ipc::codec::{decode, encode};
use std::io::Write;
use tracing::instrument::WithSubscriber;

fn event() -> FfeFlagEvaluationEvent {
    FfeFlagEvaluationEvent {
        timestamp: 1_760_000_000_000,
        flag: FlagKey {
            key: "protected".into(),
        },
        first_evaluation: 1_760_000_000_000,
        last_evaluation: 1_760_000_000_001,
        evaluation_count: 7,
        variant: None,
        allocation: None,
        targeting_rule: None,
        targeting_key: Some("jane.doe@datadoghq.com".into()),
        context: Some(FlagEvalEventContext {
            evaluation: Some(r#"{"email":"protected-context-canary"}"#.into()),
            dd: None,
        }),
        error: Some(EvalError {
            message: "private-error-canary".into(),
        }),
        runtime_default_used: false,
        observe_full_evaluation_data: false,
        is_degraded: false,
        field_omissions: Default::default(),
    }
}

fn request(events: Vec<FfeFlagEvaluationEvent>) -> SidecarInterfaceRequest {
    let ordinary = || SidecarAction::AddTelemetryMetricPoint(("ffe.healthy".into(), 3.0, vec![]));
    SidecarInterfaceRequest::EnqueueActions {
        instance_id: InstanceId::new("privacy", "runtime"),
        queue_id: QueueId::from(42),
        actions: vec![
            ordinary(),
            SidecarAction::FfeFlagEvaluationBatch(FfeFlagEvaluationBatch {
                context: FfeTelemetryContext {
                    service: "svc".into(),
                    env: "test".into(),
                    version: "1".into(),
                },
                flag_evaluations: events,
            }),
            ordinary(),
        ],
    }
}

#[test]
fn matching_ipc_preserves_rows_optional_fields_and_neighboring_actions() {
    for rows in [0, 1, 2] {
        for consent in [false, true] {
            for degraded in [false, true] {
                for key in [None, Some(""), Some("é-user")] {
                    for optional in [false, true] {
                        let mut row = event();
                        row.observe_full_evaluation_data = consent;
                        row.is_degraded = degraded;
                        row.targeting_key = key.map(str::to_owned);
                        if !optional {
                            row.context = None;
                            row.error = None;
                        }
                        row.field_omissions
                            .record_context(ContextTruncationReason::SnapshotError);
                        row.field_omissions.targeting_key_invalid = true;
                        let request = request(vec![row; rows]);
                        let decoded: SidecarInterfaceRequest = decode(&encode(&request)).unwrap();
                        assert_eq!(
                            serde_json::to_value(decoded).unwrap(),
                            serde_json::to_value(&request).unwrap()
                        );
                        let debug = format!("{request:?}");
                        for canary in ["é-user", "protected-context-canary", "private-error-canary"]
                        {
                            assert!(!debug.contains(canary));
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn matching_ipc_enforces_privacy_in_http_and_keeps_connection_healthy() {
    let http = MockServer::start_async().await;
    let output = http
        .mock_async(|when, then| {
            when.method(POST)
                .path(EVP_FLAGEVALUATION_PATH)
                .header("DD-EVP-ORIGIN", "dd-trace-php")
                .header("DD-EVP-ORIGIN-VERSION", "1.25.1")
                .body_includes(
                    "sha256_b4698f9b6d186781fa8dc59e533578fa2d8379a46b1cf6db85cda6aa9c99e51b",
                )
                .body_includes("consented-identity-canary")
                .body_includes("consented-context-canary")
                .body_includes("\"evaluation_count\":7")
                .body_includes("\"first_evaluation\":1760000000000")
                .body_includes("\"last_evaluation\":1760000000001")
                .body_includes("\"message\":\"GENERAL\"")
                .body_excludes("jane.doe@datadoghq.com")
                .body_excludes("protected-context-canary")
                .body_excludes("degraded-identity-canary")
                .body_excludes("degraded-context-canary")
                .body_excludes("private-error-canary")
                .body_excludes("runtime_default_used")
                .body_excludes("observe_full_evaluation_data")
                .body_excludes("is_degraded")
                .body_excludes("field_omissions");
            then.status(202);
        })
        .await;
    let server = SidecarServer::default();
    server.get_session("privacy").modify_trace_config(|cfg| {
        cfg.set_endpoint(Endpoint {
            url: http.url("/").parse().unwrap(),
            ..Endpoint::default()
        })
        .unwrap();
        cfg.language = "php".into();
        cfg.tracer_version = "1.25.1".into();
    });
    let logs = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let log_writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || log_writer.clone())
        .finish();
    let (local, client) = SeqpacketConn::socketpair().unwrap();
    let handler = Arc::new(ConnectionSidecarHandler::new(
        server,
        OwnedServerConn::new(local).unwrap(),
    ));
    let task = tokio::spawn(
        serve_sidecar_interface_connection(handler.clone()).with_subscriber(subscriber),
    );
    let mut consented = event();
    consented.flag.key = "consented".into();
    consented.observe_full_evaluation_data = true;
    consented.targeting_key = Some("consented-identity-canary".into());
    consented.context.as_mut().unwrap().evaluation =
        Some(r#"{"email":"consented-context-canary"}"#.into());
    let mut degraded = consented.clone();
    degraded.flag.key = "degraded".into();
    degraded.is_degraded = true;
    degraded.targeting_key = Some("degraded-identity-canary".into());
    degraded.context.as_mut().unwrap().evaluation =
        Some(r#"{"email":"degraded-context-canary"}"#.into());
    // Deliberately bypass producer normalization: the receiver must protect raw input.
    for rows in [vec![], vec![event()], vec![consented, degraded]] {
        client.try_send_raw(encode(&request(rows)), &[]).unwrap();
    }
    client
        .try_send_raw(
            encode(&SidecarInterfaceRequest::Flush {
                options: SidecarFlushOptions {
                    flag_evaluations: true,
                    ..SidecarFlushOptions::default()
                },
            }),
            &[],
        )
        .unwrap();
    client
        .try_send_raw(encode(&SidecarInterfaceRequest::Ping {}), &[])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while handler.submitted_payloads.load(Ordering::Relaxed) < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all requests, including the final ping, must complete");
    output.assert_calls_async(1).await;
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("IPC recv"));
    for canary in [
        "jane.doe@datadoghq.com",
        "protected-context-canary",
        "consented-identity-canary",
        "consented-context-canary",
        "degraded-identity-canary",
        "degraded-context-canary",
        "private-error-canary",
    ] {
        assert!(
            !logs.contains(canary),
            "sensitive evaluation data escaped into IPC logs"
        );
    }
    // Socketpair peer-close detection differs by platform; this test owns the server task.
    // Verify the payload and logs before explicitly stopping it, as in the signal-flush test.
    drop(client);
    task.abort();
    if let Err(error) = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
    {
        assert!(error.is_cancelled());
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
fn oversized_ipc_warning_does_not_log_evaluation_data() {
    let logs = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let log_writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || log_writer.clone())
        .finish();
    let (client, _peer) = SeqpacketConn::socketpair().unwrap();
    let mut channel = SidecarInterfaceChannel::new(client);
    let mut row = event();
    row.targeting_key =
        Some("oversized-identity-canary".repeat(libdd_ipc::max_message_size() / 24 + 1));
    let sent = tracing::subscriber::with_default(subscriber, || {
        channel.try_send_request(&request(vec![row]))
    });
    assert!(!sent);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("IPC message too large"));
    for canary in [
        "oversized-identity-canary",
        "protected-context-canary",
        "private-error-canary",
    ] {
        assert!(!logs.contains(canary));
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
async fn bounded_ffe_submission_preserves_http_privacy_and_survives_rejection() {
    use crate::service::blocking::{SidecarTransport, try_submit_ffe};
    use crate::service::ffe_submission::FfeSubmissionStatus;

    let http = MockServer::start_async().await;
    let output = http
        .mock_async(|when, then| {
            when.method(POST)
                .path(EVP_FLAGEVALUATION_PATH)
                .body_includes(
                    "sha256_b4698f9b6d186781fa8dc59e533578fa2d8379a46b1cf6db85cda6aa9c99e51b",
                )
                .body_includes("consented-identity-canary")
                .body_includes("consented-context-canary")
                .body_includes("\"evaluation_count\":1")
                .body_includes("\"message\":\"GENERAL\"")
                .body_excludes("jane.doe@datadoghq.com")
                .body_excludes("protected-context-canary")
                .body_excludes("private-error-canary")
                .body_excludes("oversize-canary")
                .body_excludes("observe_full_evaluation_data");
            then.status(202);
        })
        .await;
    let server = SidecarServer::default();
    server.get_session("privacy").modify_trace_config(|cfg| {
        cfg.set_endpoint(Endpoint {
            url: http.url("/").parse().unwrap(),
            ..Endpoint::default()
        })
        .unwrap();
        cfg.language = "php".into();
    });
    let logs = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let log_writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || log_writer.clone())
        .finish();
    let (local, client) = SeqpacketConn::socketpair().unwrap();
    let handler = Arc::new(ConnectionSidecarHandler::new(
        server,
        OwnedServerConn::new(local).unwrap(),
    ));
    let task = tokio::spawn(
        serve_sidecar_interface_connection(handler.clone()).with_subscriber(subscriber),
    );
    let transport = SidecarTransport::from(client);
    // Ordinary traffic remains on its existing API and connection.
    assert!(
        transport
            .inner
            .lock()
            .unwrap()
            .channel
            .try_send_enqueue_actions(
                InstanceId::new("privacy", "runtime"),
                QueueId::from(42),
                vec![SidecarAction::AddTelemetryMetricPoint((
                    "ffe.healthy".into(),
                    1.0,
                    vec![]
                ))],
            )
    );
    let single = |mut row: FfeFlagEvaluationEvent| {
        row.evaluation_count = 1;
        row.last_evaluation = row.first_evaluation;
        row.normalize();
        let mut req = request(vec![row]);
        if let SidecarInterfaceRequest::EnqueueActions { actions, .. } = &mut req {
            actions.retain(|action| matches!(action, SidecarAction::FfeFlagEvaluationBatch(_)));
        }
        req
    };
    assert_eq!(
        try_submit_ffe(&transport, || Ok(single(event()))),
        FfeSubmissionStatus::Accepted
    );
    let mut oversized = event();
    oversized.targeting_key =
        Some("oversize-canary".repeat(libdd_ipc::max_message_size() / 14 + 1));
    assert_eq!(
        try_submit_ffe(&transport, || Ok(single(oversized))),
        FfeSubmissionStatus::PayloadTooLarge
    );
    let mut consented = event();
    consented.flag.key = "consented".into();
    consented.observe_full_evaluation_data = true;
    consented.targeting_key = Some("consented-identity-canary".into());
    consented.context.as_mut().unwrap().evaluation =
        Some(r#"{"email":"consented-context-canary"}"#.into());
    assert_eq!(
        try_submit_ffe(&transport, || Ok(single(consented))),
        FfeSubmissionStatus::Accepted
    );
    for req in [
        SidecarInterfaceRequest::Flush {
            options: SidecarFlushOptions {
                flag_evaluations: true,
                ..Default::default()
            },
        },
        SidecarInterfaceRequest::Ping {},
    ] {
        assert!(
            transport
                .inner
                .lock()
                .unwrap()
                .channel
                .try_send_request(&req)
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while handler.submitted_payloads.load(Ordering::Relaxed) < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("healthy follow-up traffic must finish after rejection");
    output.assert_calls_async(1).await;
    let captured = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(captured.contains("IPC recv"));
    for canary in [
        "jane.doe@datadoghq.com",
        "protected-context-canary",
        "consented-identity-canary",
        "consented-context-canary",
        "private-error-canary",
        "oversize-canary",
    ] {
        assert!(!captured.contains(canary));
    }
    drop(transport);
    task.abort();
    if let Err(error) = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
    {
        assert!(error.is_cancelled());
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
fn oversized_ipc_warning_does_not_log_exposure_data() {
    let logs = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let log_writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || log_writer.clone())
        .finish();
    let (client, _peer) = SeqpacketConn::socketpair().unwrap();
    let mut channel = SidecarInterfaceChannel::new(client);
    let request = SidecarInterfaceRequest::EnqueueActions {
        instance_id: InstanceId::new("privacy", "runtime"),
        queue_id: QueueId::from(42),
        actions: vec![SidecarAction::FfeExposureBatch(FfeExposureBatch {
            context: FfeTelemetryContext {
                service: "svc".into(),
                env: "test".into(),
                version: "1".into(),
            },
            exposures: vec![FfeExposure {
                timestamp_ms: 123,
                flag_key: "flag".into(),
                subject_id: "oversized-identity-canary"
                    .repeat(libdd_ipc::max_message_size() / 24 + 1),
                subject_attributes_json: r#"{"email":"private-exposure-context-canary"}"#.into(),
                allocation_key: "allocation".into(),
                variant: "on".into(),
                serial_id: None,
            }],
        })],
    };
    let sent = tracing::subscriber::with_default(subscriber, || channel.try_send_request(&request));
    assert!(!sent);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("IPC message too large"));
    for canary in [
        "oversized-identity-canary",
        "private-exposure-context-canary",
    ] {
        assert!(!logs.contains(canary));
    }
}
