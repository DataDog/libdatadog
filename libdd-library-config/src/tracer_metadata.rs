// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use libdd_trace_protobuf::opentelemetry::proto as otel_proto;
use std::default::Default;

/// Value of an additional OTel process-context attribute. Mirrors the small subset of
/// `opentelemetry::proto::common::v1::AnyValue` variants we support for caller-supplied threadlocal
/// extras — string and 64-bit integer, since the only consumers so far are textual schema
/// identifiers and small numeric layout constants (e.g. struct offsets, pointer widths).
#[cfg(feature = "otel-thread-ctx")]
#[derive(serde::Serialize, Debug, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ProcessContextAttrValue {
    String(String),
    Int(i64),
}

/// This struct MUST be backward compatible.
#[derive(serde::Serialize, Debug, PartialEq, Eq, Hash)]
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
    /// Ordered list of attribute key names for thread-level context records. Key indices from
    /// thread context records index into this table. Set to `None` to disable thread-level related
    /// attributes to the process-level context.
    ///
    /// If set to `Some`, the first key will be automatically set to `datadog.local_root_span_id`
    /// in the OTel process context, because the thread context handling elsewhere in libdatadog
    /// relies on this key's index to be zero. Only set additional keys in
    /// `threadlocal_attribute_keys`; the root span id is considered to always be here implicitly.
    ///
    /// This field is specific to OTel process context. It is ignored for (de)serialization, and is
    /// only used when converting to an OTel process context in
    /// [TracerMetadata::to_otel_process_ctx].
    #[cfg(feature = "otel-thread-ctx")]
    #[serde(skip)]
    pub threadlocal_attribute_keys: Option<Vec<String>>,

    /// Identifies the record schema the writer publishes (e.g. `"tlsdesc_v1_dev"` for
    /// libdatadog's own writer, `"nodejs_v1_dev"` for a Node.js writer). Defaults to
    /// `"tlsdesc_v1_dev"` when `None`.
    ///
    /// Only emitted when `threadlocal_attribute_keys` is `Some`. Ignored for (de)serialization.
    #[cfg(feature = "otel-thread-ctx")]
    #[serde(skip)]
    pub threadlocal_schema_version: Option<String>,

    /// Extra OTel process-context attributes the threadlocal writer wants to publish alongside the
    /// key map (e.g. language-runtime layout constants that the reader needs to know about). Each entry is emitted verbatim as a KeyValue.
    ///
    /// Only emitted when `threadlocal_attribute_keys` is `Some`. Ignored for (de)serialization.
    #[cfg(feature = "otel-thread-ctx")]
    #[serde(skip)]
    pub threadlocal_extra_attributes: Vec<(String, ProcessContextAttrValue)>,
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
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_attribute_keys: None,
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_schema_version: None,
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_extra_attributes: Vec::new(),
        }
    }
}

impl TracerMetadata {
    // The value of the telemetry.sdk.name field to put in the otel context resource.
    const OTEL_SDK_NAME: &str = "libdatadog";

    pub fn to_otel_process_ctx(&self) -> otel_proto::common::v1::ProcessContext {
        #[cfg(feature = "otel-thread-ctx")]
        use otel_proto::common::v1::ArrayValue;
        use otel_proto::{
            common::v1::{any_value, AnyValue, KeyValue, ProcessContext},
            resource::v1::Resource,
        };

        fn key_value(key: &'static str, val: String) -> KeyValue {
            KeyValue {
                key: key.to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(val)),
                }),
                key_ref: 0,
            }
        }

        // Even if there's no value, we still set the key to let the reader know that we do support
        // and emit this specific attribute, which happens to be empty in this case.
        fn key_value_opt(key: &'static str, val: &Option<String>) -> KeyValue {
            key_value(key, val.as_ref().cloned().unwrap_or_default())
        }

        // Every field of `self` should get propagated to the otel context.
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
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_attribute_keys,
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_schema_version,
            #[cfg(feature = "otel-thread-ctx")]
            threadlocal_extra_attributes,
        } = self;

        #[cfg_attr(not(feature = "otel-thread-ctx"), allow(unused_mut))]
        let mut attributes = vec![
            key_value_opt("service.name", service_name),
            key_value_opt("service.instance.id", runtime_id),
            key_value_opt("service.version", service_version),
            key_value_opt("deployment.environment.name", service_env),
            key_value("telemetry.sdk.language", tracer_language.clone()),
            key_value("telemetry.sdk.version", tracer_version.clone()),
            key_value("telemetry.sdk.name", Self::OTEL_SDK_NAME.to_owned()),
            key_value("host.name", hostname.clone()),
            key_value_opt("container.id", container_id),
        ];

        #[cfg(feature = "otel-thread-ctx")]
        if let Some(threadlocal_attribute_keys) = threadlocal_attribute_keys.as_ref() {
            attributes.push(key_value(
                "threadlocal.schema_version",
                threadlocal_schema_version
                    .clone()
                    .unwrap_or_else(|| "tlsdesc_v1_dev".to_owned()),
            ));

            attributes.push(KeyValue {
                key: "threadlocal.attribute_key_map".to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::ArrayValue(ArrayValue {
                        values: std::iter::once(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "datadog.local_root_span_id".to_owned(),
                            )),
                        })
                        .chain(threadlocal_attribute_keys.iter().map(|k| AnyValue {
                            value: Some(any_value::Value::StringValue(k.clone())),
                        }))
                        .collect(),
                    })),
                }),
                key_ref: 0,
            });

            for (k, v) in threadlocal_extra_attributes {
                let value = match v {
                    ProcessContextAttrValue::String(s) => any_value::Value::StringValue(s.clone()),
                    ProcessContextAttrValue::Int(i) => any_value::Value::IntValue(*i),
                };
                attributes.push(KeyValue {
                    key: k.clone(),
                    value: Some(AnyValue { value: Some(value) }),
                    key_ref: 0,
                });
            }
        }

        ProcessContext {
            resource: Some(Resource {
                attributes,
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            extra_attributes: vec![key_value_opt("datadog.process_tags", process_tags)],
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "otel-thread-ctx")]
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{AnyValue, ProcessContext};

    fn find_attr<'a>(ctx: &'a ProcessContext, key: &str) -> Option<&'a AnyValue> {
        ctx.resource
            .as_ref()?
            .attributes
            .iter()
            .find(|kv| kv.key == key)?
            .value
            .as_ref()
    }

    #[test]
    fn tracer_metadata_equality() {
        let a = TracerMetadata {
            tracer_language: "python".into(),
            ..Default::default()
        };
        let b = TracerMetadata {
            tracer_language: "python".into(),
            ..Default::default()
        };
        let c = TracerMetadata {
            tracer_language: "ruby".into(),
            ..Default::default()
        };

        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn threadlocal_attrs_absent_when_keys_empty() {
        let ctx = TracerMetadata::default().to_otel_process_ctx();

        assert!(find_attr(&ctx, "threadlocal.schema_version").is_none());
        assert!(find_attr(&ctx, "threadlocal.attribute_key_map").is_none());
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn threadlocal_attrs_present_with_correct_values() {
        let ctx = TracerMetadata {
            threadlocal_attribute_keys: Some(vec![
                "span.id".to_owned(),
                "trace.id".to_owned(),
                "custom.key".to_owned(),
            ]),
            ..Default::default()
        }
        .to_otel_process_ctx();

        // Schema version attribute
        let schema_version = find_attr(&ctx, "threadlocal.schema_version")
            .expect("threadlocal.schema_version should be present");
        assert_eq!(
            schema_version.value,
            Some(any_value::Value::StringValue("tlsdesc_v1_dev".to_owned()))
        );

        // Key map attribute: ordered array of key name strings
        let key_map = find_attr(&ctx, "threadlocal.attribute_key_map")
            .expect("threadlocal.attribute_key_map should be present");
        let array = match &key_map.value {
            Some(any_value::Value::ArrayValue(a)) => a,
            other => panic!("expected ArrayValue, got {:?}", other),
        };
        let keys: Vec<&str> = array
            .values
            .iter()
            .map(|v| match &v.value {
                Some(any_value::Value::StringValue(s)) => s.as_str(),
                other => panic!("expected StringValue, got {:?}", other),
            })
            .collect();
        assert_eq!(
            keys,
            [
                "datadog.local_root_span_id",
                "span.id",
                "trace.id",
                "custom.key"
            ]
        );
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn threadlocal_schema_version_override() {
        let ctx = TracerMetadata {
            threadlocal_attribute_keys: Some(vec![]),
            threadlocal_schema_version: Some("nodejs_v1_dev".to_owned()),
            ..Default::default()
        }
        .to_otel_process_ctx();

        let schema_version = find_attr(&ctx, "threadlocal.schema_version")
            .expect("threadlocal.schema_version should be present");
        assert_eq!(
            schema_version.value,
            Some(any_value::Value::StringValue("nodejs_v1_dev".to_owned()))
        );
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn threadlocal_extra_attributes_are_emitted() {
        let ctx = TracerMetadata {
            threadlocal_attribute_keys: Some(vec!["k".to_owned()]),
            threadlocal_extra_attributes: vec![
                (
                    "threadlocal.wrapped_object_offset".to_owned(),
                    ProcessContextAttrValue::Int(24),
                ),
                (
                    "threadlocal.tagged_size".to_owned(),
                    ProcessContextAttrValue::Int(8),
                ),
                (
                    "threadlocal.runtime.name".to_owned(),
                    ProcessContextAttrValue::String("nodejs".to_owned()),
                ),
            ],
            ..Default::default()
        }
        .to_otel_process_ctx();

        assert_eq!(
            find_attr(&ctx, "threadlocal.wrapped_object_offset").and_then(|v| v.value.clone()),
            Some(any_value::Value::IntValue(24))
        );
        assert_eq!(
            find_attr(&ctx, "threadlocal.tagged_size").and_then(|v| v.value.clone()),
            Some(any_value::Value::IntValue(8))
        );
        assert_eq!(
            find_attr(&ctx, "threadlocal.runtime.name").and_then(|v| v.value.clone()),
            Some(any_value::Value::StringValue("nodejs".to_owned()))
        );
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn threadlocal_schema_and_extras_ignored_without_key_map() {
        // Without threadlocal_attribute_keys, no threadlocal block is emitted at all — neither the
        // schema override nor any extras leak out on their own.
        let ctx = TracerMetadata {
            threadlocal_attribute_keys: None,
            threadlocal_schema_version: Some("nodejs_v1_dev".to_owned()),
            threadlocal_extra_attributes: vec![(
                "threadlocal.wrapped_object_offset".to_owned(),
                ProcessContextAttrValue::Int(24),
            )],
            ..Default::default()
        }
        .to_otel_process_ctx();

        assert!(find_attr(&ctx, "threadlocal.schema_version").is_none());
        assert!(find_attr(&ctx, "threadlocal.wrapped_object_offset").is_none());
    }
}
