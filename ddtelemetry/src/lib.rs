// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![allow(clippy::mutex_atomic)]

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time,
};

use ddcommon::container_id;
use lazy_static::lazy_static;

use self::{
    comms::request,
    config::Config,
    data::{Application, Telemetry},
};
pub mod comms;
pub mod config;
pub mod data;
pub mod info;
pub mod worker;

const DEFAULT_API_VERSION: data::ApiVersion = data::ApiVersion::V1;

fn runtime_id() -> &'static str {
    lazy_static! {
        static ref RUNTIME_ID: String = uuid::Uuid::new_v4().to_string();
    }

    &RUNTIME_ID
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
        api_version: DEFAULT_API_VERSION,
        tracer_time: time::SystemTime::now()
            .duration_since(time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        runtime_id: runtime_id(),
        seq_id: seq_id(),
        application,
        host,
        payload,
    }
}
pub async fn build_host() -> data::Host {
    data::Host {
        hostname: info::os::real_hostname()
            .await
            .unwrap_or_else(|_| String::from("unknown_hostname")),
        container_id: container_id::get_container_id().map(|f| f.to_string()),
        os: Some(String::from(info::os::os_name())),
        os_version: info::os::os_version().ok(),
        kernel_name: None,
        kernel_release: None,
        kernel_version: None,
    }
}

fn build_app_started_payload() -> data::AppStarted {
    data::AppStarted {
        integrations: Vec::new(),
        dependencies: Vec::new(),
        config: Vec::new(),
    }
}

#[derive(Default)]
pub struct Header {
    host: Option<data::Host>,
    app: Option<data::Application>,
}

// TODO: these are quick and dirty functions to get some examples running
pub async fn build_full(header: &mut Header) -> Telemetry<'_> {
    let Header { host, app } = header;
    let host = match host {
        None => {
            *host = Some(build_host().await);
            host.as_ref().unwrap()
        }
        Some(host) => host,
    };
    let app = match app {
        None => {
            *app = Some(Application::new_rust_app());
            app.as_ref().unwrap()
        }
        Some(app) => app,
    };
    let payload = build_app_started_payload();

    build_request(app, host, data::payload::Payload::AppStarted(payload))
}

pub async fn push_telemetry(telemetry: &Telemetry<'_>) -> anyhow::Result<()> {
    let url = Config::get().telemetry_url();
    let resp = request(
        url,
        serde_json::to_string(telemetry)?,
        Config::get().api_key(),
    )
    .await?;
    if !resp.status().is_success() {
        Err(anyhow::Error::msg(format!(
            "Telemetry error: response status: {}",
            resp.status()
        )))
    } else {
        Ok(())
    }
}
