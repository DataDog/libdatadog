// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{
    fs::File,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use super::platform::{FileBackedHandle, PlatformHandle, ShmHandle};
use crate::ipc_server::OwnedServerConn;

extern crate self as datadog_ipc;

#[datadog_ipc_macros::service]
pub trait ExampleInterface {
    async fn notify();
    #[blocking]
    async fn ping();
    async fn time_now() -> Duration;
    async fn req_cnt() -> u32;
    async fn store_file(#[SerializedHandle] file: PlatformHandle<File>);
    /// Receives a shared memory handle, maps it, and returns the sum of the first `len` bytes.
    /// Used to verify cross-process handle transfer (Windows DuplicateHandle / Unix SCM_RIGHTS).
    async fn shm_sum(#[SerializedHandle] handle: ShmHandle, len: usize) -> u64;
    /// Receives a byte payload and returns its length.
    /// Used to verify that messages larger than mio's 4 KB internal read buffer are handled
    /// correctly (no ERROR_MORE_DATA panic).
    async fn echo_len(payload: Vec<u8>) -> u32;
}

/// Shared server state. Cloned into a per-connection [`ExampleConnectionHandler`] on accept.
#[derive(Default, Clone)]
pub struct ExampleServer {
    req_cnt: Arc<AtomicU64>,
    stored_files: Arc<Mutex<Vec<PlatformHandle<File>>>>,
}

impl ExampleServer {
    pub async fn accept_connection(self, conn: crate::SeqpacketConn) {
        let connection = match OwnedServerConn::new(conn) {
            Ok(c) => c,
            Err(e) => {
                ::tracing::error!("ExampleServer: failed to set up connection: {e}");
                return;
            }
        };
        serve_example_interface_connection(Arc::new(ExampleConnectionHandler {
            server: self,
            connection,
        }))
        .await
    }
}

/// Per-connection handler: owns the connection and serves requests received on it.
struct ExampleConnectionHandler {
    server: ExampleServer,
    connection: OwnedServerConn,
}

impl ExampleInterface for ExampleConnectionHandler {
    fn recv_counter(&self) -> &AtomicU64 {
        &self.server.req_cnt
    }

    fn connection(&self) -> &OwnedServerConn {
        &self.connection
    }

    fn notify(&self) -> impl std::future::Future<Output = ()> + Send + '_ {
        std::future::ready(())
    }

    fn ping(&self) -> impl std::future::Future<Output = ()> + Send + '_ {
        std::future::ready(())
    }

    fn time_now(&self) -> impl std::future::Future<Output = Duration> + Send + '_ {
        std::future::ready(Instant::now().elapsed())
    }

    fn req_cnt(&self) -> impl std::future::Future<Output = u32> + Send + '_ {
        std::future::ready(self.server.req_cnt.load(Ordering::Relaxed) as u32)
    }

    fn store_file(
        &self,
        file: PlatformHandle<File>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        #[allow(clippy::unwrap_used)]
        self.server.stored_files.lock().unwrap().push(file);
        std::future::ready(())
    }

    async fn shm_sum(&self, handle: ShmHandle, len: usize) -> u64 {
        match handle.map() {
            Ok(mapped) => mapped.as_slice()[..len].iter().map(|&b| b as u64).sum(),
            Err(_) => u64::MAX,
        }
    }

    fn echo_len(&self, payload: Vec<u8>) -> impl std::future::Future<Output = u32> + Send + '_ {
        std::future::ready(payload.len() as u32)
    }
}
