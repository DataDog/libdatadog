// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// This struct MUST be backward compatible.
#[derive(serde::Serialize, Debug)]
#[repr(C)]
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

    /// Create a memfd file storing the tracer metadata.
    pub fn store_tracer_metadata(
        data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
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
