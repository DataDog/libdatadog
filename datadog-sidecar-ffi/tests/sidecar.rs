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

use datadog_sidecar::service::blocking::SidecarTransport;
use datadog_sidecar::service::{DynamicInstrumentationConfigState, InstanceId};
use libdd_common::Endpoint;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::time::{Duration, SystemTime};
#[cfg(unix)]
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::prelude::{AsRawFd, FromRawFd},
};

#[test]
#[cfg_attr(
    miri,
    ignore = "header generation and C compilation spawn native processes"
)]
fn generated_header_exposes_native_evp_transport_abi() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let generated = tempfile::tempdir().unwrap();
    let include_dir = generated.path().join(build_common::HEADER_PATH);
    fs::create_dir_all(&include_dir).unwrap();

    // Copy the headers the `-ffi` build scripts already generated into the
    // target directory; they only re-run on cbindgen.toml changes, so check
    // freshness against the crate's sources. `sidecar.h`'s build script does
    // not generate a header, so it always takes the regenerate path.
    let cached_include = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"))
        .join(build_common::HEADER_PATH);
    let header_sources = [
        ("libdd-common-ffi", "common.h"),
        // Seed shared option types with telemetry's unconditional definitions
        // before deduplicating platform-specific sidecar declarations, as in
        // release builds.
        ("libdd-telemetry-ffi", "telemetry.h"),
        // Windows-only sidecar declarations use crashtracker metadata. Include
        // those definitions in the shared header just as the release builder
        // does.
        ("libdd-crashtracker-ffi", "crashtracker.h"),
        ("datadog-sidecar-ffi", "sidecar.h"),
    ];
    for (crate_name, header_name) in header_sources {
        let cached = cached_include.join(header_name);
        if cached.is_file() && cached_header_is_fresh(&cached, &workspace.join(crate_name)) {
            fs::copy(&cached, include_dir.join(header_name)).unwrap();
        } else {
            build_common::generate_header(
                workspace.join(crate_name),
                header_name,
                generated.path().to_path_buf(),
            );
        }
    }

    let common_header = include_dir.join("common.h");
    let telemetry_header = include_dir.join("telemetry.h");
    let crashtracker_header = include_dir.join("crashtracker.h");
    let sidecar_header = include_dir.join("sidecar.h");
    tools::headers::dedup_headers(
        common_header.to_str().unwrap(),
        &[
            telemetry_header.to_str().unwrap(),
            crashtracker_header.to_str().unwrap(),
            sidecar_header.to_str().unwrap(),
        ],
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

    let mut compiler = cc::Build::new();
    compiler
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
        .warnings_into_errors(true);
    if compiler.get_compiler().is_like_msvc() {
        // Match examples/ffi/CMakeLists.txt: generated headers deliberately
        // use anonymous structs/unions; keep all other warnings as errors.
        compiler.flag("/wd4201");
    }
    compiler.try_compile("sidecar_evp_transport_abi").unwrap();
}

/// Whether the cached header postdates every file of the crate that generated
/// it.
fn cached_header_is_fresh(cached: &Path, crate_dir: &Path) -> bool {
    match fs::metadata(cached).and_then(|m| m.modified()) {
        Ok(header_time) => header_time >= newest_mtime(crate_dir),
        Err(_) => false,
    }
}

fn newest_mtime(dir: &Path) -> SystemTime {
    fn visit(dir: &Path, best: &mut SystemTime) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            if let Ok(time) = entry.metadata().and_then(|m| m.modified()) {
                *best = (*best).max(time);
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                visit(&entry.path(), best);
            }
        }
    }
    let mut best = SystemTime::UNIX_EPOCH;
    visit(dir, &mut best);
    best
}

fn set_sidecar_per_process() {
    unsafe { std::env::set_var("_DD_DEBUG_SIDECAR_IPC_MODE", "instance_per_process") }
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
// On Windows the spawn trampoline must GetProcAddress `ddog_daemon_entry_point` from the module
// containing it - here the test .exe, which (unlike a DLL) exports no symbols. The daemon thus
// exits immediately and connecting only succeeds when it wins the race against that exit.
// Spawning from Windows needs the entrypoint in a dylib, see test_ddog_sidecar_connection.
#[cfg_attr(
    windows,
    ignore = "sidecar daemon cannot start from a test exe on Windows (no exported entrypoint)"
)]
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
        let instance_id = ddog_sidecar_instanceId_build("session_id".into(), "runtime_id".into());

        ddog_sidecar_set_connection_config(
            &mut transport,
            &instance_id,
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
            &process_tags,
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            false,
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
            libdd_common_ffi::Option::Some(error) => assert!(
                error
                    .to_string()
                    .contains("host must be errors-intake.<site>")
            ),
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

        ddog_sidecar_set_application(
            &mut transport,
            "service".into(),
            "env".into(),
            "version".into(),
            &libdd_common_ffi::Vec::default(),
            DynamicInstrumentationConfigState::NotSet,
            0,
        )
        .unwrap_none();

        ddog_sidecar_telemetry_addDependency(
            &mut transport,
            "dependency_name".into(),
            "dependency_version".into(),
        )
        .unwrap_none();

        // ddog_sidecar_telemetry_addIntegration(&mut transport, integration_name,
        // integration_version) TODO add ability to add configuration

        // reconfigure the connection
        ddog_sidecar_set_connection_config(
            &mut transport,
            &instance_id,
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
            &process_tags,
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
            false,
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

// The in-process listener is process-global, so tests that start it run one at a time.
static FFE_LISTENER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A connection to the in-process sidecar listener, configured to send to `http`.
/// A spawned daemon resets the connection in the CentOS 7 CI container.
struct FfeSidecar {
    transport: Option<Box<SidecarTransport>>,
    instance_id: Option<Box<InstanceId>>,
    _listener: std::sync::MutexGuard<'static, ()>,
}

impl FfeSidecar {
    fn start(http: &httpmock::MockServer, session: &str) -> Self {
        let listener = FFE_LISTENER.lock().unwrap_or_else(|e| e.into_inner());
        assert_maybe_no_error!(ddog_sidecar_connect_master());
        let mut transport = std::ptr::null_mut();
        assert_maybe_no_error!(ddog_sidecar_connect_worker(
            i32::try_from(std::process::id()).unwrap(),
            &mut transport
        ));
        let mut transport = unsafe { Box::from_raw(transport) };
        transport
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        transport
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let agent_endpoint = Endpoint {
            url: http.url("/").parse().unwrap(),
            ..Default::default()
        };
        let process_tags = libdd_common_ffi::Vec::default();
        let instance_id =
            unsafe { ddog_sidecar_instanceId_build(session.into(), "runtime_id".into()) };
        unsafe {
            ddog_sidecar_set_connection_config(
                &mut transport,
                &instance_id,
                &agent_endpoint,
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
                &process_tags,
                "".into(),
                "".into(),
                "".into(),
                "".into(),
                "".into(),
                false,
            )
            .unwrap_none();
        }
        FfeSidecar {
            transport: Some(transport),
            instance_id: Some(instance_id),
            _listener: listener,
        }
    }
}

impl FfeSidecar {
    fn transport(&mut self) -> &mut Box<SidecarTransport> {
        self.transport.as_mut().unwrap()
    }
}

impl Drop for FfeSidecar {
    // The listener shutdown waits for open connections, so close ours first.
    fn drop(&mut self) {
        if let Some(transport) = self.transport.take() {
            ddog_sidecar_transport_drop(transport);
        }
        if let Some(instance_id) = self.instance_id.take() {
            unsafe { ddog_sidecar_instanceId_drop(instance_id) };
        }
        let _ = ddog_sidecar_shutdown_master_listener();
    }
}

// Covers FFI conversion, IPC, the sidecar exposure deduplicator and EVP encoding. Uses the
// in-process listener: a spawned daemon resets the connection in the CentOS 7 CI container.
#[test]
#[cfg_attr(miri, ignore)]
#[cfg_attr(
    windows,
    ignore = "the in-process listener is only exercised on Unix here"
)]
fn test_ddog_sidecar_ffe_exposure_features_reach_the_intake() {
    use httpmock::{Method::POST, MockServer};
    use libdd_common_ffi::CharSlice;

    // The sidecar deduplicator outlives this test; a unique flag keeps runs independent.
    let flag_key = format!("ffe-features-{}", std::process::id());
    let http = MockServer::start();
    let exposures = http.mock(|when, then| {
        let flag_key = flag_key.clone();
        when.method(POST)
            .path("/evp_proxy/v2/api/v2/exposures")
            .is_true(move |request| {
                let Ok(payload) = serde_json::from_slice::<serde_json::Value>(request.body_ref())
                else {
                    return false;
                };
                let Some(events) = payload["exposures"].as_array() else {
                    return false;
                };
                events.len() == 4
                    && events.iter().all(|event| event["flag"]["key"] == *flag_key)
                    && events[0]["subject"]["id"] == "user-a"
                    && events[0]["features"]
                        == serde_json::json!({"holdout.key": "h1", "weight": 2.0, "included": false})
                    && events[1]["subject"]["id"] == "user-b"
                    && events[1].get("features").is_none()
                    && events[2]["subject"]["id"] == "user-c"
                    && events[2].get("features").is_none()
                    && events[3]["subject"]["id"] == "user-d"
                    && events[3].get("features").is_none()
            });
        then.status(202);
    });

    let mut sidecar = FfeSidecar::start(&http, "ffe-features-session");

    let oversized = "x".repeat(libdd_ipc::max_message_size() + 1);
    fn exposure<'a>(
        flag_key: &'a str,
        subject: &'a str,
        features: CharSlice<'a>,
    ) -> FfeExposure<'a> {
        FfeExposure {
            timestamp_ms: 1_700_000_000_000,
            flag_key: flag_key.into(),
            subject_id: subject.into(),
            subject_attributes_json: "{}".into(),
            allocation_key: "alloc".into(),
            variant: "on".into(),
            serial_id: 1,
            has_serial_id: true,
            features_json: features,
        }
    }
    // The repeat differs only by features, so deduplication drops it.
    let batch = [
        exposure(
            &flag_key,
            "user-a",
            r#"{"holdout.key":"h1","weight":2,"included":false,"nested":{}}"#.into(),
        ),
        exposure(&flag_key, "user-a", r#"{"holdout.key":"h2"}"#.into()),
        exposure(&flag_key, "user-b", "not json".into()),
        exposure(&flag_key, "user-c", CharSlice::from_bytes(&[0xff])),
        // Larger than one IPC packet: it must not take the batch down with it.
        exposure(&flag_key, "user-d", oversized.as_str().into()),
    ];
    let context = FfeTelemetryContext {
        service: "svc".into(),
        env: "prod".into(),
        version: "1".into(),
    };
    assert_maybe_no_error!(unsafe {
        ddog_sidecar_send_ffe_exposure_batch(sidecar.transport(), &context, batch[..].into())
    });
    assert_maybe_no_error!(ddog_sidecar_ping(sidecar.transport()));

    for _ in 0..500 {
        if exposures.calls() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    exposures.assert_calls(1);

    drop(sidecar);
}

// Both FFE batch APIs deliver a batch larger than the kernel socket buffer, which the
// transport moves to shared memory, as one message.
#[test]
#[cfg_attr(miri, ignore)]
#[cfg_attr(
    windows,
    ignore = "the in-process listener is only exercised on Unix here"
)]
fn test_ddog_sidecar_ffe_large_batches_reach_the_intake() {
    use datadog_sidecar::service::SidecarFlushOptions;
    use httpmock::{Method::POST, MockServer};

    let run = std::process::id();
    let mib = 1024 * 1024;
    let http = MockServer::start();
    http.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/info");
        then.status(200).json_body(serde_json::json!({
            "endpoints": ["/evp_proxy/v4"],
            "evp_proxy_allowed_headers": ["DD-EVP-ORIGIN", "DD-EVP-ORIGIN-VERSION"]
        }));
    });
    let mut sidecar = FfeSidecar::start(&http, "ffe-large-session");
    let producer = EvpProducerIdentity {
        origin: "dd-trace-php".into(),
        version: "1.0.0".into(),
    };
    let agent_endpoint = Endpoint {
        url: http.url("/").parse().unwrap(),
        ..Default::default()
    };
    assert_maybe_no_error!(unsafe {
        ddog_sidecar_session_set_evp_transport(
            sidecar.transport(),
            EvpTransportMode::AgentOnly,
            &agent_endpoint,
            null(),
            "event-platform-intake".into(),
            &producer,
        )
    });
    let context = FfeTelemetryContext {
        service: "svc".into(),
        env: "prod".into(),
        version: "1".into(),
    };

    // Three 1 MiB records exceed the kernel send size on Linux and fit one message.
    let flag_key = format!("ffe-large-{run}");
    let attributes = format!(r#"{{"blob":"{}"}}"#, "x".repeat(mib));
    let subjects: Vec<String> = (0..3).map(|i| format!("large-user-{run}-{i}")).collect();
    // One request can carry several records, and httpmock credits a request to
    // a single mock, so record what each request carried instead.
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    for (path, list, field) in [
        ("/api/v2/exposures", "exposures", "/subject/id"),
        ("/api/v2/flagevaluation", "flagEvaluations", "/flag/key"),
    ] {
        let received = received.clone();
        http.mock(move |when, then| {
            when.method(POST)
                .path_includes(path)
                .is_true(move |request| {
                    let payload: serde_json::Value =
                        serde_json::from_slice(request.body_ref()).unwrap_or_default();
                    let records = payload[list].as_array().cloned().unwrap_or_default();
                    received.lock().unwrap().extend(
                        records.iter().filter_map(|record| {
                            record.pointer(field)?.as_str().map(str::to_owned)
                        }),
                    );
                    true
                });
            then.status(202);
        });
    }
    let control_subject = format!("large-control-{run}");
    let small = [split_exposure(&flag_key, &control_subject, "{}")];
    assert_maybe_no_error!(unsafe {
        ddog_sidecar_send_ffe_exposure_batch(sidecar.transport(), &context, small[..].into())
    });
    let batch: Vec<_> = subjects
        .iter()
        .map(|subject| split_exposure(&flag_key, subject, &attributes))
        .collect();
    assert_maybe_no_error!(unsafe {
        ddog_sidecar_send_ffe_exposure_batch(sidecar.transport(), &context, batch[..].into())
    });
    // Two large messages in a row overflow the macOS socket buffer (ENOBUFS), which closes
    // the connection; let the sidecar read the first one.
    assert_maybe_no_error!(ddog_sidecar_ping(sidecar.transport()));

    let flags: Vec<String> = (0..3).map(|i| format!("large-flag-{run}-{i}")).collect();
    let large_key = "k".repeat(mib);
    let evaluations: Vec<_> = flags
        .iter()
        .map(|flag| split_evaluation(flag, &large_key))
        .collect();
    assert_maybe_no_error!(unsafe {
        ddog_sidecar_send_ffe_flag_evaluation_batch(
            sidecar.transport(),
            &context,
            evaluations[..].into(),
        )
    });
    assert_maybe_no_error!(ddog_sidecar_flush(
        sidecar.transport(),
        SidecarFlushOptions {
            flag_evaluations: true,
            ..Default::default()
        }
    ));

    let expected: Vec<&String> = subjects
        .iter()
        .chain(&flags)
        .chain([&control_subject])
        .collect();
    let arrived = |id: &String| received.lock().unwrap().contains(id);
    for _ in 0..1000 {
        if expected.iter().all(|id| arrived(id)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for id in &expected {
        assert!(arrived(id), "{id} did not arrive");
    }
    drop(sidecar);
}

// FFE batch submission never waits for the sidecar: a stalled sidecar must not block the
// caller, which has no read timeout by default.
#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore)]
fn test_ddog_sidecar_ffe_batches_do_not_wait_for_a_stalled_sidecar() {
    let (client, peer) = libdd_ipc::SeqpacketConn::socketpair().unwrap();
    // Larger than half the kernel send size, so it cannot sit next to the first request.
    let attributes = format!(
        r#"{{"blob":"{}"}}"#,
        "x".repeat(client.max_send_size() / 2 + 1)
    );
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut transport = Box::new(SidecarTransport::from(client));
        let context = FfeTelemetryContext {
            service: "svc".into(),
            env: "prod".into(),
            version: "1".into(),
        };
        let small = [split_exposure("flag", "user", "{}")];
        let large = [split_exposure("flag", "user", &attributes)];
        let evaluations = [split_evaluation("flag", &attributes)];
        unsafe {
            let _ =
                ddog_sidecar_send_ffe_exposure_batch(&mut transport, &context, small[..].into());
            let _ =
                ddog_sidecar_send_ffe_exposure_batch(&mut transport, &context, large[..].into());
            let _ = ddog_sidecar_send_ffe_flag_evaluation_batch(
                &mut transport,
                &context,
                evaluations[..].into(),
            );
        }
        done.send(()).unwrap();
    });
    // A blocked worker cannot be released: a closed datagram peer does not wake a
    // pending receive on macOS. Fail without joining it; the process exit ends it.
    assert!(
        finished.recv_timeout(Duration::from_secs(2)).is_ok(),
        "FFE batch submission waited for the sidecar"
    );
    worker.join().unwrap();
    drop(peer);
}

fn split_exposure<'a>(flag_key: &'a str, subject: &'a str, attributes: &'a str) -> FfeExposure<'a> {
    FfeExposure {
        timestamp_ms: 1_700_000_000_000,
        flag_key: flag_key.into(),
        subject_id: subject.into(),
        subject_attributes_json: attributes.into(),
        allocation_key: "alloc".into(),
        variant: "on".into(),
        serial_id: 1,
        has_serial_id: true,
        features_json: r#"{"holdout.key":"h1"}"#.into(),
    }
}

fn split_evaluation<'a>(flag_key: &'a str, targeting_key: &'a str) -> FfeFlagEvaluation<'a> {
    FfeFlagEvaluation {
        timestamp_ms: 1_700_000_000_000,
        flag_key: flag_key.into(),
        first_evaluation_ms: 1_700_000_000_000,
        last_evaluation_ms: 1_700_000_000_000,
        evaluation_count: 1,
        variant: "on".into(),
        allocation_key: "alloc".into(),
        targeting_rule_key: "".into(),
        targeting_key: targeting_key.into(),
        evaluation_context_json: "".into(),
        error_message: "".into(),
        runtime_default_used: false,
        observe_full_evaluation_data: false,
        features_json: r#"{"holdout.key":"h1"}"#.into(),
    }
}
