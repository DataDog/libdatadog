// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{fmt::Write, time::SystemTime};

use crate::SigInfo;

use super::{CrashInfo, Metadata};
use anyhow::{Context, Ok};
use chrono::{DateTime, Utc};
use libdd_common::Endpoint;
use libdd_telemetry::{
    build_host,
    data::{self, Application, LogLevel},
    worker::http_client::request_builder,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug)]
struct TelemetryMetadata {
    application: libdd_telemetry::data::Application,
    host: libdd_telemetry::data::Host,
    runtime_id: String,
}

pub struct CrashPingBuilder {
    crash_uuid: Uuid,
    custom_message: Option<String>,
    metadata: Option<Metadata>,
    sig_info: Option<SigInfo>,
}

impl CrashPingBuilder {
    /// Crash pings should only be initalized and built by the CrashInfoBuilder
    /// We require the crash uuid to be passed in because a CrashPing is always
    /// associated with a specific crash.
    pub fn new(crash_uuid: Uuid) -> Self {
        Self {
            crash_uuid,
            custom_message: None,
            metadata: None,
            sig_info: None,
        }
    }

    pub fn with_crash_uuid(mut self, uuid: Uuid) -> Self {
        self.crash_uuid = uuid;
        self
    }

    pub fn with_sig_info(mut self, sig_info: SigInfo) -> Self {
        self.sig_info = Some(sig_info);
        self
    }

    pub fn with_custom_message(mut self, message: String) -> Self {
        self.custom_message = Some(message);
        self
    }

    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn build(self) -> anyhow::Result<CrashPing> {
        let crash_uuid = self.crash_uuid;
        let sig_info = self.sig_info.context("sig_info is required")?;
        let metadata = self.metadata.context("metadata is required")?;

        let message = self.custom_message.unwrap_or_else(|| {
            format!(
                "Crashtracker crash ping: crash processing started - Process terminated with {:?} ({:?})",
                sig_info.si_code_human_readable, sig_info.si_signo_human_readable
            )
        });

        Ok(CrashPing {
            crash_uuid: crash_uuid.to_string(),
            kind: "Crash ping".to_string(),
            message,
            metadata,
            siginfo: sig_info,
            version: CrashPing::current_schema_version(),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct CrashPing {
    crash_uuid: String,
    siginfo: SigInfo,
    message: String,
    version: String,
    kind: String,
    metadata: Metadata,
}

impl CrashPing {
    pub fn crash_uuid(&self) -> &str {
        &self.crash_uuid
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn siginfo(&self) -> &SigInfo {
        &self.siginfo
    }

    fn current_schema_version() -> String {
        "1.0".to_string()
    }

    pub fn upload_to_endpoint(&self, endpoint: &Option<Endpoint>) -> anyhow::Result<()> {
        // Check early to avoid creating a tokio runtime if we're not going to use it
        if endpoint.as_ref().is_some_and(|e| e.is_file_endpoint()) {
            return Ok(());
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async { self.upload_to_endpoint_async(endpoint).await })
    }

    /// Sends this crash ping telemetry event to indicate that crash processing has started.
    /// We no-op on file endpoints because unlike production environments, we know if
    /// a crash report failed to send when file debugging.
    pub async fn upload_to_endpoint_async(
        &self,
        endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<()> {
        if endpoint.as_ref().is_some_and(|e| e.is_file_endpoint()) {
            return Ok(());
        }
        let telemetry_uploader = crate::TelemetryCrashUploader::new(self.metadata(), endpoint)?;
        telemetry_uploader.upload_crash_ping(self).await
    }
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
    cfg: libdd_telemetry::config::Config,
}

impl TelemetryCrashUploader {
    pub fn new(
        crashtracker_metadata: &Metadata,
        endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<Self> {
        let mut cfg = libdd_telemetry::config::Config::from_env();
        if let Some(endpoint) = endpoint {
            // TODO: This changes the path part of the query to target the agent.
            // What about if the crashtracker is sending directly to the intake?
            // We probably need to remap the host from intake.profile.{site} to
            // instrumentation-telemetry-intake.{site}?
            // But do we want to support direct submission to the intake?

            // ignore result because what are we going to do?
            let _ = if endpoint.url.scheme_str() == Some("file") {
                let path = libdd_common::decode_uri_path_in_authority(&endpoint.url)
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

    pub async fn upload_crash_ping(&self, crash_ping: &CrashPing) -> anyhow::Result<()> {
        let tags = self.build_crash_ping_tags(crash_ping.crash_uuid(), crash_ping.siginfo());
        let tracer_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let message = serde_json::to_string(crash_ping)?;

        self.send_log_payload(
            message,
            tags,
            tracer_time,
            LogLevel::Debug,
            false, // is_sensitive
            false, // is_crash
        )
        .await
    }

    fn build_crash_ping_tags(&self, crash_uuid: &str, sig_info: &SigInfo) -> String {
        let metadata = &self.metadata;
        let mut tags = format!(
            "uuid:{},is_crash_ping:true,service:{},language_name:{},language_version:{},tracer_version:{},si_code_human_readable:{:?},si_signo:{},si_signo_human_readable:{:?}",
            crash_uuid,
            metadata.application.service_name,
            metadata.application.language_name,
            metadata.application.language_version,
            metadata.application.tracer_version,
            sig_info.si_code_human_readable,
            sig_info.si_signo,
            sig_info.si_signo_human_readable
        );

        self.append_optional_tags(&mut tags);
        tags
    }

    fn append_optional_tags(&self, tags: &mut String) {
        let metadata = &self.metadata;
        if let Some(env) = &metadata.application.env {
            tags.push_str(&format!(",env:{env}"));
        }
        if let Some(runtime_name) = &metadata.application.runtime_name {
            tags.push_str(&format!(",runtime_name:{runtime_name}"));
        }
        if let Some(runtime_version) = &metadata.application.runtime_version {
            tags.push_str(&format!(",runtime_version:{runtime_version}"));
        }
    }

    pub async fn upload_to_telemetry(&self, crash_info: &CrashInfo) -> anyhow::Result<()> {
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

        self.send_log_payload(
            message,
            tags,
            tracer_time,
            LogLevel::Error,
            true, // is_sensitive
            true, // is_crash
        )
        .await
    }

    async fn send_log_payload(
        &self,
        message: String,
        tags: String,
        tracer_time: u64,
        level: LogLevel,
        is_sensitive: bool,
        is_crash: bool,
    ) -> anyhow::Result<()> {
        let payload = data::Telemetry {
            tracer_time,
            api_version: libdd_telemetry::data::ApiVersion::V2,
            runtime_id: &self.metadata.runtime_id,
            seq_id: 1,
            application: &self.metadata.application,
            host: &self.metadata.host,
            payload: &data::Payload::Logs(vec![data::Log {
                message,
                level,
                stack_trace: None,
                tags,
                is_sensitive,
                count: 1,
                is_crash,
            }]),
            origin: Some("Crashtracker"),
        };

        self.send_telemetry_payload(&payload).await
    }

    async fn send_telemetry_payload(&self, payload: &data::Telemetry<'_>) -> anyhow::Result<()> {
        let client = libdd_telemetry::worker::http_client::from_config(&self.cfg);
        let req = request_builder(&self.cfg)?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                libdd_common::header::APPLICATION_JSON,
            )
            .header(
                libdd_telemetry::worker::http_client::header::API_VERSION,
                libdd_telemetry::data::ApiVersion::V2.to_str(),
            )
            .header(
                libdd_telemetry::worker::http_client::header::REQUEST_TYPE,
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
    use super::{CrashPingBuilder, TelemetryCrashUploader};
    use crate::crash_info::{test_utils::TestInstance, CrashInfo, Metadata};
    use libdd_common::Endpoint;
    use std::{collections::HashSet, fs};
    use uuid::Uuid;

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
    async fn test_crash_ping_content() -> anyhow::Result<()> {
        // This keeps alive for scope
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("crash_ping_info");
            p
        };
        let seed = 1;
        let mut t = new_test_uploader(seed);

        t.cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let crash_uuid = "19ea82a5-2118-4fb0-b0dd-6c067a3026c6";
        let sig_info = crate::SigInfo::test_instance(42);

        // Build crash ping and upload using the new pattern
        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(Metadata::test_instance(1))
            .build()
            .unwrap();
        t.upload_crash_ping(&crash_ping).await.unwrap();

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

        // Crash ping properties
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["level"], "DEBUG");

        // Structured message format
        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;
        assert_eq!(message_json["siginfo"], serde_json::to_value(sig_info)?);
        assert_eq!(message_json["crash_uuid"], crash_uuid);

        assert_eq!(message_json["version"], "1.0");
        assert_eq!(message_json["kind"], "Crash ping");

        let metadata_in_message = &message_json["metadata"];
        assert!(
            metadata_in_message.is_object(),
            "metadata should be an object"
        );
        let expected_metadata = serde_json::to_value(Metadata::test_instance(1))?;
        assert_eq!(
            metadata_in_message, &expected_metadata,
            "metadata field should match expected structure"
        );

        let tags = log_entry["tags"].as_str().unwrap();
        assert!(tags.contains(&format!("uuid:{crash_uuid}")));
        assert!(tags.contains("is_crash_ping:true"));
        assert!(tags.contains("service:foo"));
        assert!(tags.contains("language_name:native"));
        assert!(tags.contains("language_version:"));
        assert!(tags.contains("tracer_version:"));

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_crash_ping_with_different_config() -> anyhow::Result<()> {
        // This keeps alive for scope
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("enhanced_crash_ping_info");
            p
        };
        let seed = 1;
        let mut t = new_test_uploader(seed);

        t.cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let crash_uuid = "19ea82a5-2118-4fb0-b0dd-6c067a3026c6";
        let sig_info = crate::SigInfo::test_instance(123);

        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(Metadata::test_instance(1))
            .build()
            .unwrap();
        t.upload_crash_ping(&crash_ping).await.unwrap();

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

        // Crash ping properties
        assert_eq!(log_entry["is_crash"], false);
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["level"], "DEBUG");

        // Structured message format
        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;
        assert_eq!(message_json["crash_uuid"], crash_uuid);
        assert_eq!(
            message_json["message"],
            format!(
                "Crashtracker crash ping: crash processing started - Process terminated with {:?} ({:?})",
                sig_info.si_code_human_readable, sig_info.si_signo_human_readable
            )
        );

        let metadata_in_message = &message_json["metadata"];
        assert!(
            metadata_in_message.is_object(),
            "metadata should be an object"
        );
        let expected_metadata = serde_json::to_value(Metadata::test_instance(1))?;
        assert_eq!(
            metadata_in_message, &expected_metadata,
            "metadata field should match expected structure"
        );

        let siginfo_in_message = &message_json["siginfo"];
        let expected_siginfo = serde_json::to_value(&sig_info)?;
        assert_eq!(
            siginfo_in_message, &expected_siginfo,
            "siginfo field should match expected structure"
        );

        assert_eq!(message_json["version"], "1.0");
        assert_eq!(message_json["kind"], "Crash ping");

        // Customer application and runtime information tags
        let tags = log_entry["tags"].as_str().unwrap();
        assert!(tags.contains(&format!("uuid:{crash_uuid}")));
        assert!(tags.contains("is_crash_ping:true"));
        assert!(tags.contains("service:foo"));
        assert!(tags.contains("language_name:native"));
        assert!(tags.contains("language_version:"));
        assert!(tags.contains("tracer_version:"));

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_crash_ping_builder_basic() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("crash_ping_builder_test");
            p
        };

        let crash_uuid = "19ea82a5-2118-4fb0-b0dd-6c067a3026c6";
        let sig_info = crate::SigInfo::test_instance(42);
        let metadata = Metadata::test_instance(1);

        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(metadata.clone())
            .build()?;

        let endpoint = Some(Endpoint::from_slice(&format!(
            "file://{}",
            output_filename.to_str().unwrap()
        )));

        // Test getters
        assert_eq!(crash_ping.crash_uuid(), crash_uuid);
        assert!(crash_ping.message().contains("crash processing started"));
        assert_eq!(crash_ping.metadata(), &metadata);

        // Use TelemetryCrashUploader to upload the crash ping
        let mut uploader = TelemetryCrashUploader::new(&metadata, &endpoint)?;
        uploader
            .cfg
            .set_host_from_url(&format!(
                "file://{}.telemetry",
                output_filename.to_str().unwrap()
            ))
            .unwrap();

        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(metadata.clone())
            .build()?;
        uploader.upload_crash_ping(&crash_ping).await?;

        // Verify the .telemetry file was created with correct content
        let telemetry_filename = format!("{}.telemetry", output_filename.to_str().unwrap());
        let payload: serde_json::value::Value =
            serde_json::de::from_str(&std::fs::read_to_string(&telemetry_filename)?)?;

        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["origin"], "Crashtracker");

        let log_entry = &payload["payload"][0];
        assert_eq!(log_entry["level"], "DEBUG");
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["is_crash"], false);

        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;
        assert_eq!(message_json["crash_uuid"], crash_uuid);
        assert_eq!(message_json["version"], "1.0");
        assert_eq!(message_json["kind"], "Crash ping");

        Ok(())
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_crash_ping_builder_validation() {
        let crash_uuid = Uuid::parse_str("19ea82a5-2118-4fb0-b0dd-6c067a3026c6").unwrap();
        // Test missing required fields
        let result = CrashPingBuilder::new(crash_uuid).build();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("sig_info is required"));

        let result = CrashPingBuilder::new(crash_uuid)
            .with_sig_info(crate::SigInfo::test_instance(1))
            .with_metadata(Metadata::test_instance(1))
            .build();
        assert!(result.is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_crash_ping_all_fields_present() {
        let crash_uuid = "19ea82a5-2118-4fb0-b0dd-6c067a3026c6";
        let sig_info = crate::SigInfo::test_instance(99);
        let metadata = Metadata::test_instance(2);
        let custom_message = "Custom crash message for testing";

        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(metadata.clone())
            .with_custom_message(custom_message.to_string())
            .build()
            .unwrap();

        assert_eq!(crash_ping.crash_uuid(), crash_uuid);
        assert_eq!(crash_ping.message(), custom_message);
        assert_eq!(crash_ping.metadata(), &metadata);
        assert_eq!(crash_ping.siginfo(), &sig_info);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_crash_ping_telemetry_upload_all_fields() -> anyhow::Result<()> {
        // Test that when crash ping is uploaded via telemetry, all fields are preserved
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("crash_ping_all_fields_upload");
            p
        };
        let seed = 3;
        let mut uploader = new_test_uploader(seed);

        uploader
            .cfg
            .set_host_from_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let crash_uuid = "19ea82a5-2118-4fb0-b0dd-6c067a3026c6";
        let sig_info = crate::SigInfo::test_instance(150);
        let metadata = Metadata::test_instance(3);

        let crash_ping = CrashPingBuilder::new(Uuid::parse_str(crash_uuid).unwrap())
            .with_sig_info(sig_info.clone())
            .with_metadata(metadata.clone())
            .build()
            .unwrap();

        uploader.upload_crash_ping(&crash_ping).await?;

        let payload: serde_json::value::Value =
            serde_json::de::from_str(&fs::read_to_string(&output_filename).unwrap())?;

        // Verify telemetry structure
        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["origin"], "Crashtracker");

        let log_entry = &payload["payload"][0];
        assert_eq!(log_entry["level"], "DEBUG");
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["is_crash"], false);

        let message_json: serde_json::Value =
            serde_json::from_str(log_entry["message"].as_str().unwrap())?;

        assert_eq!(message_json["crash_uuid"], crash_uuid);
        assert_eq!(message_json["version"], "1.0");
        assert_eq!(message_json["kind"], "Crash ping");

        let uploaded_siginfo = &message_json["siginfo"];
        assert_eq!(uploaded_siginfo["si_signo"], sig_info.si_signo);
        assert_eq!(uploaded_siginfo["si_code"], sig_info.si_code);
        assert_eq!(
            uploaded_siginfo["si_code_human_readable"],
            serde_json::to_value(&sig_info.si_code_human_readable)?
        );
        assert_eq!(
            uploaded_siginfo["si_signo_human_readable"],
            serde_json::to_value(&sig_info.si_signo_human_readable)?
        );

        let uploaded_metadata = &message_json["metadata"];
        assert!(uploaded_metadata.is_object());

        let expected_metadata_json = serde_json::to_value(&metadata)?;
        assert_eq!(uploaded_metadata, &expected_metadata_json);

        assert!(message_json["message"].is_string());
        assert!(message_json["message"]
            .as_str()
            .unwrap()
            .contains("crash processing started"));
        Ok(())
    }
}
