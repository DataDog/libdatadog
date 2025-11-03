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

use futures::future::{pending, ready, Pending, Ready};
use tarpc::context::Context;
use tarpc::server::Channel;

use super::{
    platform::PlatformHandle,
    transport::{blocking::BlockingTransport, Transport},
};

extern crate self as datadog_ipc;

#[libdd_ipc_macros::impl_transfer_handles]
#[tarpc::service]
pub trait ExampleInterface {
    async fn notify() -> ();
    async fn ping() -> ();
    async fn time_now() -> Duration;
    async fn req_cnt() -> u32;
    async fn store_file(#[SerializedHandle] file: PlatformHandle<File>) -> ();
    #[SerializedHandle]
    async fn retrieve_file() -> Option<PlatformHandle<File>>;
}

pub type ExampleTransport = BlockingTransport<ExampleInterfaceResponse, ExampleInterfaceRequest>;

#[derive(Default, Clone, Debug)]
pub struct ExampleServer {
    req_cnt: Arc<AtomicU32>,
    stored_files: Arc<Mutex<Vec<PlatformHandle<File>>>>,
}

impl ExampleServer {
    pub async fn accept_connection(self, channel: crate::platform::Channel) {
        #[allow(clippy::unwrap_used)]
        let server = tarpc::server::BaseChannel::new(
            tarpc::server::Config::default(),
            Transport::try_from(channel).unwrap(),
        );

        server.execute(self.serve()).await
    }
}

impl ExampleInterface for ExampleServer {
    type PingFut = Ready<()>;

    fn ping(self, _: Context) -> Self::PingFut {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        ready(())
    }

    type NotifyFut = Pending<()>;

    fn notify(self, _: Context) -> Self::NotifyFut {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        pending() // returning pending future, ensures the RPC system will not try to return a
                  // response to the client
    }

    type TimeNowFut = Ready<Duration>;

    fn time_now(self, _: Context) -> Self::TimeNowFut {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);
        ready(Instant::now().elapsed())
    }

    type ReqCntFut = Ready<u32>;

    fn req_cnt(self, _: Context) -> Self::ReqCntFut {
        ready(self.req_cnt.fetch_add(1, Ordering::AcqRel))
    }

    type StoreFileFut = Ready<()>;

    fn store_file(self, _: Context, file: PlatformHandle<File>) -> Self::StoreFileFut {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);

        #[allow(clippy::unwrap_used)]
        self.stored_files.lock().unwrap().push(file);

        ready(())
    }

    type RetrieveFileFut = Ready<Option<PlatformHandle<File>>>;

    fn retrieve_file(self, _: Context) -> Self::RetrieveFileFut {
        self.req_cnt.fetch_add(1, Ordering::AcqRel);

        #[allow(clippy::unwrap_used)]
        ready(self.stored_files.lock().unwrap().pop())
    }
}
