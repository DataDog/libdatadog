// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use bytes::Bytes;
use http::header::CONTENT_TYPE;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_telemetry::{
    build_host,
    config::Config,
    data::{self, AppStarted, Application, Telemetry},
    worker::http_client::request_builder,
};
use std::time::Duration;
use tokio::select;

fn build_app_started_payload() -> AppStarted {
    AppStarted {
        configuration: Vec::new(),
        dependencies: Vec::new(),
        integrations: Vec::new(),
    }
}

fn seq_id() -> u64 {
    static SEQ_ID: AtomicU64 = AtomicU64::new(0);
    SEQ_ID.fetch_add(1, Ordering::Relaxed)
}

fn build_request<'a>(
    application: &'a data::Application,
    host: &'a data::Host,
    payload: &'a data::Payload,
) -> data::Telemetry<'a> {
    data::Telemetry {
        api_version: data::ApiVersion::V2,
        tracer_time: SystemTime::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_secs()),
        runtime_id: "runtime_id",
        seq_id: seq_id(),
        origin: Some("tm-ping"),
        application,
        host,
        payload,
    }
}

pub async fn push_telemetry(telemetry: &Telemetry<'_>) -> anyhow::Result<()> {
    let config = Config::from_env();
    let timeout = Duration::from_millis(
        config
            .endpoint()
            .map(|e| e.timeout_ms)
            .unwrap_or(libdd_common::Endpoint::DEFAULT_TIMEOUT),
    );
    let client = NativeCapabilities::new_client();
    let sleeper = <NativeCapabilities as SleepCapability>::new();
    let req = request_builder(&config)?
        .method(http::Method::POST)
        .header(CONTENT_TYPE, libdd_common::header::APPLICATION_JSON)
        .body(Bytes::from(serde_json::to_vec(telemetry)?))?;

    let resp = select! {
        biased;
        result = client.request(req) => result?,
        _ = sleeper.sleep(timeout) => {
            return Err(anyhow::anyhow!("Telemetry request timed out"));
        }
    };

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
        process_tags: None,
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
