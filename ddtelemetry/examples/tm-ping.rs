// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use ddtelemetry::{
    build_host,
    config::Config,
    data::{self, AppStarted, Application, Telemetry},
    worker::http_client::request_builder,
};
use http::header::CONTENT_TYPE;

fn build_app_started_payload() -> AppStarted {
    AppStarted {
        configuration: Vec::new(),
    }
}

fn seq_id() -> u64 {
    static SEQ_ID: AtomicU64 = AtomicU64::new(0);
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

pub async fn push_telemetry(telemetry: &Telemetry<'_>) -> anyhow::Result<()> {
    let config = Config::get();
    let client = ddtelemetry::worker::http_client::from_config(config);
    let req = request_builder(config)?
        .method(http::Method::POST)
        .header(CONTENT_TYPE, ddcommon::header::APPLICATION_JSON)
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
    let app = Application {
        service_name: String::from(env!("CARGO_PKG_NAME")),
        service_version: Some(String::from(env!("CARGO_PKG_VERSION"))),
        env: None,
        language_name: String::from("rust"),
        language_version: String::from("n/a"),
        tracer_version: String::from("n/a"),
        runtime_name: None,
        runtime_version: None,
        runtime_patches: None,
    };
    let host = build_host();
    let payload = data::payload::Payload::AppStarted(build_app_started_payload());
    let telemetry = build_request(&app, &host, &payload);

    println!(
        "Payload to be sent: {}",
        serde_json::to_string_pretty(&telemetry).unwrap()
    );

    push_telemetry(&telemetry).await?;

    println!("Telemetry submitted correctly");
    Ok(())
}
