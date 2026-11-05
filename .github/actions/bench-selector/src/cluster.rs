// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Clustering of criterion benchmark IDs.
//!
//! Selection happens at two levels:
//!
//! 1. **Bench binary** — every `[[bench]]` target is built and run as its own process, so it is the
//!    outer selection unit (see `inventory::BenchBinary`).
//! 2. **Name cluster** — within one binary, benchmark IDs produced by criterion `--list` are flat
//!    strings of the form `group/function/value` (or plain `function` for `bench_function` calls,
//!    or `group/value` when no function id is set). Criterion has no API that reports the true
//!    `group_id` structure, so clusters are derived by taking the first `/`-separated segment of
//!    the ID.
//!
//! The derivation is a *heuristic that can only coarsen*: a true group whose
//! name contains a `/` (e.g. `benchmark_group(format!("span_pool/{alloc}"))`
//! yields the true groups `span_pool/mimalloc`, `span_pool/system`) clusters as
//! `span_pool`, and a flat `bench_function("otlp/encode_protobuf/{id}")` ID
//! clusters as `otlp`. A coarser cluster only means more benchmarks run, never
//! fewer, which is the safe direction for a benchmark-selection tool.

/// Characters that are regex metacharacters outside of character classes in
/// the `regex` crate (which criterion uses for its benchmark filter). Anything
/// else in a benchmark ID — including `/`, `:`, `=`, `,` — is a literal.
const REGEX_METACHARACTERS: &[char] = &[
    '\\', '.', '^', '$', '|', '(', ')', '[', ']', '{', '}', '*', '+', '?',
];

/// Escape `s` so criterion's regex benchmark filter matches it literally.
pub fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if REGEX_METACHARACTERS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The name cluster of a benchmark ID: its first `/`-separated segment, or the
/// whole ID when it has no `/` (flat `bench_function` ID).
pub fn cluster_name(bench_id: &str) -> &str {
    bench_id.split('/').next().unwrap_or(bench_id)
}

/// A name cluster within one bench binary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cluster {
    pub name: String,
    pub bench_ids: Vec<String>,
}

/// Group benchmark IDs into name clusters, sorted by cluster name.
pub fn clusters_of(bench_ids: &[String]) -> Vec<Cluster> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for id in bench_ids {
        map.entry(cluster_name(id).to_string())
            .or_default()
            .push(id.clone());
    }
    map.into_iter()
        .map(|(name, mut ids)| {
            ids.sort();
            Cluster {
                name,
                bench_ids: ids,
            }
        })
        .collect()
}

impl Cluster {
    /// Regex filter that matches every ID of this cluster and nothing else.
    ///
    /// `^<name>(/|$)` instead of a plain prefix match so the cluster `span_pool`
    /// does not also swallow `span_pool_v1/...` IDs from the same binary, and
    /// flat IDs (no `/`) whose whole text is the cluster name still match.
    pub fn coverage_filter(&self) -> String {
        format!("^{}(/|$)", escape_regex(&self.name))
    }
}

/// Regex filter that matches exactly the given benchmark IDs and nothing else.
///
/// Used for the real (measuring) benchmark run so the executed set is exact,
/// regardless of how the IDs were clustered for the coverage pass. The output
/// is sorted and deduplicated so the filter is deterministic for a given ID
/// set, independent of input order.
pub fn exact_filter(bench_ids: &[String]) -> String {
    let mut sorted: Vec<String> = bench_ids.to_vec();
    sorted.sort();
    sorted.dedup();
    let escaped: Vec<String> = sorted.iter().map(|id| escape_regex(id)).collect();
    format!("^({})$", escaped.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cluster_name_first_segment() {
        assert_eq!(cluster_name("ddsketch_add/add/uniform"), "ddsketch_add");
        assert_eq!(
            cluster_name("span_pool/mimalloc/enqueue_dequeue"),
            "span_pool"
        );
    }

    #[test]
    fn cluster_name_flat_id_is_itself() {
        assert_eq!(
            cluster_name("msgpack_encoder::v04::to_vec_with_capacity_from_v04"),
            "msgpack_encoder::v04::to_vec_with_capacity_from_v04"
        );
    }

    #[test]
    fn clusters_group_and_sort() {
        let clusters = clusters_of(&ids(&["b/2", "a/y", "a/x", "flat", "b/1"]));
        let names: Vec<&str> = clusters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "flat"]);
        assert_eq!(clusters[0].bench_ids, ["a/x", "a/y"]);
        assert_eq!(clusters[1].bench_ids, ["b/1", "b/2"]);
        assert_eq!(clusters[2].bench_ids, ["flat"]);
    }

    #[test]
    fn coverage_filter_matches_slash_ids_and_flat_ids() {
        let cluster = Cluster {
            name: "span_pool".to_string(),
            bench_ids: ids(&["span_pool/mimalloc/x", "span_pool/system/y"]),
        };
        assert_eq!(cluster.coverage_filter(), "^span_pool(/|$)");

        let flat = Cluster {
            name: "flat_bench".to_string(),
            bench_ids: ids(&["flat_bench"]),
        };
        assert_eq!(flat.coverage_filter(), "^flat_bench(/|$)");
    }

    #[test]
    fn coverage_filter_does_not_swallow_similar_prefixes() {
        let cluster = Cluster {
            name: "span_pool".to_string(),
            bench_ids: ids(&["span_pool/x"]),
        };
        let filter = cluster.coverage_filter();
        // span_pool_v1 must not match: after `span_pool` comes `_v1`, not `/` or end.
        assert!(!regex_like_match(&filter, "span_pool_v1/y"));
        assert!(regex_like_match(&filter, "span_pool/y"));
        assert!(regex_like_match(&filter, "span_pool"));

        // Verify against the real regex crate semantics by construction: the
        // escaped pattern only introduces literals; a simple manual check is
        // enough here (the regex crate itself is exercised in CI via criterion).
        fn regex_like_match(pattern: &str, id: &str) -> bool {
            let body = pattern
                .strip_prefix('^')
                .and_then(|p| p.strip_suffix("(/|$)"))
                .expect("expected ^...(/|$) pattern");
            id == body || id.starts_with(&format!("{body}/"))
        }
    }

    #[test]
    fn escape_regex_escapes_metacharacters_only() {
        assert_eq!(
            escape_regex("span_pool/mimalloc"),
            "span_pool/mimalloc",
            "plain punctuation stays literal"
        );
        assert_eq!(escape_regex("a.b*c[d]"), r"a\.b\*c\[d\]");
        assert_eq!(escape_regex("a|b"), r"a\|b");
        assert_eq!(escape_regex(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn exact_filter_alternates_all_ids() {
        let filter = exact_filter(&ids(&["a/x", "b"]));
        assert_eq!(filter, "^(a/x|b)$");
    }
}
