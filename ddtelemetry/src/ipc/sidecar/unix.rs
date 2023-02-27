// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use spawn_worker::{entrypoint, getpid, Stdio};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::{
    io::{self},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::select;

use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use crate::ipc::example_interface::{ExampleServer, ExampleTransport};
use crate::ipc::platform::Channel as IpcChannel;

use crate::ipc::setup::{self, Liaison};

async fn main_loop(listener: UnixListener) -> tokio::io::Result<()> {
    let counter = Arc::new(AtomicI32::new(0));
    let token = CancellationToken::new();
    let cloned_counter = Arc::clone(&counter);
    let cloned_token = token.clone();

    tokio::spawn(async move {
        let mut consecutive_no_active_connections = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            if cloned_counter.load(Ordering::Acquire) <= 0 {
                consecutive_no_active_connections += 1;
            } else {
                consecutive_no_active_connections = 0;
            }

            if consecutive_no_active_connections > 1 {
                cloned_token.cancel();
                println!("no active connections - shutting down");
            }
        }
    });

    let server = ExampleServer::default();

    loop {
        let (socket, _) = select! {
            res = listener.accept() => {
                res?
            },
            _ = token.cancelled() => {
                break
            },
        };

        println!("connection accepted");
        counter.fetch_add(1, Ordering::AcqRel);

        let cloned_counter = Arc::clone(&counter);
        let server = server.clone();
        tokio::spawn(async move {
            server.accept_connection(socket).await;
            cloned_counter.fetch_add(-1, Ordering::AcqRel);
            println!("connection closed");
        });
    }
    Ok(())
}

fn enter_listener_loop(listener: StdUnixListener) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _g = runtime.enter();

    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;

    runtime.block_on(main_loop(listener)).map_err(|e| e.into())
}

#[no_mangle]
pub extern "C" fn daemon_entry_point() {
    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener: StdUnixListener = fd.into();

        println!("starting sidecar, pid: {}", getpid());
        if let Err(err) = enter_listener_loop(listener) {
            println!("Error: {err}")
        }
    }
}

fn daemonize(listener: StdUnixListener) -> io::Result<()> {
    // TODO: allow passing presaved environment
    let child = unsafe { spawn_worker::SpawnWorker::new() }
        .pass_fd(listener)
        .stdin(Stdio::Null)
        .daemonize(true)
        .target(entrypoint!(daemon_entry_point))
        .spawn()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

    child
        .wait()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

    Ok(())
}

pub fn start_or_connect_to_sidecar() -> io::Result<ExampleTransport> {
    let liaison = setup::DefaultLiason::default();
    if let Some(listener) = liaison.attempt_listen()? {
        daemonize(listener)?;
    };

    Ok(IpcChannel::from(liaison.connect_to_server()?).into())
}
