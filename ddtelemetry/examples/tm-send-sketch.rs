// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use ddcommon::Endpoint;
use ddtelemetry::{
    build_host,
    config::Config,
    data::{self, metrics::Distribution, Application, Telemetry},
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
        tracer_time: SystemTime::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_secs()),
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
    for i in 0..1000 {
        for j in 0..1000 {
            sketch.add((i + j) as f64 / 1000.0).unwrap();
        }
    }

    let payload = data::Payload::Sketches(data::Distributions {
        series: vec![Distribution {
            namespace: data::metrics::MetricNamespace::Telemetry,
            tags: Vec::new(),
            common: true,
            metric: "telemetry_api.ms".to_owned(),
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

    let mut config = Config::get().clone();
    config.endpoint = Some(Endpoint {
        url: Uri::from_static(
            "https://instrumentation-telemetry-intake.datad0g.com/api/v2/apmtelemetry",
        ),
        api_key: Some(Cow::Owned(std::env::var("DD_API_KEY").unwrap())),
        ..Default::default()
    });
    push_telemetry(&config, &req).await.unwrap();
}
