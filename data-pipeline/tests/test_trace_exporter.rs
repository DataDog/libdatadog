#[cfg(test)]
mod tracing_integration_tests {
    use data_pipeline::trace_exporter::TraceExporter;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use tokio::runtime::Runtime;
    use tokio::task;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let handle = tokio::runtime::Handle::current();

        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;
        let test_agent_url = test_agent
            .get_uri_for_endpoint("v0.4/traces", Some("compare_v04_trace_snapshot_test"))
            .await;

        let res = task::spawn_blocking(move || {
            let trace_exporter = TraceExporter::builder()
                .set_url(test_agent_url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .build()
                .expect("Unable to build TraceExporter");

            trace_exporter
                .shutdown(None)
                .expect("Failed to shutdown TraceExporter");
        })
        .await;
    }
}
