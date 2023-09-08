// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod proto;
pub mod sliced_proto;

use lz4_flex::frame::FrameDecoder;
use prost::Message;
pub use proto::*;
use std::io::Read;

pub fn deserialize_zipped_pprof(encoded: &[u8]) -> anyhow::Result<proto::Profile> {
    let mut decoder = FrameDecoder::new(encoded);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    let profile = Profile::decode(buf.as_slice())?;
    Ok(profile)
}

pub fn roundtrip_to_pprof(profile: super::Profile) -> anyhow::Result<proto::Profile> {
    let encoded = profile.serialize(None, None)?;
    deserialize_zipped_pprof(&encoded.buffer)
}
