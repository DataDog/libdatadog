// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod builder;
mod error_data;
mod experimental;
mod metadata;
mod os_info;
mod proc_info;
mod sig_info;
mod spans;
mod stacktrace;
mod telemetry;
mod test_utils;
mod unknown_value;

pub use builder::*;
use ddcommon::Endpoint;
pub use error_data::*;
pub use experimental::*;
pub use metadata::Metadata;
pub use os_info::*;
pub use proc_info::*;
pub use sig_info::*;
pub use spans::*;
pub use stacktrace::*;
pub use telemetry::*;

use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::File, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashInfo {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub counters: HashMap<String, i64>,
    pub data_schema_version: String,
    pub error: ErrorData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Experimental>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_messages: Vec<String>,
    pub metadata: Metadata,
    pub os_info: OsInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_info: Option<ProcInfo>, //TODO, update the schema
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_info: Option<SigInfo>, //TODO, update the schema
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_ids: Vec<Span>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<Span>,
    pub uuid: String,
}

impl CrashInfo {
    pub fn current_schema_version() -> String {
        "1.4".to_string()
    }

    pub fn demangle_names(&mut self) -> anyhow::Result<()> {
        self.error.demangle_names()
    }
}

#[cfg(unix)]
impl CrashInfo {
    pub fn normalize_ips(&mut self, pid: u32) -> anyhow::Result<()> {
        self.error.normalize_ips(pid)
    }

    pub fn resolve_names(&mut self, pid: u32) -> anyhow::Result<()> {
        self.error.resolve_names(pid)
    }
}

impl CrashInfo {
    /// Emit the CrashInfo as structured json in file `path`.
    pub fn to_file(&self, path: &Path) -> anyhow::Result<()> {
        let file = File::options()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        serde_json::to_writer_pretty(file, self)
            .with_context(|| format!("Failed to write json to {}", path.display()))?;
        Ok(())
    }

    pub fn upload_to_endpoint(&self, endpoint: &Option<Endpoint>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async { self.async_upload_to_endpoint(endpoint).await })
    }

    pub async fn async_upload_to_endpoint(
        &self,
        endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<()> {
        // If we're debugging to a file, dump the actual crashinfo into a json
        if let Some(endpoint) = endpoint {
            if Some("file") == endpoint.url.scheme_str() {
                let path = ddcommon::decode_uri_path_in_authority(&endpoint.url)
                    .context("crash output file path was not correctly formatted")?;
                self.to_file(&path)?;
            }
        }

        self.upload_to_telemetry(endpoint).await
    }

    async fn upload_to_telemetry(&self, endpoint: &Option<Endpoint>) -> anyhow::Result<()> {
        let uploader = TelemetryCrashUploader::new(&self.metadata, endpoint)?;
        uploader.upload_to_telemetry(self).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use schemars::schema::RootSchema;
    use std::fs;

    use super::*;
    #[test]
    fn test_schema_matches_rfc() {
        let rfc_schema_filename = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/RFCs/artifacts/0009-crashtracker-schema.json"
        );
        let rfc_schema_json = fs::read_to_string(rfc_schema_filename).expect("File to exist");
        let rfc_schema: RootSchema = serde_json::from_str(&rfc_schema_json).expect("Valid json");
        let schema = schemars::schema_for!(CrashInfo);

        assert_eq!(rfc_schema, schema);
        // If it doesn't match, you can use this command to generate a new schema json
        // println!("{}", serde_json::to_string_pretty(&schema).unwrap());
    }

    impl test_utils::TestInstance for CrashInfo {
        fn test_instance(seed: u64) -> Self {
            let mut counters = HashMap::new();
            counters.insert("collecting_sample".to_owned(), 1);
            counters.insert("not_profiling".to_owned(), 0);

            let span_ids = vec![
                Span {
                    id: "42".to_string(),
                    thread_name: Some("thread1".to_string()),
                },
                Span {
                    id: "24".to_string(),
                    thread_name: Some("thread2".to_string()),
                },
            ];

            let trace_ids = vec![
                Span {
                    id: "345".to_string(),
                    thread_name: Some("thread111".to_string()),
                },
                Span {
                    id: "666".to_string(),
                    thread_name: Some("thread222".to_string()),
                },
            ];

            Self {
                counters,
                data_schema_version: CrashInfo::current_schema_version(),
                error: ErrorData::test_instance(seed),
                experimental: None,
                files: HashMap::new(),
                fingerprint: None,
                incomplete: true,
                log_messages: vec![],
                metadata: Metadata::test_instance(seed),
                os_info: ::os_info::Info::unknown().into(),
                proc_info: Some(ProcInfo::test_instance(seed)),
                sig_info: Some(SigInfo::test_instance(seed)),
                span_ids,
                timestamp: chrono::DateTime::from_timestamp(1568898000 /* Datadog IPO */, 0)
                    .unwrap()
                    .to_string(),
                trace_ids,
                uuid: uuid::uuid!("1d6b97cb-968c-40c9-af6e-e4b4d71e8781").to_string(),
            }
        }
    }
}
