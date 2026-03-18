// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_profiling_protobuf::prost_impls::{Profile, Sample};

fn decompress(encoded: &[u8]) -> anyhow::Result<Vec<u8>> {
    // The zstd bindings use FFI so they don't work under miri. This means the
    // buffer isn't compressed, so simply convert to a vec.
    #[cfg(miri)]
    let buf = encoded.to_vec();
    #[cfg(not(miri))]
    let buf = {
        use anyhow::Context;
        use std::io::{Cursor, Read};
        let mut decoder =
            zstd::Decoder::new(Cursor::new(encoded)).context("failed to create zstd decoder")?;
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        out
    };
    Ok(buf)
}

fn deserialize_compressed_pprof(encoded: &[u8]) -> anyhow::Result<Profile> {
    use prost::Message;
    let buf = decompress(encoded)?;
    let profile = Profile::decode(buf.as_slice())?;
    Ok(profile)
}

pub fn roundtrip_to_pprof(profile: crate::internal::Profile) -> anyhow::Result<Profile> {
    let encoded = profile.serialize_into_compressed_pprof(None, None)?;
    deserialize_compressed_pprof(&encoded.buffer)
}

#[cfg(feature = "otel")]
pub fn roundtrip_to_otel(
    profile: crate::internal::Profile,
) -> anyhow::Result<crate::otel::ProfilesData> {
    use prost::Message;
    let encoded = profile.serialize_into_compressed_otel(None, None)?;
    let buf = decompress(&encoded.buffer)?;
    let data = crate::otel::ProfilesData::decode(buf.as_slice())?;
    Ok(data)
}

#[cfg(feature = "otel")]
#[track_caller]
pub fn otel_string_table_fetch(dict: &crate::otel::ProfilesDictionary, id: i32) -> &str {
    dict.string_table
        .get(id as usize)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("String {id} not found"))
}

#[cfg(feature = "otel")]
#[track_caller]
pub fn otel_attribute_key_value<'a>(
    dict: &'a crate::otel::ProfilesDictionary,
    attr: &crate::otel::KeyValueAndUnit,
) -> (&'a str, i64) {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let key = otel_string_table_fetch(dict, attr.key_strindex);
    let value = attr
        .value
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .map(|v| match v {
            Value::IntValue(n) => *n,
            _ => 0,
        })
        .unwrap_or(0);
    (key, value)
}

#[cfg(feature = "otel")]
fn otel_sample_attr_by_key<'a>(
    dict: &'a crate::otel::ProfilesDictionary,
    sample: &crate::otel::Sample,
    key: &str,
) -> Option<&'a crate::otel::KeyValueAndUnit> {
    for &idx in &sample.attribute_indices {
        let attr = dict.attribute_table.get(idx as usize)?;
        if otel_string_table_fetch(dict, attr.key_strindex) == key {
            return Some(attr);
        }
    }
    None
}

#[cfg(feature = "otel")]
pub fn otel_sample_attr_int(
    dict: &crate::otel::ProfilesDictionary,
    sample: &crate::otel::Sample,
    key: &str,
) -> Option<i64> {
    let attr = otel_sample_attr_by_key(dict, sample, key)?;
    let (_, value) = otel_attribute_key_value(dict, attr);
    Some(value)
}

#[cfg(feature = "otel")]
pub fn otel_sample_attr_str<'a>(
    dict: &'a crate::otel::ProfilesDictionary,
    sample: &crate::otel::Sample,
    key: &str,
) -> Option<&'a str> {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let attr = otel_sample_attr_by_key(dict, sample, key)?;
    let value = attr.value.as_ref()?.value.as_ref()?;
    match value {
        Value::StringValue(s) => Some(s.as_str()),
        _ => None,
    }
}

pub fn sorted_samples(profile: &Profile) -> Vec<Sample> {
    let mut samples = profile.samples.clone();
    samples.sort_unstable();
    samples
}

#[track_caller]
pub fn string_table_fetch(profile: &Profile, id: i64) -> &String {
    profile
        .string_table
        .get(id as usize)
        .unwrap_or_else(|| panic!("String {id} not found"))
}

#[track_caller]
pub fn string_table_fetch_owned(profile: &Profile, id: i64) -> Box<str> {
    string_table_fetch(profile, id).clone().into_boxed_str()
}
