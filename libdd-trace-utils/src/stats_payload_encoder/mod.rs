// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Encoding utilities for the top-level [`pb::StatsPayload`] message.
//!
//! This module wraps a single client [`pb::ClientStatsPayload`] into a
//! [`pb::StatsPayload`] and serializes it as msgpack, matching the wire format
//! the Datadog Agent uses when sending stats to the `/api/v0.2/stats` intake.
//!
//! Unlike the Agent's stats writer, we do **not** split payloads at 4000
//! entries: a single client cannot exceed that limit, so we always emit one
//! [`pb::StatsPayload`] with `split_payload = false`.

use libdd_trace_protobuf::pb;

/// Wrap a single client stats payload into a top-level [`pb::StatsPayload`].
pub fn build_stats_payload(
    payload: pb::ClientStatsPayload,
    hostname: String,
    env: String,
    version: String,
) -> pb::StatsPayload {
    pb::StatsPayload {
        agent_hostname: hostname,
        agent_env: env,
        stats: vec![payload],
        agent_version: version,
        // `client_computed` is always set to `true` since the stats were computed by the
        // tracer/client, not the Agent
        client_computed: true,
        split_payload: false,
    }
}

/// Serialize a [`pb::StatsPayload`] as msgpack (with named fields), matching the
/// encoding accepted by the `/api/v0.2/stats` intake.
pub fn encode_stats_payload_msgpack(
    payload: &pb::StatsPayload,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_protobuf::pb;

    fn sample_client_payload() -> pb::ClientStatsPayload {
        pb::ClientStatsPayload {
            hostname: "client-host".to_string(),
            env: "test".to_string(),
            version: "1.0.0".to_string(),
            stats: vec![pb::ClientStatsBucket {
                start: 0,
                duration: 10_000_000_000,
                stats: vec![pb::ClientGroupedStats {
                    service: "svc".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    hits: 3,
                    top_level_hits: 3,
                    duration: 42,
                    ..Default::default()
                }],
                agent_time_shift: 0,
            }],
            lang: "rust".to_string(),
            tracer_version: "0.0.0".to_string(),
            runtime_id: "00000000-0000-0000-0000-000000000000".to_string(),
            sequence: 1,
            ..Default::default()
        }
    }

    #[test]
    fn build_wraps_single_payload() {
        let client = sample_client_payload();
        let payload = build_stats_payload(
            client.clone(),
            "host-a".to_string(),
            "prod".to_string(),
            "1.2.3-libdatadog".to_string(),
        );

        assert_eq!(payload.agent_hostname, "host-a");
        assert_eq!(payload.agent_env, "prod");
        assert_eq!(payload.agent_version, "1.2.3-libdatadog");
        assert!(payload.client_computed);
        assert!(!payload.split_payload);
        assert_eq!(payload.stats.len(), 1);
        assert_eq!(payload.stats[0], client);
    }

    #[test]
    fn encode_roundtrips_through_msgpack() {
        let payload = build_stats_payload(
            sample_client_payload(),
            "host-a".to_string(),
            "prod".to_string(),
            "1.2.3-libdatadog".to_string(),
        );

        let encoded = encode_stats_payload_msgpack(&payload).expect("encode should succeed");
        let decoded: pb::StatsPayload =
            rmp_serde::from_slice(&encoded).expect("decode should succeed");

        assert_eq!(decoded, payload);
    }
}
