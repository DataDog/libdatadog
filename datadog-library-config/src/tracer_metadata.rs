// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::default::Default;

const MAX_SIZE: usize = 64 * 1024; //< 64KiB

/// This struct MUST be backward compatible.
#[derive(serde::Serialize, Debug)]
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
        }
    }
}

pub enum AnonymousFileHandle {
    #[cfg(target_os = "linux")]
    Linux(memfd::Memfd),
    #[cfg(target_os = "windows")]
    Windows(windows::AnonymousFile),
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    Other(()),
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{anyhow, Context};
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    use std::io::Write;

    /// Create a memfd file storing the tracer metadata.
    pub fn store_tracer_metadata(
        data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        let buf = rmp_serde::to_vec_named(data).context("failed serialization")?;
        if buf.len() > super::MAX_SIZE {
            return Err(anyhow!(
                "serialized tracer configuration exceeds {} limit",
                super::MAX_SIZE
            ));
        }

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

#[cfg(target_os = "windows")]
mod windows {
    use anyhow::{anyhow, Context};
    use std::ffi::CString;
    use std::{process, ptr};
    use windows::core::PSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Memory::{
        CreateFileMappingA, MapViewOfFile, UnmapViewOfFile, VirtualProtect, FILE_MAP_WRITE,
        MEMORY_MAPPED_VIEW_ADDRESS, PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE,
    };

    pub struct AnonymousFile {
        handle: HANDLE,
        addr: MEMORY_MAPPED_VIEW_ADDRESS,
    }

    impl Drop for AnonymousFile {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(self.addr);
                let _ = CloseHandle(self.handle);
            }
        }
    }

    pub fn store_tracer_metadata(
        data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        let buf = rmp_serde::to_vec_named(data).context("failed serialization")?;
        if buf.len() > super::MAX_SIZE {
            return Err(anyhow!(
                "serialized tracer configuration exceeds {} limit",
                super::MAX_SIZE
            ));
        }

        let pid = process::id();
        let anon_file_id =
            CString::new(format!("datadog-tracer-info-{pid}")).expect("CString::new failed");

        unsafe {
            let handle = CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                super::MAX_SIZE.try_into().unwrap(),
                PSTR(anon_file_id.as_ptr() as *mut u8),
            )
            .context("failed to create a memory file mapping")?;

            let addr = MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, buf.len());
            if addr.Value.is_null() {
                return Err(anyhow!("failed to map a view of {:?}", anon_file_id));
            }

            // EEEEEEEEEHHHH can this be outside of `unsafe`?
            let dest = addr.Value as *mut u8;
            ptr::copy_nonoverlapping(buf.as_ptr(), dest, buf.len());
            *dest.add(buf.len()) = 0; // Add null-terminate char

            let mut out_flag = PAGE_PROTECTION_FLAGS(0);
            let _ = VirtualProtect(addr.Value, super::MAX_SIZE, PAGE_READONLY, &mut out_flag)
                .context("failed to seal memory file");

            Ok(super::AnonymousFileHandle::Windows(AnonymousFile {
                handle,
                addr,
            }))
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod other {
    pub fn store_tracer_metadata(
        _data: &super::TracerMetadata,
    ) -> anyhow::Result<super::AnonymousFileHandle> {
        Ok(super::AnonymousFileHandle::Other(()))
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub use other::*;
#[cfg(target_os = "windows")]
pub use windows::*;
