// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{
    fs::File,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use super::platform::PlatformHandle;

extern crate self as datadog_ipc;

#[datadog_ipc_macros::service]
pub trait ExampleInterface {
    async fn notify();
    #[blocking]
    async fn ping();
    async fn time_now() -> Duration;
    async fn req_cnt() -> u32;
    async fn store_file(#[SerializedHandle] file: PlatformHandle<File>);
}

#[derive(Default, Clone)]
pub struct ExampleServer {
    req_cnt: Arc<AtomicU32>,
    stored_files: Arc<Mutex<Vec<PlatformHandle<File>>>>,
}

#[cfg(unix)]
impl ExampleServer {
    pub async fn accept_connection(self, conn: crate::SeqpacketConn) {
        serve_example_interface_connection(conn, Arc::new(self)).await
    }
}

impl ExampleInterface for ExampleServer {
    fn notify(
        &self,
        _peer: datadog_ipc::PeerCredentials,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        std::future::ready(())
    }

    fn ping(
        &self,
        _peer: datadog_ipc::PeerCredentials,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        std::future::ready(())
    }

    fn time_now(
        &self,
        _peer: datadog_ipc::PeerCredentials,
    ) -> impl std::future::Future<Output = Duration> + Send + '_ {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        std::future::ready(Instant::now().elapsed())
    }

    fn req_cnt(
        &self,
        _peer: datadog_ipc::PeerCredentials,
    ) -> impl std::future::Future<Output = u32> + Send + '_ {
        std::future::ready(self.req_cnt.fetch_add(1, Ordering::AcqRel))
    }

    fn store_file(
        &self,
        _peer: datadog_ipc::PeerCredentials,
        file: PlatformHandle<File>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        #[allow(clippy::unwrap_used)]
        self.stored_files.lock().unwrap().push(file);
        std::future::ready(())
    }
}
