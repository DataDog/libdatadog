// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_os = "linux")]
use memfd::{Memfd, MemfdOptions};
use serde::Serialize;

/// This struct MUST be backward compatible.
#[derive(Serialize, Debug)]
#[repr(C)]
pub struct TracerMetadata {
    /// Version of the schema.
    pub schema_version: u8,
    /// Runtime UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// Programming language of the tracer library (e.g., “python”). Refers to telemetry        for valid values.
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
    Linux(Box<Memfd>),
    #[cfg(not(target_os = "linux"))]
    Other(()),
}

#[cfg(target_os = "linux")]
mod linux {
  use serde::ser::Serialize;
  use rand::Rng;
  use log::warn;
  use std::io::Write;
  use rmp_serde::Serializer;
  use rand::distributions::Alphanumeric;

  /// Create a memfd file storing the tracer metadata.
  pub fn store_tracer_metadata(data: &super::TracerMetadata) -> Result<super::AnonymousFileHandle, String> {
    let uid: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();

    let mfd_name: String = format!("datadog-tracer-info-{}", uid);

    let mfd = super::MemfdOptions::default()
        .close_on_exec(true)
        .allow_sealing(true)
        .create::<&str>(mfd_name.as_ref())
        .map_err(|e| format!("unable to create memfd: {}", e))?;

    let mut buf = Vec::new();
    data.serialize(&mut Serializer::new(&mut buf).with_struct_map()).unwrap();

    mfd.as_file().write_all(&buf)
        .map_err(|e| format!("unable to write into memfd: {}", e))?;

    mfd.add_seals(&[
        memfd::FileSeal::SealShrink,
        memfd::FileSeal::SealGrow,
        memfd::FileSeal::SealSeal,
    ])
    .unwrap_or_else(|e| warn!("unable to seal: {}", e));

    Ok(super::AnonymousFileHandle::Linux(Box::new(mfd)))
  }
}

#[cfg(not(target_os = "linux"))]
mod other {
  pub fn store_tracer_metadata(_data: &super::TracerMetadata) -> Result<super::AnonymousFileHandle, String> {
    Ok(super::AnonymousFileHandle::Other(()))
  }
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(not(target_os = "linux"))]
pub use other::*;
