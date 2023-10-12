// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]
use datadog_sidecar_ffi::unix::*;

macro_rules! assert_maybe_no_error {
    ($maybe_erroring:expr) => {
        match $maybe_erroring {
            ddcommon_ffi::Option::Some(err) => panic!(
                "{}",
                String::from_utf8_lossy(
                    #[allow(unused_unsafe)]
                    unsafe {
                        err.as_slice().into_slice()
                    }
                )
            ),
            ddcommon_ffi::Option::None => {}
        }
    };
}

use ddcommon::Endpoint;
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::prelude::{AsRawFd, FromRawFd},
    time::Duration,
};

fn set_sidecar_per_process() {
    std::env::set_var("_DD_DEBUG_SIDECAR_IPC_MODE", "instance_per_process")
}

#[test]
#[cfg_attr(miri, ignore)]
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
}

#[test]
#[ignore] // run all tests that can fork in a separate run, to avoid any race conditions with default rust test harness
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
                api_key: None,
                url: hyper::Uri::from_static("http://localhost:8082/"),
            },
            1000,
            1000000,
            10000000,
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

        // ddog_sidecar_telemetry_addIntegration(&mut transport, instance_id, &queue_id, integration_name, integration_version)
        // TODO add ability to add configuration

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
                api_key: None,
                url: hyper::Uri::from_static("http://localhost:8083/"),
            },
            1000,
            1000000,
            10000000,
        );

        //TODO: Shutdown the service
        // enough case: have C api that shutsdown telemetry worker
        // ideal case : when connection socket is closed by the client the telemetry worker shuts down automatically
        ddog_sidecar_instanceId_drop(instance_id);
        ddog_sidecar_runtimeMeta_drop(meta);
    };

    ddog_sidecar_transport_drop(transport);
}
