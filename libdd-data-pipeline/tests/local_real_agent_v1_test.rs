// Local-only helper: sends native V1 input through the full `TraceExporter` pipeline to a real,
// already-running Datadog Agent (not the Docker `dd-apm-test-agent`).
//
// Not pushed / not part of CI. Requires a real Agent listening on `DD_TRACE_AGENT_URL`
// (defaults to http://localhost:8126). Marked `#[ignore]` so normal `cargo test` / `cargo
// nextest run` invocations skip it.
//
// Run with:
//   cargo test -p libdd-data-pipeline --test local_real_agent_v1_test -- --ignored --nocapture

use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::{
    TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
use libdd_trace_utils::span::v1::{
    AttributeValue, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes, TracerPayloadBytes,
};
use libdd_trace_utils::span::vec_map::VecMap;

fn bs(s: &str) -> BytesString {
    BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
}

fn build_native_v1_payload() -> Vec<u8> {
    let mut attrs = VecMap::new();
    attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
    attrs.insert(bs("http.status_code"), AttributeValue::Int(200));

    let span = V1SpanBytes {
        service: bs("local-real-agent-test"),
        name: bs("local_real_agent_v1_test"),
        resource: bs("/api/local-test"),
        r#type: bs("web"),
        span_id: 1,
        parent_id: 0,
        start: 1_000_000,
        duration: 5_000,
        span_kind: SpanKind::Server,
        env: bs("local-test-env"),
        attributes: attrs,
        ..Default::default()
    };

    let chunk = TraceChunkBytes {
        trace_id: {
            let mut id = [0u8; 16];
            id[15] = 0x01;
            id
        },
        priority: Some(1),
        sampling_mechanism: Some(4),
        spans: vec![span],
        ..Default::default()
    };

    let payload = TracerPayloadBytes {
        language_name: bs("test-lang"),
        language_version: bs("2.0"),
        tracer_version: bs("1.0"),
        env: bs("local-test-env"),
        chunks: vec![chunk],
        ..Default::default()
    };

    to_vec_from_v1(&payload)
}

#[ignore]
#[test]
fn send_native_v1_to_real_agent() {
    let agent_url =
        std::env::var("DD_TRACE_AGENT_URL").unwrap_or_else(|_| "http://localhost:8126".into());

    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&agent_url)
        .set_language("test-lang")
        .set_language_version("2.0")
        .set_language_interpreter_vendor("vendor")
        .set_language_interpreter("interpreter")
        .set_tracer_version("1.0")
        .set_env("local-test-env")
        .set_service("local-real-agent-test")
        .set_input_format(TraceExporterInputFormat::V1)
        .set_output_format(TraceExporterOutputFormat::V1);

    let trace_exporter = builder
        .build::<NativeCapabilities>()
        .expect("Unable to build TraceExporter");

    // V1 is gated by /info negotiation (fail-closed). Wait until the background fetcher has
    // populated agent_info so the send below promotes v1_active=true before routing, instead of
    // silently falling back to v0.4.
    let start = std::time::Instant::now();
    while libdd_data_pipeline::agent_info::get_agent_info().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            panic!("timeout waiting for /info from {agent_url} — is a real Agent running there?");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let data = build_native_v1_payload();
    let response = trace_exporter.send(data.as_ref());
    println!("agent response: {response:?}");
    assert!(response.is_ok(), "send failed: {:?}", response.err());
}
