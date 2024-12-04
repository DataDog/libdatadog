// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use datadog_sidecar_ffi::*;

macro_rules! assert_maybe_no_error {
    ($maybe_erroring:expr) => {
        match $maybe_erroring {
            ddcommon_ffi::Option::Some(err) => panic!("{}", err.to_string()),
            ddcommon_ffi::Option::None => {}
        }
    };
}

use ddcommon_net1::Endpoint;
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
#[cfg_attr(not(windows), ignore)]
// run all tests that can fork in a separate run, to avoid any race conditions with default rust
// test harness
/// run with: RUSTFLAGS="-C prefer-dynamic" cargo test --package test_spawn_from_lib --features
/// prefer-dynamic -- --ignored
#[cfg_attr(windows, ignore = "requires -C prefer-dynamic")]
#[cfg_attr(windows, cfg(feature = "prefer_dynamic"))]
fn test_ddog_sidecar_connection() {
    set_sidecar_per_process();

    let mut transport = std::ptr::null_mut();
    assert_maybe_no_error!(ddog_sidecar_connect(&mut transport));
    let mut transport = unsafe { Box::from_raw(transport) };
    assert_maybe_no_error!(ddog_sidecar_ping(&mut transport));

    ddog_sidecar_transport_drop(transport);
}

#[test]
#[ignore = "TODO: ci-flaky can't reproduce locally"]
fn test_ddog_sidecar_register_app() {
    set_sidecar_per_process();

    let mut transport = std::ptr::null_mut();
    assert_maybe_no_error!(ddog_sidecar_connect(&mut transport));
    let mut transport = unsafe { Box::from_raw(transport) };
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
                url: hyper::Uri::from_static("http://localhost:8082/"),
                ..Default::default()
            },
            &Endpoint::default(),
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
        );

        let meta = ddog_sidecar_runtimeMeta_build(
            "language_name".into(),
            "language_version".into(),
            "tracer_version".into(),
        );

        let instance_id = ddog_sidecar_instanceId_build("session_id".into(), "runtime_id".into());
        let queue_id = ddog_sidecar_queueId_generate();

        ddog_sidecar_telemetry_addDependency(
            &mut transport,
            &instance_id,
            &queue_id,
            "dependency_name".into(),
            "dependency_version".into(),
        );

        // ddog_sidecar_telemetry_addIntegration(&mut transport, instance_id, &queue_id,
        // integration_name, integration_version) TODO add ability to add configuration

        assert_maybe_no_error!(ddog_sidecar_telemetry_flushServiceData(
            &mut transport,
            &instance_id,
            &queue_id,
            &meta,
            "service_name".into(),
            "env_name".into()
        ));
        // reset session config - and cause shutdown of all existing instances
        ddog_sidecar_session_set_config(
            &mut transport,
            "session_id".into(),
            &Endpoint {
                url: hyper::Uri::from_static("http://localhost:8083/"),
                ..Default::default()
            },
            &Endpoint::default(),
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
        );

        //TODO: Shutdown the service
        // enough case: have C api that shutsdown telemetry worker
        // ideal case : when connection socket is closed by the client the telemetry worker shuts
        // down automatically
        ddog_sidecar_instanceId_drop(instance_id);
        ddog_sidecar_runtimeMeta_drop(meta);
    };

    ddog_sidecar_transport_drop(transport);
}
