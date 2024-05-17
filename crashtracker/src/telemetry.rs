// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::fmt::Write;
use std::time::{self, SystemTime};

use super::{CrashInfo, CrashtrackerConfiguration, CrashtrackerMetadata, StackFrame};
use anyhow::Ok;
use ddtelemetry::{
    build_host,
    data::{self, Application, LogLevel},
    worker::http_client::request_builder,
};

struct TelemetryMetadata {
    application: ddtelemetry::data::Application,
    host: ddtelemetry::data::Host,
    runtime_id: String,
}

macro_rules! parse_tags {
    (   $tag_iterator:expr,
        $($tag_name:literal => $var:ident),* $(,)?)  => {
        $(
            let mut $var: Option<&str> = None;
        )*
        for tag in $tag_iterator {
            let Some((name, value)) = tag.as_ref().split_once(':') else {
                continue;
            };
            match name {
                $($tag_name => {$var = Some(value);}, )*
                _ => {},
            }
        }

    };
}

#[derive(Debug, serde::Serialize)]
/// This struct represents the part of the crash_info that we are sending in the
/// log `message` field as a json
struct TelemetryCrashInfoMessage<'a> {
    pub additional_stacktraces: &'a HashMap<String, Vec<StackFrame>>,
    pub files: &'a HashMap<String, Vec<String>>,
    pub metadata: Option<&'a CrashtrackerMetadata>,
    pub os_info: &'a os_info::Info,
    pub tags: &'a HashMap<String, String>,
}

pub struct TelemetryCrashUploader {
    rt: tokio::runtime::Runtime,
    metadata: TelemetryMetadata,
    cfg: ddtelemetry::config::Config,
}

impl TelemetryCrashUploader {
    pub fn new(
        prof_metadata: &CrashtrackerMetadata,
        prof_cfg: &CrashtrackerConfiguration,
    ) -> anyhow::Result<Self> {
        let mut cfg = ddtelemetry::config::Config::from_env();
        if let Some(endpoint) = &prof_cfg.endpoint {
            // TODO: This changes the path part of the query to target the agent.
            // What about if the crashtracker is sending directly to the intake?
            // We probably need to remap the host from intake.profile.{site} to
            // instrumentation-telemetry-intake.{site}?
            // But do we want to support direct submission to the intake?

            // ignore result because what are we going to do?
            let _ = if endpoint.url.scheme_str() == Some("file") {
                cfg.set_url(&format!("file://{}.telemetry", endpoint.url.path()))
            } else {
                cfg.set_endpoint(endpoint.clone())
            };
        }

        parse_tags!(
            prof_metadata.tags.iter(),
            "service" => service_name,
            "service_version" => service_version,
            "language" => language_name,
            "runtime_version" => language_version,
            "library_version" => library_version,
            "profiler_version" => profiler_version,
            "runtime-id" => runtime_id,
            "env" => env,
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
            rt: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            metadata: TelemetryMetadata {
                host,
                application,
                runtime_id: runtime_id.unwrap_or("unknown").to_owned(),
            },
            cfg,
        };
        Ok(s)
    }

    pub fn upload_to_telemetry(
        &self,
        crash_info: &CrashInfo,
        timeout: time::Duration,
    ) -> anyhow::Result<()> {
        let metadata = &self.metadata;

        let message = serde_json::to_string(&TelemetryCrashInfoMessage {
            additional_stacktraces: &crash_info.additional_stacktraces,
            files: &crash_info.files,
            metadata: crash_info.metadata.as_ref(),
            os_info: &crash_info.os_info,
            tags: &crash_info.tags,
        })?;

        let stack_trace = serde_json::to_string(&crash_info.stacktrace)?;
        let tags = extract_crash_info_tags(crash_info).unwrap_or_default();

        let tracer_time = match &crash_info.timestamp {
            Some(ts) => ts.timestamp() as u64,
            None => SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };

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
                stack_trace: Some(stack_trace),
                tags,
                is_sensitive: true,
                count: 1,
            }]),
        };
        let client = ddtelemetry::worker::http_client::from_config(&self.cfg);
        let req = request_builder(&self.cfg)?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                ddcommon::header::APPLICATION_JSON,
            )
            .body(serde_json::to_string(&payload)?.into())?;
        self.rt
            .block_on(async { tokio::time::timeout(timeout, client.request(req)).await })??;
        Ok(())
    }
}

fn extract_crash_info_tags(crash_info: &CrashInfo) -> anyhow::Result<String> {
    let mut tags = String::new();
    write!(&mut tags, "uuid:{}", crash_info.uuid)?;
    if let Some(siginfo) = &crash_info.siginfo {
        write!(&mut tags, ",signum:{}", siginfo.signum)?;
        if let Some(signame) = &siginfo.signame {
            write!(&mut tags, ",signame:{}", signame)?;
        }
    }
    for (counter, value) in &crash_info.counters {
        write!(&mut tags, ",{}:{}", counter, value)?;
    }
    Ok(tags)
}
#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        fs, time,
    };

    use crate::SigInfo;
    use chrono::DateTime;
    use ddcommon::{tag::Tag, Endpoint};

    use super::TelemetryCrashUploader;

    fn new_test_uploader() -> TelemetryCrashUploader {
        TelemetryCrashUploader::new(
            &new_test_prof_metadata(),
            &crate::CrashtrackerConfiguration {
                additional_files: vec![],
                create_alt_stack: true,
                endpoint: Some(Endpoint {
                    url: hyper::Uri::from_static("http://localhost:8126/profiling/v1/input"),
                    api_key: None,
                }),
                resolve_frames: crate::StacktraceCollection::WithoutSymbols,
                timeout: time::Duration::from_secs(30),
            },
        )
        .unwrap()
    }

    fn new_test_prof_metadata() -> super::CrashtrackerMetadata {
        super::CrashtrackerMetadata {
            profiling_library_name: "libdatadog".to_owned(),
            profiling_library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                Tag::new("service", "foo").unwrap(),
                Tag::new("service_version", "bar").unwrap(),
                Tag::new("runtime-id", "xyz").unwrap(),
                Tag::new("language", "native").unwrap(),
            ],
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_profiler_config_extraction() {
        let t = new_test_uploader();

        let metadata = t.metadata;
        assert_eq!(metadata.application.service_name, "foo");
        assert_eq!(metadata.application.service_version.as_deref(), Some("bar"));
        assert_eq!(metadata.application.language_name, "native");
        assert_eq!(metadata.runtime_id, "xyz");

        let cfg = t.cfg;
        assert_eq!(
            cfg.endpoint.unwrap().url.to_string(),
            "http://localhost:8126/telemetry/proxy/api/v2/apmtelemetry"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_crash_request_content() {
        let tmp = tempfile::tempdir().unwrap();
        let output_filename = {
            let mut p = tmp.into_path();
            p.push("crash_info");
            p
        };
        let mut t = new_test_uploader();

        t.cfg
            .set_url(&format!("file://{}", output_filename.to_str().unwrap()))
            .unwrap();

        let mut counters = HashMap::new();
        counters.insert("collecting_sample".to_owned(), 1);
        counters.insert("not_profiling".to_owned(), 0);
        t.upload_to_telemetry(
            &crate::CrashInfo {
                counters,
                files: HashMap::new(),
                metadata: Some(new_test_prof_metadata()),
                os_info: os_info::Info::unknown(),
                siginfo: Some(SigInfo {
                    signum: 11,
                    signame: Some("SIGSEGV".to_owned()),
                }),
                stacktrace: vec![],
                additional_stacktraces: HashMap::new(),
                tags: HashMap::new(),
                timestamp: DateTime::from_timestamp(1702465105, 0),
                uuid: uuid::uuid!("1d6b97cb-968c-40c9-af6e-e4b4d71e8781"),
                incomplete: true,
            },
            time::Duration::from_secs(1),
        )
        .unwrap();

        let payload: serde_json::value::Value =
            serde_json::de::from_str(&fs::read_to_string(&output_filename).unwrap()).unwrap();
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["application"]["service_name"], "foo");
        assert_eq!(payload["application"]["language_name"], "native");

        assert_eq!(payload["payload"].as_array().unwrap().len(), 1);
        let tags = payload["payload"][0]["tags"]
            .as_str()
            .unwrap()
            .split(',')
            .collect::<HashSet<_>>();
        assert_eq!(
            HashSet::from_iter([
                "uuid:1d6b97cb-968c-40c9-af6e-e4b4d71e8781",
                "signum:11",
                "signame:SIGSEGV",
                "collecting_sample:1",
                "not_profiling:0"
            ]),
            tags
        );
        assert_eq!(payload["payload"][0]["is_sensitive"], true);
    }
}
