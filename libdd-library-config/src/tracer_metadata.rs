// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use libdd_trace_protobuf::opentelemetry::proto as otel_proto;
use std::default::Default;

/// This struct MUST be backward compatible.
#[derive(serde::Serialize, Debug)]
pub struct TracerMetadata {
    /// Version of the schema.
    pub schema_version: u8,
    /// Runtime UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// Programming language of the tracer library (e.g., “python”). Refers to telemetry
    /// for valid values.
    pub tracer_language: String,
    /// Version of the tracer (e.g., "1.0.0").
    pub tracer_version: String,
    /// Identifier of the machine running the process.
    pub hostname: String,
    /// Name of the service being instrumented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Environment of the service being instrumented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_env: Option<String>,
    /// Version of the service being instrumented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    /// Process tags of the application being instrumented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_tags: Option<String>,
    /// Container id seen by the application.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
}

impl Default for TracerMetadata {
    fn default() -> Self {
        TracerMetadata {
            schema_version: 2,
            runtime_id: None,
            tracer_language: String::new(),
            tracer_version: String::new(),
            hostname: String::new(),
            service_name: None,
            service_env: None,
            service_version: None,
            process_tags: None,
            container_id: None,
        }
    }
}

impl TracerMetadata {
    // The value of the telemetry.sdk.name field to put in the otel context resource.
    const OTEL_SDK_NAME: &str = "libdatadog";

    pub fn to_otel_process_ctx(&self) -> otel_proto::common::v1::ProcessContext {
        use otel_proto::common::v1::{any_value, AnyValue, KeyValue};

        // Every field of `self` should gets propagated to the otel context.
        // If you add a new field, please also add it here and as a key/value in the otel context.
        let TracerMetadata {
            // This one isn't propagated on purpose
            schema_version: _,
            runtime_id,
            tracer_language,
            tracer_version,
            hostname,
            service_name,
            service_env,
            service_version,
            process_tags,
            container_id,
        } = self;

        fn key_value(key: &'static str, val: String) -> KeyValue {
            KeyValue {
                key: key.to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(val)),
                }),
                key_ref: 0,
            }
        }

        let mut attributes = vec![
            key_value("telemetry.sdk.language", tracer_language.clone()),
            key_value("telemetry.sdk.version", tracer_version.clone()),
            key_value("telemetry.sdk.name", Self::OTEL_SDK_NAME.to_owned()),
            key_value("host.name", hostname.clone()),
        ];

        let mut set_opt_attr = |key: &'static str, val: &Option<String>| {
            if let Some(val) = val {
                attributes.push(key_value(key, val.clone()))
            }
        };

        set_opt_attr("service.name", service_name);
        set_opt_attr("service.instance.id", runtime_id);
        set_opt_attr("service.version", service_version);
        set_opt_attr("deployment.environment.name", service_env);
        set_opt_attr("container.id", container_id);

        let extra_attributes: Vec<_> = process_tags
            .as_ref()
            .map(|tags| key_value("datadog.process_tags", tags.clone()))
            .into_iter()
            .collect();

        otel_proto::common::v1::ProcessContext {
            resource: Some(otel_proto::resource::v1::Resource {
                attributes,
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            extra_attributes,
        }
    }
}

pub enum AnonymousFileHandle {
    #[cfg(target_os = "linux")]
    Linux(memfd::Memfd),
    #[cfg(not(target_os = "linux"))]
    Other(()),
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::Context;
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    use std::io::Write;

    /// Create a memfd file storing the tracer metadata. This function also attempts to publish the
    /// tracer metadata as an OTel process context separately, but will ignore resulting errors.
    pub fn store_tracer_metadata(
        data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        let _ = crate::otel_process_ctx::linux::publish(&data.to_otel_process_ctx());

        let uid: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();

        let mfd_name: String = format!("datadog-tracer-info-{uid}");

        let mfd = memfd::MemfdOptions::default()
            .close_on_exec(true)
            .allow_sealing(true)
            .create::<&str>(mfd_name.as_ref())
            .context("unable to create memfd")?;

        let buf = rmp_serde::to_vec_named(data).context("failed serialization")?;
        mfd.as_file()
            .write_all(&buf)
            .context("unable to write into memfd")?;

        mfd.add_seals(&[
            memfd::FileSeal::SealShrink,
            memfd::FileSeal::SealGrow,
            memfd::FileSeal::SealSeal,
        ])
        .context("unable to seal memfd")?;

        Ok(super::AnonymousFileHandle::Linux(mfd))
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    pub fn store_tracer_metadata(
        _data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        Ok(super::AnonymousFileHandle::Other(()))
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(not(target_os = "linux"))]
pub use other::*;
