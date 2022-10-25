// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::os::unix::net::UnixListener as StdUnixListener;
use std::{
    io::{self},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};

use nix::{sys::wait::waitpid, unistd::Pid};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use crate::{
    fork::{fork_fn, getpid},
    ipc::setup::{self, Liaison},
};

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

    loop {
        let (mut socket, _) = select! {
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
        tokio::spawn(async move {
            let mut buf = vec![0; 1024];

            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);

                if n == 0 {
                    break;
                }
                println!("writing stuff back");

                socket.write_all(&buf[0..n]).await.ok();
            }
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

fn daemonize(listener: StdUnixListener) -> io::Result<()> {
    unsafe {
        let pid = fork_fn(listener, |listener| {
            fork_fn(listener, |listener| {
                println!("starting sidecar, pid: {}", getpid());
                if let Err(err) = enter_listener_loop(listener) {
                    println!("Error: {}", err)
                }
            })
            .ok();
        })?;
        waitpid(Pid::from_raw(pid), None)?;
    };
    Ok(())
}

pub fn start_or_connect_to_sidecar() -> io::Result<UnixStream> {
    let liaison = setup::DefaultLiason::default();
    if let Some(listener) = liaison.attempt_listen()? {
        daemonize(listener)?;
    };

    liaison.connect_to_server()
}
