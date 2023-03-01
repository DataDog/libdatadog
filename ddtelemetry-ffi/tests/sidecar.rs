use ddtelemetry_ffi::sidecar::unix::*;
use ddtelemetry_ffi::MaybeError;
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::{fd::FromRawFd, unix::prelude::AsRawFd},
};

fn set_sidecar_per_process() {
    std::env::set_var("_DD_DEBUG_SIDECAR_IPC_MODE", "instance_per_process")
}

#[test]
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
    assert_eq!(ddog_sidecar_connect(&mut transport), MaybeError::None);
    let mut transport = unsafe { Box::from_raw(transport) };
    assert_eq!(ddog_sidecar_ping(&mut transport), MaybeError::None);

    ddog_sidecar_transport_drop(transport);
}

#[test]
fn test_ddog_sidecar_register_app() {
    set_sidecar_per_process();

    let mut transport = std::ptr::null_mut();
    assert_eq!(ddog_sidecar_connect(&mut transport), MaybeError::None);
    let mut transport = unsafe { Box::from_raw(transport) };
    unsafe {
        ddog_sidecar_session_config_setAgentUrl(
            &mut transport,
            "session_id".into(),
            "http://localhost:8082/".into(),
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

        assert_eq!(
            ddog_sidecar_telemetry_flushServiceData(
                &mut transport,
                &instance_id,
                &queue_id,
                &meta,
                "service_name".into()
            ),
            MaybeError::None
        );
        // reset session config - and cause shutdown of all existing instances
        ddog_sidecar_session_config_setAgentUrl(&mut transport, "session_id".into(), "".into());

        //TODO: Shutdown the service
        // enough case: have C api that shutsdown telemetry worker
        // ideal case : when connection socket is closed by the client the telemetry worker shuts down automatically
        ddog_sidecar_instanceId_drop(instance_id);
        ddog_sidecar_runtimeMeta_drop(meta);
    };

    ddog_sidecar_transport_drop(transport);
}
