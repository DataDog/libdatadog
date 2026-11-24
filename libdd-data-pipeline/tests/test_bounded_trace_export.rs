// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(
    feature = "external-observations",
    not(target_arch = "wasm32"),
    not(miri)
))]

use httpmock::{Method::POST, MockServer};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::{
    TraceExporter, agent_response::AgentResponse, error::TraceExporterError,
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::{
    span::{span_pool::PooledChunks, v04::SpanBytes},
    test_utils::create_test_no_alloc_span,
};

fn chunks(large_middle: bool) -> PooledChunks<'static, libdd_trace_utils::span::BytesData> {
    PooledChunks::unpooled(
        (1..=3)
            .map(|id| {
                let mut span: SpanBytes = create_test_no_alloc_span(id, id, 0, 1, true);
                span.resource = BytesString::from(format!(
                    "{}{}",
                    if id == 2 { "bad" } else { "good" },
                    "x".repeat(if id == 2 && large_middle { 2048 } else { 600 })
                ));
                vec![span]
            })
            .collect(),
    )
}

#[test]
fn bounded_sends_preserve_partial_success_and_do_not_retry_413() {
    for status in [413, 503] {
        let server = MockServer::start();
        let good = server.mock(|when, then| {
            when.method(POST)
                .path("/v0.4/traces")
                .body_excludes("bad")
                .is_true(|request| request.body_vec().len() <= 1024);
            then.status(200)
                .body(r#"{"rate_by_service":{"service:,env:":0.5}}"#);
        });
        let rejected = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces").body_includes("bad");
            then.status(status).body("rejected");
        });
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder.set_url(&server.url("/"));
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let batch = exporter.send_trace_chunks_batched(chunks(false), 1024, None);
        assert!(batch.error.is_none());
        assert_eq!(batch.payloads.len(), 3);
        assert!(
            matches!(&batch.payloads[2].response, Ok(AgentResponse::Changed { body }) if body.contains("rate_by_service"))
        );
        assert!(
            matches!(&batch.payloads[1].response, Err(TraceExporterError::Request(error)) if error.status() == status)
        );
        assert!(batch.payloads[2].response.is_ok());
        assert_eq!(batch.unattempted_chunks, 0);
        assert_eq!(batch.in_flight_chunks, 0);
        let attempts = u64::try_from(rejected.calls()).unwrap();
        if status == 413 {
            assert_eq!(attempts, 1);
        } else {
            assert!(attempts > 1);
        }
        for (payload, code) in batch.payloads.iter().zip([200, status, 200]) {
            assert_eq!(payload.chunks, 1);
            assert_eq!(
                payload.observations.requests_count,
                if code == 200 { 1 } else { attempts }
            );
            assert_eq!(payload.observations.responses_count, 1);
            assert_eq!(payload.observations.status_code, code);
            assert_eq!(payload.observations.chunks_sent, u64::from(code == 200));
            assert_eq!(
                payload.observations.chunks_dropped_send_failure,
                u64::from(code != 200)
            );
        }
        good.assert_calls(2);
    }
}

#[test]
fn oversized_chunks_do_not_discard_their_neighbours() {
    let server = MockServer::start();
    let accepted = server.mock(|when, then| {
        when.method(POST)
            .path("/v0.4/traces")
            .body_excludes("bad")
            .is_true(|request| request.body_vec().len() <= 1024);
        then.status(200).body("{}");
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&server.url("/"));
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let batch = exporter.send_trace_chunks_batched(chunks(true), 1024, None);
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 2);
    assert!(
        batch
            .payloads
            .iter()
            .all(|payload| payload.response.is_ok())
    );
    assert_eq!((batch.oversized_chunks, batch.oversized_spans), (1, 1));
    assert_eq!((batch.unattempted_chunks, batch.in_flight_chunks), (0, 0));
    accepted.assert_calls(2);
}

#[test]
fn retains_only_the_latest_sampling_feedback() {
    let server = MockServer::start();
    let body = format!(
        r#"{{"rate_by_service":{{"service:{},env:":0.5}}}}"#,
        "x".repeat(1024 * 1024)
    );
    let accepted = server.mock(|when, then| {
        when.method(POST).path("/v0.4/traces");
        then.status(200).body(&body);
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&server.url("/"));
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let chunks = (1..=64)
        .map(|id| {
            vec![SpanBytes {
                trace_id: id,
                span_id: u64::try_from(id).unwrap(),
                ..Default::default()
            }]
        })
        .collect();
    let batch = exporter.send_trace_chunks_batched(PooledChunks::unpooled(chunks), 256, None);
    assert!(batch.error.is_none());
    assert!(batch.payloads.len() > 1);
    assert!(batch.payloads.iter().all(|p| p.response.is_ok()));
    let retained: usize = batch
        .payloads
        .iter()
        .map(|p| match &p.response {
            Ok(AgentResponse::Changed { body }) => body.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(retained, body.len());
    accepted.assert_calls(batch.payloads.len());
}

#[test]
fn malformed_later_feedback_does_not_displace_valid_rates() {
    for later in ["[{}]", r#"{"rate_by_service":null}"#, "{}", "not json"] {
        let server = MockServer::start();
        let rates = r#"{"rate_by_service":{"service:,env:":0.5}}"#;
        let first = server.mock(|when, then| {
            when.method(POST)
                .path("/v0.4/traces")
                .body_includes("first");
            then.status(200).body(rates);
        });
        let second = server.mock(|when, then| {
            when.method(POST)
                .path("/v0.4/traces")
                .body_includes("second");
            then.status(200).body(later);
        });
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder.set_url(&server.url("/"));
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let chunks = ["first", "second"]
            .into_iter()
            .map(|name| {
                vec![SpanBytes {
                    resource: BytesString::from(format!("{name}{}", "x".repeat(600))),
                    ..Default::default()
                }]
            })
            .collect();
        let result = exporter.send_trace_chunks_batched(PooledChunks::unpooled(chunks), 1024, None);
        assert!(result.error.is_none());
        assert_eq!(result.payloads.len(), 2);
        assert!(
            matches!(&result.payloads[0].response, Ok(AgentResponse::Changed { body }) if body == rates)
        );
        assert!(matches!(
            &result.payloads[1].response,
            Ok(AgentResponse::Unchanged)
        ));
        first.assert_calls(1);
        second.assert_calls(1);
    }
}

#[test]
fn v1_rejection_replans_only_unsent_neighbours_as_v04() {
    assert_v1_rejection(0);
}

#[test]
fn v1_rejection_does_not_replay_earlier_successes() {
    assert_v1_rejection(1);
}

fn assert_v1_rejection(rejected_index: usize) {
    use libdd_shared_runtime::{BlockingRuntime, SharedRuntime, Worker};
    let names = ["first", "second", "third"];
    let server = MockServer::start();
    let _info = server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/info");
        then.status(200).body(r#"{"endpoints":["/v1.0/traces"]}"#);
    });
    let runtime = ForkSafeRuntime::new().unwrap();
    runtime
        .block_on(async {
            let (mut fetcher, _) =
                libdd_data_pipeline::agent_info::AgentInfoFetcher::<NativeCapabilities>::new(
                    libdd_common::Endpoint::from_slice(&server.url("/info")),
                    std::time::Duration::from_secs(300),
                );
            fetcher.run().await;
        })
        .unwrap();
    let rejected = server.mock(|when, then| {
        when.method(POST)
            .path("/v1.0/traces")
            .body_includes(names[rejected_index]);
        then.status(404);
    });
    let initial = server.mock(|when, then| {
        when.method(POST)
            .path("/v1.0/traces")
            .body_excludes(names[rejected_index]);
        then.status(200).body(r#"{"rate_by_service":{}}"#);
    });
    let accepted = server.mock(|when, then| {
        when.method(POST)
            .path("/v0.4/traces")
            .body_excludes(names[0])
            .body_excludes(names[rejected_index]);
        then.status(200).body(r#"{"rate_by_service":{}}"#);
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&server.url("/")).enable_v1_protocol();
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let traces = names
        .into_iter()
        .map(|name| {
            vec![SpanBytes {
                resource: BytesString::from(format!("{name}{}", "x".repeat(600))),
                ..Default::default()
            }]
        })
        .collect();
    let batch = exporter.send_trace_chunks_batched(PooledChunks::unpooled(traces), 1024, None);
    assert!(batch.error.is_none());
    assert_eq!(batch.payloads.len(), 3);
    for (index, payload) in batch.payloads.iter().enumerate() {
        assert_eq!(payload.response.is_err(), index == rejected_index);
        assert_eq!(payload.chunks, 1);
    }
    assert_eq!((batch.in_flight_chunks, batch.unattempted_chunks), (0, 0));
    rejected.assert_calls(6);
    initial.assert_calls(rejected_index);
    accepted.assert_calls(2 - rejected_index);
}
