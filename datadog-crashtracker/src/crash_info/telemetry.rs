// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{fmt::Write, time::SystemTime};

use crate::SigInfo;

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

#[derive(Debug)]
struct TelemetryMetadata {
    application: ddtelemetry::data::Application,
    host: ddtelemetry::data::Host,
    runtime_id: String,
}

pub struct CrashPingBuilder {
    crash_uuid: Option<String>,
    sig_info: Option<SigInfo>,
    endpoint: Option<Endpoint>,
    custom_message: Option<String>,
    os_info: Option<super::OsInfo>,
    proc_info: Option<super::ProcInfo>,
}

impl CrashPingBuilder {
    pub fn new() -> Self {
        Self {
            crash_uuid: None,
            sig_info: None,
            endpoint: None,
            custom_message: None,
            os_info: None,
            proc_info: None,
        }
    }

    pub fn with_crash_uuid(mut self, uuid: String) -> Self {
        self.crash_uuid = Some(uuid);
        self
    }

    pub fn with_sig_info(mut self, sig_info: SigInfo) -> Self {
        self.sig_info = Some(sig_info);
        self
    }

    pub fn with_endpoint(mut self, endpoint: Option<Endpoint>) -> Self {
        self.endpoint = endpoint;
        self
    }

    pub fn with_custom_message(mut self, message: String) -> Self {
        self.custom_message = Some(message);
        self
    }

    /// Set the OS information (optional - for additional insight)
    pub fn with_os_info(mut self, os_info: super::OsInfo) -> Self {
        self.os_info = Some(os_info);
        self
    }

    /// Set the OS information to current machine (optional - for additional insight)
    pub fn with_os_info_this_machine(mut self) -> Self {
        let os_info = ::os_info::get();
        let crashtracker_os_info: super::OsInfo = os_info.into();
        self.os_info = Some(crashtracker_os_info);
        self
    }

    /// Set the process information (optional - for additional insight)
    pub fn with_proc_info(mut self, proc_info: super::ProcInfo) -> Self {
        self.proc_info = Some(proc_info);
        self
    }

    /// Set the process information to current process (optional - for additional insight)
    pub fn with_proc_info_this_process(mut self) -> Self {
        let pid = std::process::id();
        let proc_info = super::ProcInfo { pid };
        self.proc_info = Some(proc_info);
        self
    }

    pub fn build(self) -> anyhow::Result<CrashPing> {
        let crash_uuid = self.crash_uuid.context("crash_uuid is required")?;
        let sig_info = self.sig_info.context("sig_info is required")?;

        let message = self.custom_message.unwrap_or_else(|| {
            format!(
                "Crashtracker crash ping: crash processing started - Process terminated with {:?} ({:?})",
                sig_info.si_code_human_readable, sig_info.si_signo_human_readable
            )
        });

        Ok(CrashPing {
            crash_uuid,
            siginfo: sig_info,
            message,
            version: CrashPing::current_schema_version(),
            kind: "Crash ping".to_string(),
            endpoint: self.endpoint,
            os_info: self.os_info,
            proc_info: self.proc_info,
        })
    }
}

impl Default for CrashPingBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct CrashPing {
    crash_uuid: String,
    siginfo: SigInfo,
    message: String,
    version: String,
    kind: String,
    #[serde(skip)]
    endpoint: Option<Endpoint>,
    os_info: Option<super::OsInfo>,
    proc_info: Option<super::ProcInfo>,
}

impl CrashPing {
    pub fn crash_uuid(&self) -> &str {
        &self.crash_uuid
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn endpoint(&self) -> &Option<Endpoint> {
        &self.endpoint
    }

    fn current_schema_version() -> String {
        "1.0".to_string()
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

    pub async fn upload_crash_ping(&self, crash_ping: &CrashPing) -> anyhow::Result<()> {
        self.upload_crash_ping_internal(crash_ping, &crash_ping.crash_uuid, &crash_ping.siginfo)
            .await
    }

    async fn upload_crash_ping_internal(
        &self,
        crash_ping: &CrashPing,
        crash_uuid: &str,
        sig_info: &SigInfo,
    ) -> anyhow::Result<()> {
        let tags = self.build_crash_ping_tags(crash_uuid, sig_info);
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
            api_version: ddtelemetry::data::ApiVersion::V2,
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
    use super::{CrashPingBuilder, TelemetryCrashUploader};
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

        let crash_uuid = "test-uuid-12345";
        let sig_info = crate::SigInfo::test_instance(42);

        // Build crash ping and upload using the new pattern
        let crash_ping = CrashPingBuilder::new()
            .with_crash_uuid(crash_uuid.to_string())
            .with_sig_info(sig_info.clone())
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

        let crash_uuid = "test-enhanced-uuid-67890";
        let sig_info = crate::SigInfo::test_instance(123);

        let crash_ping = CrashPingBuilder::new()
            .with_crash_uuid(crash_uuid.to_string())
            .with_sig_info(sig_info.clone())
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

        let crash_uuid = "builder-test-uuid-12345";
        let sig_info = crate::SigInfo::test_instance(42);
        let metadata = Metadata::test_instance(1);

        let crash_ping = CrashPingBuilder::new()
            .with_crash_uuid(crash_uuid.to_string())
            .with_sig_info(sig_info.clone())
            .with_endpoint(Some(Endpoint::from_slice(&format!(
                "file://{}",
                output_filename.to_str().unwrap()
            ))))
            .build()?;

        // Test getters
        assert_eq!(crash_ping.crash_uuid(), crash_uuid);
        assert!(crash_ping.message().contains("crash processing started"));

        // Use TelemetryCrashUploader to upload the crash ping
        let mut uploader = TelemetryCrashUploader::new(&metadata, crash_ping.endpoint())?;
        uploader
            .cfg
            .set_host_from_url(&format!(
                "file://{}.telemetry",
                output_filename.to_str().unwrap()
            ))
            .unwrap();

        let crash_ping = CrashPingBuilder::new()
            .with_crash_uuid(crash_uuid.to_string())
            .with_sig_info(sig_info.clone())
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
        // Test missing required fields
        let result = CrashPingBuilder::new().build();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("crash_uuid is required"));

        let result = CrashPingBuilder::new()
            .with_crash_uuid("test".to_string())
            .build();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("sig_info is required"));

        let result = CrashPingBuilder::new()
            .with_crash_uuid("test".to_string())
            .with_sig_info(crate::SigInfo::test_instance(1))
            .build();
        assert!(result.is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_crash_ping_builder_default() {
        let builder = CrashPingBuilder::default();
        let result = builder.build();
        assert!(result.is_err());
    }
}
