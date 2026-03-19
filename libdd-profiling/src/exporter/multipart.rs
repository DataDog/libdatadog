// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Context;

use super::profile_exporter::File;
use crate::internal::{EncodedProfile, Profile};
use crate::profiles::{Compressor, DefaultProfileCodec};

#[derive(Debug)]
pub(crate) struct PreparedMultipart {
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
}

pub(crate) fn build_multipart(
    event: &serde_json::Value,
    profile: EncodedProfile,
    additional_files: &[File<'_>],
) -> anyhow::Result<PreparedMultipart> {
    let boundary = format!(
        "------------------------{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    );

    let event_bytes = serde_json::to_vec(event)?;
    let mut body = Vec::with_capacity(
        event_bytes.len()
            + profile.buffer.len()
            + additional_files
                .iter()
                .map(|f| f.bytes.len())
                .sum::<usize>(),
    );

    append_part(
        &mut body,
        &boundary,
        "event",
        "event.json",
        Some("application/json"),
        &event_bytes,
    );

    for file in additional_files {
        let mut encoder = Compressor::<DefaultProfileCodec>::try_new(
            (file.bytes.len() >> 3).next_power_of_two(),
            10 * 1024 * 1024,
            Profile::COMPRESSION_LEVEL,
        )
        .context("failed to create compressor")?;
        encoder.write_all(file.bytes)?;
        let compressed = encoder.finish()?;

        append_part(
            &mut body,
            &boundary,
            file.name,
            file.name,
            None,
            &compressed,
        );
    }

    append_part(
        &mut body,
        &boundary,
        "profile.pprof",
        "profile.pprof",
        None,
        &profile.buffer,
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    Ok(PreparedMultipart {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body,
    })
}

fn append_part(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: &str,
    content_type: Option<&str>,
    content: &[u8],
) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    if let Some(content_type) = content_type {
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(content);
    body.extend_from_slice(b"\r\n");
}
