// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use datadog_profiling_protobuf::prost_impls::{Profile, Sample};
use std::io::Cursor;

pub fn deserialize_compressed_pprof(encoded: &[u8]) -> anyhow::Result<Profile> {
    use prost::Message;
    use std::io::Read;

    let mut decoder =
        zstd::Decoder::new(Cursor::new(encoded)).context("failed to create zstd decoder")?;
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    let profile = Profile::decode(buf.as_slice())?;
    Ok(profile)
}

pub fn roundtrip_to_pprof(profile: crate::internal::Profile) -> anyhow::Result<Profile> {
    let encoded = profile.serialize_into_compressed_pprof(None, None)?;
    deserialize_compressed_pprof(&encoded.buffer)
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
