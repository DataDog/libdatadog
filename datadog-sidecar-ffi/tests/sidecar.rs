// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use datadog_sidecar_ffi::*;

macro_rules! assert_maybe_no_error {
    ($maybe_erroring:expr) => {
        match $maybe_erroring {
            libdd_common_ffi::Option::Some(err) => panic!("{}", err.to_string()),
            libdd_common_ffi::Option::None => {}
        }
    };
}

use libdd_common::Endpoint;
use std::ptr::{null, null_mut};
use std::time::Duration;
#[cfg(unix)]
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::prelude::{AsRawFd, FromRawFd},
};

fn set_sidecar_per_process() {
    std::env::set_var("_DD_DEBUG_SIDECAR_IPC_MODE", "instance_per_process")
}

/// Locate the `datadog-ipc-helper` binary for use in tests.
///
/// Resolution order:
/// 1. `DATADOG_IPC_HELPER` environment variable (explicit override)
/// 2. Sibling of the current test executable (works when the full workspace is built)
///
/// Returns `None` if the binary cannot be found; the caller should skip the test.
fn find_ipc_helper() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("DATADOG_IPC_HELPER") {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    // cargo places the test binary at target/{profile}/deps/{name}-{hash}[.exe]
    // and other workspace binaries at target/{profile}/{name}[.exe]
    let name = if cfg!(windows) {
        "datadog-ipc-helper.exe"
    } else {
        "datadog-ipc-helper"
    };
    let test_exe = std::env::current_exe().ok()?;
    let candidate = test_exe.parent()?.parent()?.join(name);
    if candidate.exists() { Some(candidate) } else { None }
}

#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore)]
#[cfg_attr(coverage_nightly, ignore)] // this fails on nightly coverage
fn test_ddog_ph_file_handling() {
    let fname = CString::new(std::env::temp_dir().join("test_file").to_str().unwrap()).unwrap();
    let mode = CString::new("a+").unwrap();

    let file = unsafe { libc::fopen(fname.as_ptr(), mode.as_ptr()) };
    let file = unsafe { ddog_ph_file_from(file) };
    let fd = file.handle.as_raw_fd();
    {
        let mut file = &*file.handle.as_filelike_view().unwrap();
        writeln!(file, "test").unwrap();
    }
    ddog_ph_file_drop(file);

    let mut file = unsafe { File::from_raw_fd(fd) };
    writeln!(file, "test").unwrap_err(); // file is closed, so write returns an error
    std::mem::forget(file); // leak to avoid debug runtime SIGABRT: "file descriptor already closed"
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_ddog_sidecar_register_app() {
    set_sidecar_per_process();

    let binary = match find_ipc_helper() {
        Some(b) => b,
        None => {
            eprintln!("Skipping test_ddog_sidecar_register_app: datadog-ipc-helper not found. \
                       Set DATADOG_IPC_HELPER or build the full workspace.");
            return;
        }
    };

    let cfg = datadog_sidecar::config::FromEnv::config();
        let mut transport = Box::new(
            datadog_sidecar::start_or_connect_with_exec_binary(binary, cfg)
                .expect("failed to start/connect to sidecar"),
        );
        transport
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        transport
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();

        unsafe {
            ddog_sidecar_session_set_config(
                &mut transport,
                "session_id".into(),
                &Endpoint {
                    url: http::Uri::from_static("http://localhost:8082/"),
                    ..Default::default()
                },
                &Endpoint::default(),
                "".into(),
                "".into(),
                "".into(),
                1000,
                1000000,
                1,
                10000000,
                10000000,
                "".into(),
                "".into(),
                null_mut(),
                null(),
                0,
                null(),
                0,
                false,
                false,
                "".into(),
            )
            .unwrap_none();

            let meta = ddog_sidecar_runtimeMeta_build(
                "language_name".into(),
                "language_version".into(),
                "tracer_version".into(),
            );

            let instance_id =
                ddog_sidecar_instanceId_build("session_id".into(), "runtime_id".into());
            let queue_id = ddog_sidecar_queueId_generate();

            ddog_sidecar_telemetry_addDependency(
                &mut transport,
                &instance_id,
                &queue_id,
                "dependency_name".into(),
                "dependency_version".into(),
            )
            .unwrap_none();

            // reset session config - and cause shutdown of all existing instances
            ddog_sidecar_session_set_config(
                &mut transport,
                "session_id".into(),
                &Endpoint {
                    url: http::Uri::from_static("http://localhost:8083/"),
                    ..Default::default()
                },
                &Endpoint::default(),
                "".into(),
                "".into(),
                "".into(),
                1000,
                1000000,
                1,
                10000000,
                10000000,
                "".into(),
                "".into(),
                null_mut(),
                null(),
                0,
                null(),
                0,
                false,
                false,
                "".into(),
            )
            .unwrap_none();

            ddog_sidecar_instanceId_drop(instance_id);
            ddog_sidecar_runtimeMeta_drop(meta);
        };

        ddog_sidecar_transport_drop(transport);
}
