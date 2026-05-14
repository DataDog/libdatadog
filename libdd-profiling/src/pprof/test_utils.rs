// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_profiling_protobuf::prost_impls::{Profile, Sample};
use std::time::{Duration, SystemTime};

fn deserialize_compressed_pprof(encoded: &[u8]) -> anyhow::Result<Profile> {
    use prost::Message;

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
    let profile = Profile::decode(buf.as_slice())?;
    Ok(profile)
}

pub fn roundtrip_to_pprof(profile: crate::internal::Profile) -> anyhow::Result<Profile> {
    roundtrip_to_pprof_with_times(profile, None, None)
}

pub fn roundtrip_to_pprof_with_times(
    profile: crate::internal::Profile,
    end_time: Option<SystemTime>,
    duration: Option<Duration>,
) -> anyhow::Result<Profile> {
    let encoded = profile.serialize_into_compressed_pprof(end_time, duration)?;
    deserialize_compressed_pprof(&encoded.buffer)
}

pub fn roundtrip_to_pprof2(
    mut profile: crate::internal::Profile,
    start_time: Option<SystemTime>,
    end_time: Option<SystemTime>,
    duration: Option<Duration>,
) -> anyhow::Result<Profile> {
    let encoded = profile.serialize_into_compressed_pprof2(start_time, end_time, duration)?;
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
