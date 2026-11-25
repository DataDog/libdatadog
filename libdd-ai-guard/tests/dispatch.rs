// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::Bytes;
use futures::StreamExt;
use libdd_ai_guard::{DEBUG_PING_URI, handle_dispatch, run_dispatch_loop};
use rc_crypto::Signature;
use rc_x509_client::{
    Payload,
    dispatch::{Dispatch, DispatchError, new_dispatcher_interconnect},
    host_runtime::CorrelationId,
};
use rc_x509_proto::{
    decode, encode,
    magic_tunnel::{
        remote_config::v1::{PingRequest, PingResponse},
        v1::{MagicTunnelRequest, magic_tunnel_response},
    },
    protocol::v1,
};
use rc_x509_test_client_harness::{TestClientBuilder, client::conn_id_to_proto};
use rc_x509_test_helpers::test_issuer::TestChain;
use tokio::sync::watch;

#[tokio::test]
async fn mock_io_magic_tunnel_ping_roundtrip() {
    let chain = TestChain::default();
    let mut client = TestClientBuilder::default()
        .with_testing_roots(chain.root())
        .build();
    let mut conn = client.new_connection().await;

    let conn_id = conn.perform_handshake().await;
    let proto_conn_id = conn_id_to_proto(conn_id.clone());

    let ping_req = PingRequest {
        connection_id: conn_id.to_string(),
        reason: "poc-phase-0".to_string(),
        payload: b"hello-from-mock-io".to_vec(),
    };
    let correlation_id = CorrelationId::new(7);

    conn.dispatch_magic_tunnel(
        correlation_id,
        chain.leaf.cert().cert_id(),
        Signature::try_from([42_u8; 10].as_slice()).unwrap(),
        DEBUG_PING_URI,
        encode(&ping_req),
    )
    .await;

    let (payload_tx, payload_rx) = watch::channel(None);

    let test_dispatch = conn.get_application_dispatch().await;
    let (uri, _) = test_dispatch.assume_magic_tunnel(&proto_conn_id);
    assert_eq!(uri, DEBUG_PING_URI);

    let dispatch = Dispatch {
        correlation_id: test_dispatch.correlation_id(),
        payload: Payload::from(test_dispatch.payload().as_bytes().clone()),
    };
    let result = handle_dispatch(dispatch, &payload_tx);
    test_dispatch.respond(result.result).await;

    let received = payload_rx.borrow().clone().expect("payload stored in watch");
    assert_eq!(received.uri, DEBUG_PING_URI);
    assert_eq!(received.reason, "poc-phase-0");
    assert_eq!(received.as_utf8_lossy(), "hello-from-mock-io");

    let mt_resp = conn
        .recv()
        .await
        .assume_magic_tunnel_response(correlation_id);
    let resp_bytes = match mt_resp {
        magic_tunnel_response::Result::Response(b) => b,
        other => panic!("expected Response, got {other:?}"),
    };
    let ping_resp: PingResponse = decode(resp_bytes).expect("valid PingResponse");
    assert!(ping_resp.now.is_some(), "PingResponse.now must be populated");

    client.shutdown().await;
}

#[tokio::test]
async fn dispatch_loop_routes_ping_and_rejects_unknown_uri() {
    let (mut publisher, stream, responder) = new_dispatcher_interconnect();
    let mut responses = publisher.take_recv_stream().expect("recv stream");

    let (payload_tx, mut payload_rx) = watch::channel(None);
    let loop_task = tokio::spawn(run_dispatch_loop(stream, responder, payload_tx));

    let ping_req = PingRequest {
        connection_id: "test-conn".to_string(),
        reason: "loop-test".to_string(),
        payload: b"payload-1".to_vec(),
    };
    let req_payload = encode(&v1::DispatchRequestPayload {
        connection_id: None,
        payload: Some(v1::dispatch_request_payload::Payload::MagicTunnel(
            MagicTunnelRequest {
                uri: DEBUG_PING_URI.to_string(),
                request: encode(&ping_req),
            },
        )),
    });

    publisher
        .dispatch(Dispatch {
            correlation_id: CorrelationId::new(1),
            payload: Payload::from(req_payload),
        })
        .await
        .expect("dispatch ping");

    payload_rx.changed().await.expect("watch notified");
    assert_eq!(
        payload_rx.borrow().as_ref().unwrap().as_utf8_lossy(),
        "payload-1"
    );

    let first_resp = responses.next().await.expect("ping response");
    assert_eq!(first_resp.correlation_id, CorrelationId::new(1));
    assert!(first_resp.result.is_ok());

    let unknown_payload = encode(&v1::DispatchRequestPayload {
        connection_id: None,
        payload: Some(v1::dispatch_request_payload::Payload::MagicTunnel(
            MagicTunnelRequest {
                uri: "rc.x509.magic_tunnel.unknown.v1.Svc/Op".to_string(),
                request: Bytes::new(),
            },
        )),
    });
    publisher
        .dispatch(Dispatch {
            correlation_id: CorrelationId::new(2),
            payload: Payload::from(unknown_payload),
        })
        .await
        .expect("dispatch unknown");

    let second_resp = responses.next().await.expect("unknown response");
    assert_eq!(second_resp.correlation_id, CorrelationId::new(2));
    assert!(matches!(
        second_resp.result,
        Err(DispatchError::NoDispatchHandler)
    ));

    drop(publisher);
    loop_task.await.expect("dispatch loop exits cleanly");
}
