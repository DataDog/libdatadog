// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::sender::SidecarSender;
use super::sidecar_interface::SidecarInterfaceRequest;
use super::{
    FFE_DEFERRED_V1_CAPABILITY, FfeExposure, FfeExposureBatch, FfeTelemetryContext, SidecarAction,
};
use libdd_ipc::{client::DeadlineCallError, codec, oversized::encode_request};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkOutcome {
    Confirmed,
    LegacySubmitted,
    Refused,
    Unknown,
    Invalid,
}

pub(super) const MAX_CHUNK_ROWS: usize = 64;

fn request(batch: FfeExposureBatch, checked: bool) -> SidecarInterfaceRequest {
    if checked {
        SidecarInterfaceRequest::SubmitDeferredFfeV1 { batch }
    } else {
        SidecarInterfaceRequest::EnqueueActions {
            actions: vec![SidecarAction::FfeExposureBatch(batch)],
        }
    }
}

fn prefix(
    context: FfeTelemetryContext,
    rows: impl Iterator<Item = Option<FfeExposure>>,
    checked: bool,
    max: usize,
) -> Option<(SidecarInterfaceRequest, u32)> {
    let mut batch = FfeExposureBatch {
        context,
        exposures: Vec::new(),
    };
    // Bincode uses a fixed-width sequence length, so each row adds its serialized size.
    let overhead = codec::encoded_size(&request(batch.clone(), checked)).ok()?;
    let mut remaining = (max as u64).checked_sub(overhead)?;
    for row in rows.take(MAX_CHUNK_ROWS) {
        let Some(row) = row else { break };
        if !libdd_ffe::telemetry::exposures::is_complete(&row)
            || (checked && !super::ffe_exposures_flusher::fits_http(&batch.context, &row))
        {
            break;
        }
        let Some(rest) = codec::encoded_size(&row)
            .ok()
            .and_then(|n| remaining.checked_sub(n))
        else {
            break;
        };
        remaining = rest;
        batch.exposures.push(row);
    }
    let n = batch.exposures.len() as u32;
    (n > 0).then(|| (request(batch, checked), n))
}

impl SidecarSender {
    #[doc(hidden)]
    pub fn submit_deferred_chunk_for_test(
        &mut self,
        context: FfeTelemetryContext,
        rows: impl Iterator<Item = Option<FfeExposure>>,
    ) -> (ChunkOutcome, u32) {
        let mut rows = rows.peekable();
        if rows.peek().is_none() {
            return (ChunkOutcome::Confirmed, 0);
        }
        if !self.fixture_peer_supports(FFE_DEFERRED_V1_CAPABILITY) {
            return (ChunkOutcome::Refused, 0);
        }
        let Some((request, count)) = prefix(context, rows, true, libdd_ipc::max_message_size())
        else {
            return (ChunkOutcome::Invalid, 1);
        };
        match self.channel.call_request_blocking::<bool>(&request) {
            Ok(true) => (ChunkOutcome::Confirmed, count),
            Ok(false) => (ChunkOutcome::Refused, 0),
            Err(_) => (ChunkOutcome::Unknown, count),
        }
    }

    /// Negotiates whether a deferred reply fences later admissions on this connection.
    pub fn supports_ordered_ffe_handoff_until(&mut self, deadline: Instant) -> bool {
        self.ffe_capabilities_until(deadline)
            .is_some_and(|bits| bits & FFE_DEFERRED_V1_CAPABILITY != 0)
    }

    /// Submits at most one ordered prefix. Confirmed and legacy counts cover submitted rows;
    /// unknown counts cover the possibly applied chunk, invalid counts identify one rejected row.
    pub fn submit_deferred_chunk(
        &mut self,
        context: FfeTelemetryContext,
        rows: impl Iterator<Item = Option<FfeExposure>>,
        deadline: Instant,
    ) -> (ChunkOutcome, u32) {
        let mut rows = rows.peekable();
        if rows.peek().is_none() {
            return (ChunkOutcome::Confirmed, 0);
        }
        let Some(bits) = self.ffe_capabilities_until(deadline) else {
            return (ChunkOutcome::Refused, 0);
        };
        let checked = bits & FFE_DEFERRED_V1_CAPABILITY != 0;
        let Some((req, count)) = prefix(context, rows, checked, libdd_ipc::max_message_size())
        else {
            return (ChunkOutcome::Invalid, 1);
        };
        let channel = &mut self.channel.0;
        let Ok(msg) = encode_request(&req, channel.max_send_size()) else {
            return (ChunkOutcome::Refused, 0);
        };
        if Instant::now() >= deadline {
            return (ChunkOutcome::Refused, 0);
        }
        if !checked {
            return match channel.try_send(msg.data, &msg.fds) {
                Ok(()) => (ChunkOutcome::LegacySubmitted, count),
                Err(_) if channel.is_closed() => (ChunkOutcome::Unknown, count),
                Err(_) => (ChunkOutcome::Refused, 0),
            };
        }
        match channel.call_with_handles_until(msg.data, &msg.fds, deadline) {
            Ok(bytes) => match codec::decode::<bool>(&bytes) {
                Ok(true) => (ChunkOutcome::Confirmed, count),
                Ok(false) => (ChunkOutcome::Refused, 0),
                Err(_) => (ChunkOutcome::Unknown, count),
            },
            Err(DeadlineCallError::Uncertain) => (ChunkOutcome::Unknown, count),
            Err(DeadlineCallError::NotSent) => (ChunkOutcome::Refused, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_accounts_for_the_entire_wire_envelope() {
        let row = FfeExposure {
            timestamp_ms: 0,
            flag_key: "flag".into(),
            subject_id: "user".into(),
            subject_attributes_json: "{}".into(),
            allocation_key: "alloc".into(),
            variant: "v".into(),
            serial_id: Some(0),
            features_json: String::new(),
        };
        let ctx = FfeTelemetryContext {
            service: "service".into(),
            ..Default::default()
        };
        for checked in [false, true] {
            let size = codec::encode(&request(
                FfeExposureBatch {
                    context: ctx.clone(),
                    exposures: vec![row.clone(), row.clone()],
                },
                checked,
            ))
            .len();
            for (limit, expected) in [(size - 1, 1), (size, 2), (size + 1, 2)] {
                let (req, count) = prefix(
                    ctx.clone(),
                    std::iter::repeat_n(Some(row.clone()), 3),
                    checked,
                    limit,
                )
                .unwrap();
                assert_eq!(count, expected);
                assert!(codec::encode(&req).len() <= limit);
            }
            assert!(prefix(ctx.clone(), std::iter::once(Some(row.clone())), checked, 1).is_none());
            assert_eq!(
                prefix(
                    ctx.clone(),
                    [Some(row.clone()), None, Some(row.clone())].into_iter(),
                    checked,
                    size
                )
                .unwrap()
                .1,
                1
            );
        }
    }
}
