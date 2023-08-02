use std::time::Instant;
use tokio::net::windows::named_pipe::NamedPipeServer;

#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point() {
    #[cfg(feature = "tracing")]
    enable_tracing().ok();
    let now = Instant::now();

    if let Some(handle) = spawn_worker::recv_passed_handle() {
        tracing::info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || unsafe { NamedPipeServer::from_raw_handle(handle) };
        if let Err(err) = enter_listener_loop(acquire_listener) {
            tracing::error!("Error: {err}")
        }
    }

    tracing::info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        getpid(),
        now.elapsed().as_secs_f64()
    )
}
