// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{fmt::Write, time::SystemTime};

use super::{CrashInfo, Metadata};
use anyhow::{Context, Ok};
use chrono::{DateTime, Utc};
use ddcommon::Endpoint;
use ddtelemetry::{
    build_host,
    data::{self, Application, LogLevel},
    worker::http_client::request_builder,
};
use serde::Serialize;

struct TelemetryMetadata {
    application: ddtelemetry::data::Application,
    host: ddtelemetry::data::Host,
    runtime_id: String,
}

#[derive(Serialize)]
struct HeartbeatMessage {
    crash_uuid: String,
    message: String,
}

macro_rules! parse_tags {
    (   $tag_iterator:expr,
        $($tag_name:literal => $var:ident),* $(,)?)  => {
        $(
            let mut $var: Option<&str> = None;
        )*
        for tag in $tag_iterator {
            let Some((name, value)) = tag.split_once(':') else {
                continue;
            };
            match name {
                $($tag_name => {$var = Some(value);}, )*
                _ => {},
            }
        }

    };
}

pub struct TelemetryCrashUploader {
    metadata: TelemetryMetadata,
    cfg: ddtelemetry::config::Config,
}

impl TelemetryCrashUploader {
    pub fn new(
        crashtracker_metadata: &Metadata,
        endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<Self> {
        let mut cfg = ddtelemetry::config::Config::from_env();
        if let Some(endpoint) = endpoint {
            // TODO: This changes the path part of the query to target the agent.
            // What about if the crashtracker is sending directly to the intake?
            // We probably need to remap the host from intake.profile.{site} to
            // instrumentation-telemetry-intake.{site}?
            // But do we want to support direct submission to the intake?

            // ignore result because what are we going to do?
            let _ = if endpoint.url.scheme_str() == Some("file") {
                let path = ddcommon::decode_uri_path_in_authority(&endpoint.url)
                    .context("file path is not valid")?;
                cfg.set_host_from_url(&format!("file://{}.telemetry", path.display()))
            } else {
                cfg.set_endpoint(endpoint.clone())
            };
        }

        parse_tags!(
            crashtracker_metadata.tags.iter(),
            "env" => env,
            "language" => language_name,
            "library_version" => library_version,
            "profiler_version" => profiler_version,
            "runtime_version" => language_version,
            "runtime-id" => runtime_id,
            "service_version" => service_version,
            "service" => service_name,
        );

        let application = Application {
            service_name: service_name.unwrap_or("unknown").to_owned(),
            language_name: language_name.unwrap_or("unknown").to_owned(),
            language_version: language_version.unwrap_or("unknown").to_owned(),
            tracer_version: library_version
                .or(profiler_version)
                .unwrap_or("unknown")
                .to_owned(),
            env: env.map(ToOwned::to_owned),
            service_version: service_version.map(ToOwned::to_owned),
            ..Default::default()
        };

        let host = build_host();

        let s = Self {
            metadata: TelemetryMetadata {
                host,
                application,
                runtime_id: runtime_id.unwrap_or("unknown").to_owned(),
            },
            cfg,
        };
        Ok(s)
    }

    /// Sends heartbeat message with customer application and runtime information.
    pub async fn send_heartbeat(&self, crash_uuid: &str) -> anyhow::Result<()> {
        let metadata = &self.metadata;

        let tracer_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut tags = format!(
            "uuid:{},is_heartbeat:true,service:{},language_name:{},language_version:{},tracer_version:{}",
            crash_uuid,
            metadata.application.service_name,
            metadata.application.language_name,
            metadata.application.language_version,
            metadata.application.tracer_version
        );

        if let Some(env) = &metadata.application.env {
            tags.push_str(&format!(",env:{}", env));
        }
        if let Some(runtime_name) = &metadata.application.runtime_name {
            tags.push_str(&format!(",runtime_name:{}", runtime_name));
        }
        if let Some(runtime_version) = &metadata.application.runtime_version {
            tags.push_str(&format!(",runtime_version:{}", runtime_version));
        }

        let heartbeat_msg = HeartbeatMessage {
            crash_uuid: crash_uuid.to_string(),
            message: "Crashtracker heartbeat: crash processing started".to_string(),
        };

        let payload = data::Telemetry {
            tracer_time,
            api_version: ddtelemetry::data::ApiVersion::V2,
            runtime_id: &metadata.runtime_id,
            seq_id: 1,
            application: &metadata.application,
            host: &metadata.host,
            payload: &data::Payload::Logs(vec![data::Log {
                message: serde_json::to_string(&heartbeat_msg)?,
                level: LogLevel::Warn,
                stack_trace: None,
                tags,
                is_sensitive: false,
                count: 1,
                is_crash: false,
            }]),
            origin: Some("Crashtracker"),
        };

        self.send_telemetry_payload(&payload).await
    }

    pub async fn upload_to_telemetry(&self, crash_info: &CrashInfo) -> anyhow::Result<()> {
        let metadata = &self.metadata;

        let message = serde_json::to_string(crash_info)?;

        let tags = extract_crash_info_tags(crash_info).unwrap_or_default();

        let tracer_time = crash_info.timestamp.parse::<DateTime<Utc>>().map_or_else(
            |_| {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            },
            |ts| ts.timestamp() as u64,
        );

        let payload = data::Telemetry {
            tracer_time,
            api_version: ddtelemetry::data::ApiVersion::V2,
            runtime_id: &metadata.runtime_id,
            seq_id: 1,
            application: &metadata.application,
            host: &metadata.host,
            payload: &data::Payload::Logs(vec![data::Log {
                message,
                level: LogLevel::Error,
                // The stacktrace is already included in the `crash_info` inside `message`.
                stack_trace: None,
                tags,
                is_sensitive: true,
                count: 1,
                is_crash: true,
            }]),
            origin: Some("Crashtracker"),
        };

        self.send_telemetry_payload(&payload).await
    }

    async fn send_telemetry_payload(&self, payload: &data::Telemetry<'_>) -> anyhow::Result<()> {
        let client = ddtelemetry::worker::http_client::from_config(&self.cfg);
        let req = request_builder(&self.cfg)?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                ddcommon::header::APPLICATION_JSON,
            )
            .header(
                ddtelemetry::worker::http_client::header::API_VERSION,
                ddtelemetry::data::ApiVersion::V2.to_str(),
            )
            .header(
                ddtelemetry::worker::http_client::header::REQUEST_TYPE,
                "logs",
            )
            .body(serde_json::to_string(&payload)?.into())?;

        tokio::time::timeout(
            std::time::Duration::from_millis({
                if let Some(endp) = self.cfg.endpoint() {
                    endp.timeout_ms
                } else {
                    Endpoint::DEFAULT_TIMEOUT
                }
            }),
            client.request(req),
        )
        .await??;

        Ok(())
    }
}

fn extract_crash_info_tags(crash_info: &CrashInfo) -> anyhow::Result<String> {
    let mut tags = String::new();
    write!(
        &mut tags,
        "data_schema_version:{}",
        crash_info.data_schema_version
    )?;
    if let Some(fingerprint) = &crash_info.fingerprint {
        write!(&mut tags, ",fingerprint:{fingerprint}")?;
    }
    write!(&mut tags, ",incomplete:{}", crash_info.incomplete)?;
    write!(&mut tags, ",is_crash:{}", crash_info.error.is_crash)?;
    write!(&mut tags, ",uuid:{}", crash_info.uuid)?;
    for (counter, value) in &crash_info.counters {
        write!(&mut tags, ",{counter}:{value}")?;
    }

    if let Some(siginfo) = &crash_info.sig_info {
        if let Some(si_addr) = &siginfo.si_addr {
            write!(&mut tags, ",si_addr:{si_addr}")?;
        }
        write!(&mut tags, ",si_code:{}", siginfo.si_code)?;
        write!(
            &mut tags,
            ",si_code_human_readable:{:?}",
            siginfo.si_code_human_readable
        )?;
        write!(&mut tags, ",si_signo:{}", siginfo.si_signo)?;
        write!(
            &mut tags,
            ",si_signo_human_readable:{:?}",
            siginfo.si_signo_human_readable
        )?;
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::TelemetryCrashUploader;
    use crate::crash_info::{test_utils::TestInstance, CrashInfo, Metadata};
    use ddcommon::Endpoint;
    use std::{collections::HashSet, fs};

    fn new_test_uploader(seed: u64) -> TelemetryCrashUploader {
        TelemetryCrashUploader::new(
            &Metadata::test_instance(seed),
            &Some(Endpoint::from_slice("http://localhost:8126")),
        )
        .unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_profiler_config_extraction() {
        let t = new_test_uploader(1);

        let metadata = t.metadata;
        assert_eq!(metadata.application.service_name, "foo");
        assert_eq!(metadata.application.service_version.as_deref(), Some("bar"));
        assert_eq!(metadata.application.language_name, "native");
        assert_eq!(metadata.runtime_id, "xyz");
        let cfg = t.cfg;
        assert_eq!(
            cfg.endpoint().unwrap().url.to_string(),
            "http://localhost:8126/telemetry/proxy/api/v2/apmtelemetry"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_crash_request_content() -> anyhow::Result<()> {
        // This keeps alive for scope
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("crash_info");
            p
        };
        let seed = 1;
        let mut t = new_test_uploader(seed);

        t.cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();
        let test_instance = super::CrashInfo::test_instance(seed);

        t.upload_to_telemetry(&test_instance).await.unwrap();

        let payload: serde_json::value::Value =
            serde_json::de::from_str(&fs::read_to_string(&output_filename).unwrap()).unwrap();
        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["application"]["language_name"], "native");
        assert_eq!(payload["application"]["service_name"], "foo");
        assert_eq!(payload["application"]["service_version"], "bar");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["tracer_time"], 1568898000);
        assert_eq!(payload["origin"], "Crashtracker");

        assert_eq!(payload["payload"].as_array().unwrap().len(), 1);
        let tags = payload["payload"][0]["tags"]
            .as_str()
            .unwrap()
            .split(',')
            .collect::<HashSet<_>>();
        assert_eq!(
            HashSet::from_iter([
                "collecting_sample:1",
                "data_schema_version:1.4",
                "incomplete:true",
                "is_crash:true",
                "not_profiling:0",
                "si_addr:0x0000000000001234",
                "si_code_human_readable:SEGV_BNDERR",
                "si_code:1",
                "si_signo_human_readable:SIGSEGV",
                "si_signo:11",
                "uuid:1d6b97cb-968c-40c9-af6e-e4b4d71e8781",
            ]),
            tags
        );
        assert_eq!(payload["payload"][0]["is_sensitive"], true);
        assert_eq!(payload["payload"][0]["level"], "ERROR");
        let body: CrashInfo =
            serde_json::from_str(payload["payload"][0]["message"].as_str().unwrap())?;
        assert_eq!(body, test_instance);
        assert_eq!(payload["payload"][0]["is_crash"], true);
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_content() -> anyhow::Result<()> {
        // This keeps alive for scope
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("heartbeat_info");
            p
        };
        let seed = 1;
        let mut t = new_test_uploader(seed);

        t.cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let crash_uuid = "test-uuid-12345";

        t.send_heartbeat(crash_uuid).await.unwrap();

        let payload: serde_json::value::Value =
            serde_json::de::from_str(&fs::read_to_string(&output_filename).unwrap()).unwrap();
        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["application"]["language_name"], "native");
        assert_eq!(payload["application"]["service_name"], "foo");
        assert_eq!(payload["application"]["service_version"], "bar");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["origin"], "Crashtracker");

        assert_eq!(payload["payload"].as_array().unwrap().len(), 1);
        let log_entry = &payload["payload"][0];

        // Heartbeat properties
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["level"], "WARN");

        // Structured message format
        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;
        assert_eq!(message_json["crash_uuid"], crash_uuid);
        assert_eq!(
            message_json["message"],
            "Crashtracker heartbeat: crash processing started"
        );

        // Customer application and runtime information tags
        let tags = log_entry["tags"].as_str().unwrap();
        assert!(tags.contains(&format!("uuid:{}", crash_uuid)));
        assert!(tags.contains("is_heartbeat:true"));
        assert!(tags.contains("service:foo"));
        assert!(tags.contains("language_name:native"));
        assert!(tags.contains("language_version:"));
        assert!(tags.contains("tracer_version:"));

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_with_different_config() -> anyhow::Result<()> {
        // This keeps alive for scope
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("enhanced_heartbeat_info");
            p
        };
        let seed = 1;
        let mut t = new_test_uploader(seed);

        t.cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let crash_uuid = "test-enhanced-uuid-67890";

        t.send_heartbeat(crash_uuid).await.unwrap();

        let payload: serde_json::value::Value =
            serde_json::de::from_str(&fs::read_to_string(&output_filename).unwrap()).unwrap();
        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["application"]["language_name"], "native");
        assert_eq!(payload["application"]["service_name"], "foo");
        assert_eq!(payload["application"]["service_version"], "bar");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["origin"], "Crashtracker");

        assert_eq!(payload["payload"].as_array().unwrap().len(), 1);
        let log_entry = &payload["payload"][0];

        // Heartbeat properties
        assert_eq!(log_entry["is_crash"], false);
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["level"], "WARN");

        // Structured message format
        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;
        assert_eq!(message_json["crash_uuid"], crash_uuid);
        assert_eq!(
            message_json["message"],
            "Crashtracker heartbeat: crash processing started"
        );

        // Customer application and runtime information tags
        let tags = log_entry["tags"].as_str().unwrap();
        assert!(tags.contains(&format!("uuid:{}", crash_uuid)));
        assert!(tags.contains("is_heartbeat:true"));
        assert!(tags.contains("service:foo"));
        assert!(tags.contains("language_name:native"));
        assert!(tags.contains("language_version:"));
        assert!(tags.contains("tracer_version:"));

        Ok(())
    }
}
