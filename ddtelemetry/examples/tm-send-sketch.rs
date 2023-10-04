use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use ddcommon::Endpoint;
use ddtelemetry::{
    build_host,
    config::Config,
    data::{self, metrics::Sketch, Application, Telemetry},
    worker::http_client::request_builder,
};
use http::{header::CONTENT_TYPE, Uri};

fn seq_id() -> u64 {
    static SEQ_ID: AtomicU64 = AtomicU64::new(1);
    SEQ_ID.fetch_add(1, Ordering::SeqCst)
}

fn build_request<'a>(
    application: &'a data::Application,
    host: &'a data::Host,
    payload: &'a data::Payload,
) -> data::Telemetry<'a> {
    data::Telemetry {
        api_version: data::ApiVersion::V1,
        tracer_time: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        runtime_id: "runtime_id",
        seq_id: seq_id(),
        application,
        host,
        payload,
    }
}

pub async fn push_telemetry(config: &Config, telemetry: &Telemetry<'_>) -> anyhow::Result<()> {
    let client = ddtelemetry::worker::http_client::from_config(config);
    let req = request_builder(config)?
        .method(http::Method::POST)
        .header(CONTENT_TYPE, ddcommon::header::APPLICATION_JSON)
        .header("dd-telemetry-debug-enabled", "true")
        .body(serde_json::to_string(telemetry)?.into())?;

    let resp = client.request(req).await?;

    if !resp.status().is_success() {
        Err(anyhow::Error::msg(format!(
            "Telemetry error: response status: {}",
            resp.status()
        )))
    } else {
        Ok(())
    }
}

fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main())
}

async fn async_main() {
    let app = Application {
        service_name: "sketch_test".to_owned(),
        service_version: None,
        env: None,
        language_name: String::from("nodejs"),
        language_version: String::from("0.0.0"),
        tracer_version: String::from("n/a"),
        runtime_name: None,
        runtime_version: None,
        runtime_patches: None,
    };
    let host = build_host();

    let mut sketch = datadog_ddsketch::DDSketch::default();
    sketch.add(0.0).unwrap();
    sketch.add(1.0).unwrap();
    sketch.add(2.0).unwrap();

    let payload = data::Payload::Sketches(data::Sketches {
        series: vec![Sketch {
            namespace: data::metrics::MetricNamespace::General,
            tags: Vec::new(),
            common: true,
            metric: "test_sketch_distribution".to_owned(),
            sketch: data::metrics::SerializedSketch::B64 {
                sketch_b64: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    sketch.encode_to_vec(),
                ),
            },
            _type: data::metrics::MetricType::Distribution,
            interval: 10,
        }],
    });

    let req = build_request(&app, &host, &payload);

    let config = Config::get().clone();
    push_telemetry(&config, &req).await.unwrap();
}
