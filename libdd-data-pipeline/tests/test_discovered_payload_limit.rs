// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(
    feature = "external-observations",
    not(target_arch = "wasm32"),
    not(miri)
))]
use httpmock::{Method::POST, MockServer};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::TraceExporter;
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::span::{span_pool::PooledChunks, v04::SpanBytes};
#[test]
fn discovered_receiver_limit_is_exclusive_and_local_limit_is_inclusive() {
    use libdd_shared_runtime::{BlockingRuntime, SharedRuntime, Worker};
    use std::time::Duration;

    let runtime = ForkSafeRuntime::new().unwrap();
    // A v0.4 payload containing one empty chunk is exactly two MessagePack bytes.
    for (agent, local, fits) in [(3, 2, true), (2, 2, false), (1, 0, false)] {
        let server = MockServer::start();
        let _info = server.mock(|when, then| {
            when.path("/cap-fixture/info");
            then.status(200)
                .body(format!(r#"{{"config":{{"max_request_bytes":{agent}}}}}"#));
        });
        let (mut fetcher, _) =
            libdd_data_pipeline::agent_info::AgentInfoFetcher::<NativeCapabilities>::new(
                libdd_common::Endpoint::from_slice(&server.url("/cap-fixture/info")),
                Duration::from_secs(300),
            );
        runtime.block_on(fetcher.run()).unwrap();
        let sent = server.mock(|when, then| {
            when.method(POST)
                .path("/cap-fixture/v0.4/traces")
                .is_true(|req| req.body_vec() == [0x91, 0x90]);
            then.status(200);
        });
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder.set_url(&server.url("/cap-fixture/"));
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let chunks = PooledChunks::unpooled(vec![Vec::<SpanBytes>::new()]);
        let result = exporter.send_trace_chunks_batched(chunks, local, None);
        assert!(result.error.is_none());
        assert_eq!(result.oversized_chunks, usize::from(!fits));
        assert_eq!(result.payloads.len(), usize::from(fits));
        sent.assert_calls(usize::from(fits));
    }
}
