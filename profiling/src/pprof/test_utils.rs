// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub fn deserialize_compressed_pprof(encoded: &[u8]) -> anyhow::Result<super::Profile> {
    use prost::Message;
    use std::io::Read;

    let mut decoder = lz4_flex::frame::FrameDecoder::new(encoded);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    let profile = super::Profile::decode(buf.as_slice())?;
    Ok(profile)
}

pub fn roundtrip_to_pprof(profile: crate::internal::Profile) -> anyhow::Result<super::Profile> {
    let encoded = profile.serialize_into_compressed_pprof(None, None)?;
    deserialize_compressed_pprof(&encoded.buffer)
}
