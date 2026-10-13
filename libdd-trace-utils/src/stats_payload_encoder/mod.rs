// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Encoding utilities for the top-level [`pb::StatsPayload`] message.
//!
//! Wraps a [`pb::ClientStatsPayload`] into a [`pb::StatsPayload`] and serializes
//! it as msgpack, matching the Agent's `/api/v0.2/stats` wire format. Payloads
//! over [`MAX_GROUPED_STATS_PER_PAYLOAD`] entries are split via
//! [`split_stats_buckets`] and sent one per group.

use libdd_trace_protobuf::pb;

/// Max [`pb::ClientGroupedStats`] entries per stats payload before splitting,
/// matching the Agent (`pkg/trace/writer/stats.go`).
pub const MAX_GROUPED_STATS_PER_PAYLOAD: usize = 4000;

/// Wrap a client stats payload into a top-level [`pb::StatsPayload`].
///
/// Set `split_payload` when this is one fragment of a split flush.
pub fn build_stats_payload(
    payload: pb::ClientStatsPayload,
    hostname: String,
    env: String,
    version: String,
    split_payload: bool,
) -> pb::StatsPayload {
    pb::StatsPayload {
        agent_hostname: hostname,
        agent_env: env,
        stats: vec![payload],
        agent_version: version,
        // `client_computed` is always set to `true` since the stats were computed by the
        // tracer/client, not the Agent
        client_computed: true,
        split_payload,
    }
}

/// Split buckets into groups of at most `max_entries` [`pb::ClientGroupedStats`].
/// An oversized bucket is split across output buckets sharing its `start`,
/// `duration` and `agent_time_shift`. Empty buckets are dropped, so all-empty
/// input yields no groups.
pub fn split_stats_buckets(
    buckets: Vec<pb::ClientStatsBucket>,
    max_entries: usize,
) -> Vec<Vec<pb::ClientStatsBucket>> {
    let max_entries = max_entries.max(1);
    let mut groups: Vec<Vec<pb::ClientStatsBucket>> = Vec::new();
    let mut current: Vec<pb::ClientStatsBucket> = Vec::new();
    let mut current_count = 0usize;

    for bucket in buckets {
        let pb::ClientStatsBucket {
            start,
            duration,
            agent_time_shift,
            mut stats,
        } = bucket;
        while !stats.is_empty() {
            if current_count == max_entries {
                groups.push(std::mem::take(&mut current));
                current_count = 0;
            }
            let take = (max_entries - current_count).min(stats.len());
            let rest = stats.split_off(take);
            let chunk = std::mem::replace(&mut stats, rest);
            current.push(pb::ClientStatsBucket {
                start,
                duration,
                agent_time_shift,
                stats: chunk,
            });
            current_count += take;
        }
    }

    if !current.is_empty() {
        groups.push(current);
    }
    groups
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
            false,
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
            false,
        );

        let encoded = encode_stats_payload_msgpack(&payload).expect("encode should succeed");
        let decoded: pb::StatsPayload =
            rmp_serde::from_slice(&encoded).expect("decode should succeed");

        assert_eq!(decoded, payload);
    }

    fn grouped(resource: &str) -> pb::ClientGroupedStats {
        pb::ClientGroupedStats {
            resource: resource.to_string(),
            ..Default::default()
        }
    }

    fn bucket(start: u64, count: usize) -> pb::ClientStatsBucket {
        pb::ClientStatsBucket {
            start,
            duration: 10,
            agent_time_shift: 0,
            stats: (0..count).map(|i| grouped(&i.to_string())).collect(),
        }
    }

    fn total_stats(groups: &[Vec<pb::ClientStatsBucket>]) -> usize {
        groups
            .iter()
            .flat_map(|g| g.iter())
            .map(|b| b.stats.len())
            .sum()
    }

    #[test]
    fn split_keeps_single_group_when_under_limit() {
        let groups = split_stats_buckets(vec![bucket(0, 3), bucket(10, 2)], 4000);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(total_stats(&groups), 5);
    }

    #[test]
    fn split_breaks_across_multiple_groups() {
        // 5 entries across two buckets, max 2 per payload -> 3 groups (2, 2, 1).
        let groups = split_stats_buckets(vec![bucket(0, 3), bucket(10, 2)], 2);
        assert_eq!(groups.len(), 3);
        for group in &groups {
            let count: usize = group.iter().map(|b| b.stats.len()).sum();
            assert!(count <= 2);
        }
        assert_eq!(total_stats(&groups), 5);
    }

    #[test]
    fn split_splits_an_oversized_bucket_preserving_metadata() {
        // A single bucket of 5 entries, max 2 -> split into 3 output buckets that
        // all share the original start/duration.
        let groups = split_stats_buckets(vec![bucket(42, 5)], 2);
        assert_eq!(groups.len(), 3);
        for group in &groups {
            for b in group {
                assert_eq!(b.start, 42);
                assert_eq!(b.duration, 10);
            }
        }
        assert_eq!(total_stats(&groups), 5);
    }

    #[test]
    fn split_drops_empty_buckets() {
        let groups = split_stats_buckets(vec![bucket(0, 0), bucket(10, 0)], 2);
        assert!(groups.is_empty());
    }

    #[test]
    fn split_handles_zero_max_as_one() {
        let groups = split_stats_buckets(vec![bucket(0, 2)], 0);
        assert_eq!(groups.len(), 2);
        assert_eq!(total_stats(&groups), 2);
    }
}
