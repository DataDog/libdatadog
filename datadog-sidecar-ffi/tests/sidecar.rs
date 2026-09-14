// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use datadog_sidecar::service::blocking::SidecarTransport;
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
use libdd_common_ffi::{CharSlice, MaybeError};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::time::Duration;
#[cfg(unix)]
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::prelude::{AsRawFd, FromRawFd},
};

#[test]
fn generated_header_exposes_native_evp_transport_abi() {
    // Keep the Rust symbol and its exact C-facing signature under compile-time
    // test coverage independently of cbindgen's generated declaration.
    let _: unsafe extern "C" fn(
        &mut Box<SidecarTransport>,
        EvpTransportMode,
        &Endpoint,
        *const Endpoint,
        CharSlice<'_>,
        &EvpProducerIdentity<'_>,
    ) -> MaybeError = ddog_sidecar_session_set_evp_transport;

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let generated = tempfile::tempdir().unwrap();
    build_common::generate_header(
        workspace.join("libdd-common-ffi"),
        "common.h",
        generated.path().to_path_buf(),
    );
    build_common::generate_header(
        workspace.join("datadog-sidecar-ffi"),
        "sidecar.h",
        generated.path().to_path_buf(),
    );

    let include_dir = generated.path().join(build_common::HEADER_PATH);
    let common_header = include_dir.join("common.h");
    let sidecar_header = include_dir.join("sidecar.h");
    tools::headers::dedup_headers(
        common_header.to_str().unwrap(),
        &[sidecar_header.to_str().unwrap()],
    );

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(rustc).arg("-vV").output().unwrap();
    assert!(version.status.success());
    let host = String::from_utf8(version.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap()
        .to_owned();

    cc::Build::new()
        .cargo_metadata(false)
        .out_dir(generated.path())
        .host(&host)
        .target(&host)
        .opt_level(0)
        .debug(false)
        .include(include_dir)
        .file(
            workspace
                .join("datadog-sidecar-ffi")
                .join("tests/evp_transport_abi.c"),
        )
        .warnings(true)
        .warnings_into_errors(true)
        .try_compile("sidecar_evp_transport_abi")
        .unwrap();
}

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
#[cfg_attr(miri, ignore)]
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
        let process_tags = libdd_common_ffi::Vec::default();
        let agent_endpoint = Endpoint {
            url: http::Uri::from_static("http://localhost:8082/"),
            test_token: Some("agent-token".into()),
            ..Default::default()
        };
        let otlp_metrics_endpoint = Endpoint {
            url: http::Uri::from_static("http://localhost:4318/v1/metrics"),
            ..Default::default()
        };
        ddog_sidecar_session_set_config(
            &mut transport,
            "session_id".into(),
            &agent_endpoint,
            &Endpoint::default(),
            &otlp_metrics_endpoint,
            "".into(),
            "".into(),
            "".into(),
            1000,
            100,
            1000000,
            1,
            86400000,
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
            &process_tags,
            "".into(),
            "".into(),
            "".into(),
            "".into(),
        )
        .unwrap_none();

        let direct_endpoint = Endpoint {
            url: http::Uri::from_static("https://event-platform-intake.datadoghq.com/"),
            api_key: Some("test-api-key".into()),
            ..Endpoint::default()
        };

        let producer = EvpProducerIdentity {
            origin: "dd-trace-rb".into(),
            version: "3.0.0".into(),
        };

        // Bind direct credentials to the consumer's declared intake target.
        match ddog_sidecar_session_set_evp_transport(
            &mut transport,
            EvpTransportMode::PreferLocalThenDirect,
            &agent_endpoint,
            &direct_endpoint,
            "errors-intake".into(),
            &producer,
        ) {
            libdd_common_ffi::Option::Some(error) => assert!(error
                .to_string()
                .contains("host must be errors-intake.<site>")),
            libdd_common_ffi::Option::None => {
                panic!("EVP transport accepted a mismatched direct intake")
            }
        }

        // Clients stay Agent-only unless they explicitly select fallback.
        assert_maybe_no_error!(ddog_sidecar_session_set_evp_transport(
            &mut transport,
            EvpTransportMode::AgentOnly,
            &agent_endpoint,
            null(),
            "event-platform-intake".into(),
            &producer,
        ));
        assert_maybe_no_error!(ddog_sidecar_session_set_evp_transport(
            &mut transport,
            EvpTransportMode::PreferLocalThenDirect,
            &agent_endpoint,
            &direct_endpoint,
            "event-platform-intake".into(),
            &producer,
        ));

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
        )
        .unwrap_none();

        // ddog_sidecar_telemetry_addIntegration(&mut transport, instance_id, &queue_id,
        // integration_name, integration_version) TODO add ability to add configuration

        // reset session config - and cause shutdown of all existing instances
        ddog_sidecar_session_set_config(
            &mut transport,
            "session_id".into(),
            &Endpoint {
                url: http::Uri::from_static("http://localhost:8083/"),
                ..Default::default()
            },
            &Endpoint::default(),
            null(),
            "".into(),
            "".into(),
            "".into(),
            1000,
            100,
            1000000,
            1,
            86400000,
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
            &process_tags,
            "".into(),
            "".into(),
            "".into(),
            "".into(),
        )
        .unwrap_none();

        //TODO: Shutdown the service
        // enough case: have C api that shutsdown telemetry worker
        // ideal case : when connection socket is closed by the client the telemetry worker shuts
        // down automatically
        ddog_sidecar_instanceId_drop(instance_id);
        ddog_sidecar_runtimeMeta_drop(meta);
    };

    ddog_sidecar_transport_drop(transport);
}
