use std::{
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::net::UnixListener,
    },
    path::PathBuf,
    thread,
    time::Duration,
};

use ddtelemetry::ipc::setup::Liaison;
use spawn_worker::{entrypoint, Stdio};

#[no_mangle]
pub extern "C" fn sidecar_entrypoint() {
    eprintln!("ehlo mah dudes");
    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener = UnixListener::try_from(fd).unwrap();
    }

    thread::sleep(Duration::from_secs(100));
    eprintln!("bye");
}

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
