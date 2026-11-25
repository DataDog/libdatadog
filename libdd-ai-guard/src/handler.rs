// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use rc_x509_client::dispatch::{
    Dispatch, DispatchError, DispatchResponder, DispatchResult, DispatchStream,
};
use rc_x509_proto::{
    decode, encode,
    magic_tunnel::{
        remote_config::v1::{PingRequest, PingResponse},
        v1::{MagicTunnelResponse, magic_tunnel_response},
    },
    protocol::v1,
};
use tokio::sync::watch;
use tracing::{debug, info, warn};

pub const DEBUG_PING_URI: &str = "rc.x509.magic_tunnel.remote_config.v1.DebugService/Ping";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedPayload {
    pub uri: String,
    pub reason: String,
    pub payload: Vec<u8>,
}

impl ReceivedPayload {
    pub fn as_utf8_lossy(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }
}

pub fn handle_dispatch(
    dispatch: Dispatch,
    payload_tx: &watch::Sender<Option<ReceivedPayload>>,
) -> DispatchResult {
    let correlation_id = dispatch.correlation_id;
    let result = route_dispatch(dispatch, payload_tx);
    DispatchResult {
        correlation_id,
        result,
    }
}

fn route_dispatch(
    dispatch: Dispatch,
    payload_tx: &watch::Sender<Option<ReceivedPayload>>,
) -> Result<v1::DispatchResponsePayload, DispatchError> {
    let req: v1::DispatchRequestPayload = decode(dispatch.payload.as_bytes().clone())
        .map_err(|err| {
            warn!(error = %err, "failed to decode DispatchRequestPayload");
            DispatchError::UnknownPayload
        })?;

    let mt = match req.payload {
        Some(v1::dispatch_request_payload::Payload::MagicTunnel(mt)) => mt,
        None => return Err(DispatchError::UnknownPayload),
    };

    match mt.uri.as_str() {
        DEBUG_PING_URI => {
            let ping: PingRequest = match decode(mt.request) {
                Ok(p) => p,
                Err(err) => {
                    warn!(error = %err, "failed to decode PingRequest");
                    return Ok(magic_tunnel_error_response(format!(
                        "invalid PingRequest: {err}"
                    )));
                }
            };

            let received = ReceivedPayload {
                uri: mt.uri,
                reason: ping.reason,
                payload: ping.payload,
            };

            info!(
                reason = %received.reason,
                payload_len = received.payload.len(),
                payload = %received.as_utf8_lossy(),
                "received Magic Tunnel DebugService/Ping payload"
            );

            let _ = payload_tx.send(Some(received));

            let resp = PingResponse {
                now: Some(current_timestamp()),
            };
            Ok(v1::DispatchResponsePayload {
                payload: Some(v1::dispatch_response_payload::Payload::MagicTunnel(
                    MagicTunnelResponse {
                        result: Some(magic_tunnel_response::Result::Response(encode(&resp))),
                    },
                )),
            })
        }
        unknown => {
            debug!(uri = %unknown, "no dispatch handler for Magic Tunnel URI");
            Err(DispatchError::NoDispatchHandler)
        }
    }
}

fn magic_tunnel_error_response(message: String) -> v1::DispatchResponsePayload {
    v1::DispatchResponsePayload {
        payload: Some(v1::dispatch_response_payload::Payload::MagicTunnel(
            MagicTunnelResponse {
                result: Some(magic_tunnel_response::Result::HandlerError(message)),
            },
        )),
    }
}

fn current_timestamp() -> pbjson_types::Timestamp {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    pbjson_types::Timestamp {
        seconds: dur.as_secs() as i64,
        nanos: dur.subsec_nanos() as i32,
    }
}

pub async fn run_dispatch_loop(
    mut stream: DispatchStream,
    responder: DispatchResponder,
    payload_tx: watch::Sender<Option<ReceivedPayload>>,
) {
    while let Some(dispatch) = stream.next().await {
        let result = handle_dispatch(dispatch, &payload_tx);
        if responder.send_response(result).await.is_err() {
            debug!("dispatch responder closed, stopping dispatch loop");
            break;
        }
    }
}
