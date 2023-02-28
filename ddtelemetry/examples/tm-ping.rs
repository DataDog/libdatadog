// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{sync::atomic::{AtomicU64, Ordering}, time::{SystemTime}};

use ddtelemetry::{data::{self, Telemetry, AppStarted, Application}, build_host, config::Config};
use http::header::CONTENT_TYPE;

fn build_app_started_payload() -> AppStarted {
    AppStarted {
        integrations: Vec::new(),
        dependencies: Vec::new(),
        config: Vec::new(),
    }
}

fn seq_id() -> u64 {
    static SEQ_ID: AtomicU64 = AtomicU64::new(0);
    SEQ_ID.fetch_add(1, Ordering::SeqCst)
}


fn build_request<'a>(
    application: &'a data::Application,
    host: &'a data::Host,
    payload: data::Payload,
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

pub async fn push_telemetry(telemetry: &Telemetry<'_>) -> anyhow::Result<()> {
    let config = Config::get();
    let client = config.http_client();
    let req = config
        .into_request_builder()?
        .method(http::Method::POST)
        .header(CONTENT_TYPE, "application/json")
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

// Simple worker that sends app-started telemetry request to the backend then exits
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let app = Application::new_rust_app();
    let host = build_host();
    let payload = build_app_started_payload();

    let telemetry = build_request(&app, &host, data::payload::Payload::AppStarted(payload));

    println!(
        "Payload to be sent: {}",
        serde_json::to_string_pretty(&telemetry).unwrap()
    );

    push_telemetry(&telemetry).await?;

    println!("Telemetry submitted correctly");
    Ok(())
}
