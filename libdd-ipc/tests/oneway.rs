// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_ipc::{SeqpacketConn, ipc_server::OwnedServerConn};
use std::sync::atomic::AtomicU64;

#[libdd_ipc_macros::service]
trait Oneway {
    #[oneway]
    async fn post(value: u64);
    async fn read() -> u64;
}

struct Handler {
    connection: OwnedServerConn,
    received: AtomicU64,
    value: u64,
}

impl Oneway for Handler {
    fn connection(&self) -> &OwnedServerConn {
        &self.connection
    }
    fn recv_counter(&self) -> &AtomicU64 {
        &self.received
    }
    async fn post(&mut self, value: u64) {
        self.value += value;
    }
    async fn read(&mut self) -> u64 {
        self.value
    }
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(miri, ignore = "requires native IPC sockets")]
async fn oneway_packets_preserve_order_without_consuming_reply_slots() {
    let (client, server) = SeqpacketConn::socketpair().unwrap();
    let mut handler = Handler {
        connection: OwnedServerConn::new(server).unwrap(),
        received: AtomicU64::new(0),
        value: 0,
    };
    let server = tokio::spawn(async move { serve_oneway_connection(&mut handler).await });
    tokio::task::spawn_blocking(move || {
        let mut client = OnewayChannel::new(client);
        assert!(client.try_send_post(1));
        assert!(client.try_send_request(&OnewayRequest::Post { value: 2 }));
        client
            .send_request_blocking(&OnewayRequest::Post { value: 4 })
            .unwrap();
        assert_eq!(client.0.outstanding(), 0);
        assert_eq!(client.call_read().unwrap(), 7);
        assert!(
            client
                .call_request_blocking::<u64>(&OnewayRequest::Post { value: 8 })
                .is_err()
        );
        assert_eq!(client.call_read().unwrap(), 7);
        assert_eq!(client.0.outstanding(), 0);
    })
    .await
    .unwrap();
    // macOS datagram socketpairs do not report EOF when the peer closes.
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled());
    }
}
