// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod compressed_streaming_encoder;

pub use compressed_streaming_encoder::*;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub enum UploadCompression {
    Off,
    /// On is the default, with the exact compression algorithm being
    /// unspecified, and free to change. For example, we're testing zstd.
    #[default]
    On,
    Lz4,
    Zstd,
}
