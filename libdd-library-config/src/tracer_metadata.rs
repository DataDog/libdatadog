// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Tracer metadata and serialization available with `core` and `alloc`.
//!
//! Linux discovery uses rustix for syscalls and the shared startup shadow environment:
//!
//! ```
//! use libdd_library_config::tracer_metadata::{
//!     TracerMetadata,
//!     discovery::{self, EntityInfo},
//! };
//!
//! # #[cfg(target_os = "linux")]
//! fn collect() -> anyhow::Result<(alloc::vec::Vec<u8>, EntityInfo)> {
//!     let origin = discovery::discover_origin();
//!     let metadata = TracerMetadata {
//!         tracer_language: "c".into(),
//!         hostname: discovery::read_hostname()?,
//!         container_id: origin.container_id.clone(),
//!         ..Default::default()
//!     };
//!     Ok((metadata.to_msgpack()?, origin))
//! }
//! # extern crate alloc;
//! ```
//!
//! The returned origin metadata provides the container, entity, and external-environment headers
//! through [`discovery::EntityInfo::headers`]. Origin headers are separate from the legacy
//! metadata MessagePack schema.

use alloc::string::String;
use alloc::vec::Vec;
#[cfg(feature = "std")]
use alloc::{borrow::ToOwned, vec};
use core::{convert::Infallible, num::TryFromIntError};
#[cfg(feature = "std")]
use libdd_trace_protobuf::opentelemetry::proto as otel_proto;

pub mod discovery;

/// Thread-level context metadata the tracer wants to publish as part of the OTel process context.
/// Set [TracerMetadata::threadlocal_metadata] to `Some(_)` to enable the threadlocal section; the
/// fields inside then drive its contents.
#[cfg(feature = "otel-thread-ctx")]
#[derive(Debug, PartialEq, Default)]
pub struct ThreadLocalMetadata {
    /// Ordered list of attribute key names for thread-level context records. Key indices from
    /// thread context records index into this table.
    ///
    /// The first key is automatically set to `datadog.local_root_span_id` in the OTel process
    /// context, because the thread context handling elsewhere in libdatadog relies on this key's
    /// index to be zero. Only set additional keys here; the root span id is considered to always
    /// be present implicitly.
    pub attribute_keys: Vec<String>,

    /// Identifies the record schema the writer publishes (e.g. `"tlsdesc_v1_dev"` for
    /// libdatadog's own writer, `"nodejs_v1_dev"` for a Node.js writer). Defaults to
    /// `"tlsdesc_v1_dev"` when `None`.
    pub schema_version: Option<String>,

    /// Extra OTel process-context attributes the threadlocal writer wants to publish alongside the
    /// key map (e.g. language-runtime layout constants that the reader needs to know about). Each
    /// entry is emitted verbatim as a KeyValue.
    pub extra_attributes: Vec<(String, otel_proto::common::v1::any_value::Value)>,
}

/// Tracer metadata supplied by the caller, available with or without `std`.
///
/// Constructing metadata does not read the environment or discover host information.
/// This struct MUST be backward compatible.
#[derive(serde::Serialize, Debug, PartialEq)]
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
    /// Thread-level context metadata; emit a `threadlocal.*` block in the OTel process context
    /// when `Some`. `None` disables the section entirely. See [ThreadLocalMetadata]. Ignored for
    /// (de)serialization; only used when converting to an OTel process context in
    /// `TracerMetadata::to_otel_process_ctx` (available with `std`).
    #[cfg(feature = "otel-thread-ctx")]
    #[serde(skip)]
    pub threadlocal_metadata: Option<ThreadLocalMetadata>,
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
            threadlocal_metadata: None,
        }
    }
}

impl TracerMetadata {
    /// Encode the legacy tracer metadata MessagePack map without publishing it.
    ///
    /// Available without `std`; the caller owns the returned bytes and decides how to store
    /// them. The schema and field order match `rmp_serde::to_vec_named(self)`, including
    /// omission of absent optional fields. Thread-context metadata is not part of this format.
    ///
    /// Returns an error if a string's byte length exceeds MessagePack's `u32` limit.
    pub fn to_msgpack(&self) -> Result<Vec<u8>, TryFromIntError> {
        // Keep this exhaustive so new schema fields must be considered by the encoder.
        let Self {
            schema_version,
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
                threadlocal_metadata: _,
        } = self;
        let fields = [
            ("runtime_id", runtime_id.as_deref()),
            ("tracer_language", Some(tracer_language.as_str())),
            ("tracer_version", Some(tracer_version.as_str())),
            ("hostname", Some(hostname.as_str())),
            ("service_name", service_name.as_deref()),
            ("service_env", service_env.as_deref()),
            ("service_version", service_version.as_deref()),
            ("process_tags", process_tags.as_deref()),
            ("container_id", container_id.as_deref()),
        ];
        let count = u32::try_from(1 + fields.iter().filter(|(_, value)| value.is_some()).count())?;
        let mut buf = rmp::encode::ByteBuf::new();
        infallible_write(rmp::encode::write_map_len(&mut buf, count));
        write_string(&mut buf, "schema_version")?;
        infallible_write(rmp::encode::write_uint(
            &mut buf,
            u64::from(*schema_version),
        ));
        for (key, value) in fields {
            if let Some(value) = value {
                write_string(&mut buf, key)?;
                write_string(&mut buf, value)?;
            }
        }
        Ok(buf.into_vec())
    }
}

fn write_string(buf: &mut rmp::encode::ByteBuf, value: &str) -> Result<(), TryFromIntError> {
    let len = u32::try_from(value.len())?;
    infallible_write(rmp::encode::write_str_len(buf, len));
    buf.as_mut_vec().extend_from_slice(value.as_bytes());
    Ok(())
}

fn infallible_write<T>(result: Result<T, rmp::encode::ValueWriteError<Infallible>>) -> T {
    match result {
        Ok(value) => value,
        Err(
            rmp::encode::ValueWriteError::InvalidMarkerWrite(error)
            | rmp::encode::ValueWriteError::InvalidDataWrite(error),
        ) => match error {},
    }
}

#[cfg(feature = "std")]
impl TracerMetadata {
    // The value of the telemetry.sdk.name field to put in the otel context resource.
    const OTEL_SDK_NAME: &str = "libdatadog";

    pub fn to_otel_process_ctx(&self) -> otel_proto::common::v1::ProcessContext {
        #[cfg(feature = "otel-thread-ctx")]
        use otel_proto::common::v1::ArrayValue;
        use otel_proto::{
            common::v1::{AnyValue, KeyValue, ProcessContext, any_value},
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

        fn key_value_if_some(key: &'static str, val: &Option<String>) -> Option<KeyValue> {
            val.as_ref().map(|val| key_value(key, val.clone()))
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
            threadlocal_metadata,
        } = self;

        let resource_attrs = [
            key_value_if_some("service.name", service_name),
            key_value_if_some("service.instance.id", runtime_id),
            key_value_if_some("service.version", service_version),
            key_value_if_some("deployment.environment.name", service_env),
            Some(key_value("telemetry.sdk.language", tracer_language.clone())),
            Some(key_value("telemetry.sdk.version", tracer_version.clone())),
            Some(key_value(
                "telemetry.sdk.name",
                Self::OTEL_SDK_NAME.to_owned(),
            )),
            Some(key_value("host.name", hostname.clone())),
            key_value_if_some("container.id", container_id),
        ]
        .into_iter()
        .flatten()
        .collect();

        // `mut` is only needed when `otel-thread-ctx` is enabled.
        #[allow(unused_mut)]
        let mut extra_attributes: Vec<_> = key_value_if_some("datadog.process_tags", process_tags)
            .into_iter()
            .collect();

        #[cfg(feature = "otel-thread-ctx")]
        if let Some(ThreadLocalMetadata {
            attribute_keys,
            schema_version,
            extra_attributes: threadlocal_extras,
        }) = threadlocal_metadata.as_ref()
        {
            extra_attributes.push(key_value(
                "threadlocal.schema_version",
                schema_version
                    .clone()
                    .unwrap_or_else(|| "tlsdesc_v1_dev".to_owned()),
            ));

            extra_attributes.push(KeyValue {
                key: "threadlocal.attribute_key_map".to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::ArrayValue(ArrayValue {
                        values: core::iter::once(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "datadog.local_root_span_id".to_owned(),
                            )),
                        })
                        .chain(attribute_keys.iter().map(|k| AnyValue {
                            value: Some(any_value::Value::StringValue(k.clone())),
                        }))
                        .collect(),
                    })),
                }),
                key_ref: 0,
            });

            extra_attributes.extend(threadlocal_extras.iter().map(|(k, v)| KeyValue {
                key: k.clone(),
                value: Some(AnyValue {
                    value: Some(v.clone()),
                }),
                key_ref: 0,
            }));
        }

        ProcessContext {
            resource: Some(Resource {
                attributes: resource_attrs,
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            extra_attributes,
        }
    }
}

#[cfg(feature = "std")]
pub enum AnonymousFileHandle {
    #[cfg(target_os = "linux")]
    Linux(memfd::Memfd),
    #[cfg(not(target_os = "linux"))]
    Other(()),
}

#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    use anyhow::Context;
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    use std::io::Write;

    /// Create a memfd file storing the tracer metadata. This function also attempts to publish the
    /// tracer metadata as an OTel process context separately, but will ignore resulting errors.
    pub fn store_tracer_metadata(
        data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        #[cfg(feature = "process-context-writer")]
        let _ = crate::otel_process_ctx::publish(&data.to_otel_process_ctx());

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

        let buf = data.to_msgpack().context("failed serialization")?;
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

#[cfg(all(feature = "std", not(target_os = "linux")))]
mod other {
    pub fn store_tracer_metadata(
        _data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        Ok(super::AnonymousFileHandle::Other(()))
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
pub use linux::*;
#[cfg(all(feature = "std", not(target_os = "linux")))]
pub use other::*;

#[cfg(test)]
mod serialization_tests {
    use super::*;
    use alloc::collections::BTreeMap;

    #[test]
    fn default_metadata_preserves_schema_and_omits_absent_fields() {
        let serialized = yaml_serde::to_string(&TracerMetadata::default()).unwrap();
        let fields: BTreeMap<String, String> = yaml_serde::from_str(&serialized).unwrap();

        assert_eq!(fields.len(), 4);
        assert_eq!(fields["schema_version"], "2");
        assert_eq!(fields["tracer_language"], "");
        assert_eq!(fields["tracer_version"], "");
        assert_eq!(fields["hostname"], "");
        assert_eq!(
            TracerMetadata::default().to_msgpack().unwrap(),
            b"\x84\xaeschema_version\x02\xaftracer_language\xa0\xaetracer_version\xa0\xa8hostname\xa0"
        );
    }

    #[test]
    fn caller_supplied_metadata_is_serializable_without_discovery() {
        let metadata = TracerMetadata {
            runtime_id: Some("runtime-id".into()),
            tracer_language: "c".into(),
            tracer_version: "1.2.3".into(),
            hostname: "host".into(),
            service_name: Some("checkout".into()),
            service_env: Some("production".into()),
            service_version: Some("4.5.6".into()),
            process_tags: Some("region:us-east-1".into()),
            container_id: Some("container-id".into()),
            ..Default::default()
        };
        let serialized = yaml_serde::to_string(&metadata).unwrap();
        let fields: BTreeMap<String, String> = yaml_serde::from_str(&serialized).unwrap();

        assert_eq!(fields.len(), 10);
        for (key, value) in [
            ("schema_version", "2"),
            ("runtime_id", "runtime-id"),
            ("tracer_language", "c"),
            ("tracer_version", "1.2.3"),
            ("hostname", "host"),
            ("service_name", "checkout"),
            ("service_env", "production"),
            ("service_version", "4.5.6"),
            ("process_tags", "region:us-east-1"),
            ("container_id", "container-id"),
        ] {
            assert_eq!(fields[key], value, "{key}");
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn msgpack_matches_serde_for_all_optional_field_combinations() {
        for mask in 0u8..64 {
            let metadata = TracerMetadata {
                runtime_id: (mask & 1 != 0).then(|| "runtime-id".into()),
                tracer_language: "c".into(),
                tracer_version: "1.2.3".into(),
                hostname: "host".into(),
                service_name: (mask & 2 != 0).then(|| "checkout".into()),
                service_env: (mask & 4 != 0).then(|| "production".into()),
                service_version: (mask & 8 != 0).then(|| "4.5.6".into()),
                process_tags: (mask & 16 != 0).then(|| "region:us-east-1".into()),
                container_id: (mask & 32 != 0).then(String::new),
                ..Default::default()
            };
            assert_eq!(
                metadata.to_msgpack().unwrap(),
                rmp_serde::to_vec_named(&metadata).unwrap(),
                "optional field mask: {mask}"
            );
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn msgpack_matches_serde_across_string_length_boundaries() {
        for length in [0, 1, 31, 32, 255, 256, 65535, 65536] {
            let metadata = TracerMetadata {
                hostname: "h".repeat(length),
                service_name: Some("サービス\0".into()),
                schema_version: 255,
                ..Default::default()
            };
            assert_eq!(
                metadata.to_msgpack().unwrap(),
                rmp_serde::to_vec_named(&metadata).unwrap(),
                "string length: {length}"
            );
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{AnyValue, ProcessContext};

    fn find_extra_attr<'a>(ctx: &'a ProcessContext, key: &str) -> Option<&'a AnyValue> {
        ctx.extra_attributes
            .iter()
            .find(|kv| kv.key == key)?
            .value
            .as_ref()
    }

    fn find_resource_attr<'a>(ctx: &'a ProcessContext, key: &str) -> Option<&'a AnyValue> {
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
    fn absent_optional_metadata_is_omitted_from_process_context() {
        let ctx = TracerMetadata::default().to_otel_process_ctx();

        for key in [
            "service.name",
            "service.instance.id",
            "service.version",
            "deployment.environment.name",
            "container.id",
        ] {
            assert!(find_resource_attr(&ctx, key).is_none(), "unexpected {key}");
        }
        assert!(find_extra_attr(&ctx, "datadog.process_tags").is_none());
        assert!(find_extra_attr(&ctx, "threadlocal.schema_version").is_none());
        assert!(find_extra_attr(&ctx, "threadlocal.attribute_key_map").is_none());
    }

    #[test]
    fn present_optional_metadata_is_emitted_in_process_context() {
        let ctx = TracerMetadata {
            runtime_id: Some("runtime-id".to_owned()),
            service_name: Some("checkout".to_owned()),
            service_env: Some("production".to_owned()),
            service_version: Some("1.2.3".to_owned()),
            process_tags: Some("region:us-east-1".to_owned()),
            container_id: Some("container-id".to_owned()),
            ..Default::default()
        }
        .to_otel_process_ctx();

        for (key, expected) in [
            ("service.name", "checkout"),
            ("service.instance.id", "runtime-id"),
            ("service.version", "1.2.3"),
            ("deployment.environment.name", "production"),
            ("container.id", "container-id"),
        ] {
            assert_eq!(
                find_resource_attr(&ctx, key).and_then(|value| value.value.as_ref()),
                Some(&any_value::Value::StringValue(expected.to_owned()))
            );
        }
        assert_eq!(
            find_extra_attr(&ctx, "datadog.process_tags").and_then(|value| value.value.as_ref()),
            Some(&any_value::Value::StringValue(
                "region:us-east-1".to_owned()
            ))
        );
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn threadlocal_attrs_present_with_correct_values() {
        let ctx = TracerMetadata {
            threadlocal_metadata: Some(ThreadLocalMetadata {
                attribute_keys: vec![
                    "span.id".to_owned(),
                    "trace.id".to_owned(),
                    "custom.key".to_owned(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }
        .to_otel_process_ctx();

        // Schema version attribute
        let schema_version = find_extra_attr(&ctx, "threadlocal.schema_version")
            .expect("threadlocal.schema_version should be present");
        assert_eq!(
            schema_version.value,
            Some(any_value::Value::StringValue("tlsdesc_v1_dev".to_owned()))
        );

        // Key map attribute: ordered array of key name strings
        let key_map = find_extra_attr(&ctx, "threadlocal.attribute_key_map")
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
            threadlocal_metadata: Some(ThreadLocalMetadata {
                schema_version: Some("nodejs_v1_dev".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        }
        .to_otel_process_ctx();

        let schema_version = find_extra_attr(&ctx, "threadlocal.schema_version")
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
            threadlocal_metadata: Some(ThreadLocalMetadata {
                attribute_keys: vec!["k".to_owned()],
                extra_attributes: vec![
                    (
                        "threadlocal.wrapped_object_offset".to_owned(),
                        any_value::Value::IntValue(24),
                    ),
                    (
                        "threadlocal.tagged_size".to_owned(),
                        any_value::Value::IntValue(8),
                    ),
                    (
                        "threadlocal.runtime.name".to_owned(),
                        any_value::Value::StringValue("nodejs".to_owned()),
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }
        .to_otel_process_ctx();

        assert_eq!(
            find_extra_attr(&ctx, "threadlocal.wrapped_object_offset")
                .and_then(|v| v.value.clone()),
            Some(any_value::Value::IntValue(24))
        );
        assert_eq!(
            find_extra_attr(&ctx, "threadlocal.tagged_size").and_then(|v| v.value.clone()),
            Some(any_value::Value::IntValue(8))
        );
        assert_eq!(
            find_extra_attr(&ctx, "threadlocal.runtime.name").and_then(|v| v.value.clone()),
            Some(any_value::Value::StringValue("nodejs".to_owned()))
        );
    }

    #[cfg(feature = "otel-thread-ctx")]
    #[test]
    fn default_threadlocal_metadata_preserves_legacy_metadata() {
        let mut metadata = TracerMetadata {
            service_name: Some("checkout".to_owned()),
            ..Default::default()
        };

        let legacy_before = rmp_serde::to_vec_named(&metadata).expect("metadata should serialize");

        metadata.threadlocal_metadata = Some(ThreadLocalMetadata::default());

        let ctx = metadata.to_otel_process_ctx();

        for key in [
            "threadlocal.schema_version",
            "threadlocal.attribute_key_map",
        ] {
            let count = ctx
                .extra_attributes
                .iter()
                .filter(|attribute| attribute.key == key)
                .count();

            assert_eq!(count, 1, "expected exactly one {key}");
        }

        let schema = find_extra_attr(&ctx, "threadlocal.schema_version")
            .expect("schema version should be present");

        assert_eq!(
            schema.value,
            Some(any_value::Value::StringValue("tlsdesc_v1_dev".to_owned()))
        );

        let key_map = find_extra_attr(&ctx, "threadlocal.attribute_key_map")
            .expect("attribute key map should be present");

        let array = match &key_map.value {
            Some(any_value::Value::ArrayValue(array)) => array,
            other => panic!("expected ArrayValue, got {other:?}"),
        };

        assert_eq!(array.values.len(), 1);
        assert_eq!(
            array.values[0].value,
            Some(any_value::Value::StringValue(
                "datadog.local_root_span_id".to_owned()
            ))
        );

        let legacy_after = rmp_serde::to_vec_named(&metadata).expect("metadata should serialize");

        assert_eq!(legacy_before, legacy_after);
    }
}
