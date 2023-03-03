use std::{
    os::{fd::AsRawFd, unix::net::UnixListener as StdUnixListener},
    path::PathBuf,
};

use ddtelemetry::ipc::setup::Liaison;
use spawn_worker::{entrypoint, Stdio};
use tokio::net::UnixListener;

use crate::mini_agent;

#[no_mangle]
pub extern "C" fn sidecar_entrypoint() {
    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener = StdUnixListener::try_from(fd).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt_guard = rt.enter();
        listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(listener).unwrap();

        let server_future = mini_agent::main(listener);

        rt.block_on(server_future).unwrap();
    }
}

#[allow(dead_code)]
pub(crate) unsafe fn maybe_start() -> anyhow::Result<PathBuf> {
    let liaison = ddtelemetry::ipc::setup::SharedDirLiaison::new_tmp_dir();
    if let Some(listener) = liaison.attempt_listen()? {
        spawn_worker::SpawnWorker::new()
            .stdin(Stdio::Null)
            .stderr(Stdio::Inherit)
            .stdout(Stdio::Inherit)
            .pass_fd(listener)
            .daemonize(true)
            .target(entrypoint!(sidecar_entrypoint))
            .spawn()?;
    };

    // TODO: temporary hack - connect to socket and leak it
    // this should lead to sidecar being up as long as the processes that attempted to connect to it

    let con = liaison.connect_to_server()?;
    nix::unistd::dup(con.as_raw_fd())?; // LEAK! - dup also resets (?) CLOEXEC flag set by Rust UnixStream constructor

    Ok(liaison.path().to_path_buf())
}
