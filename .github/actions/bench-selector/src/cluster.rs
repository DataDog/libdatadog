// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Cluster criterion IDs by their first `/`-separated segment within each binary.
//! Criterion does not expose group structure; this heuristic may combine groups
//! whose names contain `/`, conservatively running more benchmarks.

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

pub fn cluster_name(bench_id: &str) -> &str {
    bench_id.split('/').next().unwrap_or(bench_id)
}

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
    /// Match slash-separated and flat IDs, but not similarly prefixed clusters.
    pub fn coverage_filter(&self) -> String {
        format!("^{}(/|$)", escape_regex(&self.name))
    }
}

/// Deterministic filter for exactly the selected IDs in the measuring run.
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
        let filter = exact_filter(&ids(&["b", "a/x", "b"]));
        assert_eq!(filter, "^(a/x|b)$");
    }
}
