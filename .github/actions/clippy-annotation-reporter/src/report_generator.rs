// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generates the compact PR comment from annotation analysis results.
//!
//! Only rows whose count actually changed between the base and PR branches are
//! rendered. The comment is only generated when there is at least one change;
//! see `AnalysisResult::has_changes`.

use crate::analyzer::AnalysisResult;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Generate the PR comment body for the given analysis.
pub fn generate_report(
    analysis: &AnalysisResult,
    rules: &[String],
    repository: &str,
    base_branch: &str,
) -> String {
    let mut report = String::new();

    add_header(&mut report, analysis, repository, base_branch);
    add_rule_table(&mut report, analysis, rules);
    add_details(&mut report, analysis);
    add_explanation(&mut report);

    report
}

/// Add the title and one-line headline with the net change.
fn add_header(report: &mut String, analysis: &AnalysisResult, repository: &str, base_branch: &str) {
    // Display the branch without the `origin/` prefix used internally.
    let branch = base_branch.strip_prefix("origin/").unwrap_or(base_branch);
    let (base_total, head_total) = tracked_totals(analysis);
    let delta = format_delta(head_total as isize - base_total as isize);

    report.push_str("## Clippy Allow Annotation Report\n\n");
    report.push_str(&format!(
        "Tracked Clippy `allow` annotations changed vs [`{}`](https://github.com/{}/tree/{}): {} ({} → {})\n\n",
        branch, repository, branch, delta, base_total, head_total
    ));
}

/// Add the by-rule table, listing only rules whose count changed.
fn add_rule_table(report: &mut String, analysis: &AnalysisResult, rules: &[String]) {
    report.push_str("| Rule | Base | PR | Δ |\n");
    report.push_str("|------|------|----|---|\n");

    for rule in rules {
        let base = *analysis.base_counts.get(rule).unwrap_or(&0);
        let head = *analysis.head_counts.get(rule).unwrap_or(&0);
        if base == head {
            continue;
        }
        report.push_str(&format_row(rule, base, head));
    }

    report.push('\n');
}

/// Add the collapsed by-file and by-crate breakdown, listing only changed rows.
fn add_details(report: &mut String, analysis: &AnalysisResult) {
    let file_rows = build_file_rows(analysis);
    let crate_rows = build_crate_rows(analysis);

    if file_rows.is_empty() && crate_rows.is_empty() {
        return;
    }

    report.push_str("<details>\n<summary>By file and crate</summary>\n\n");

    if !file_rows.is_empty() {
        report.push_str("**By file**\n\n");
        report.push_str("| File | Base | PR | Δ |\n");
        report.push_str("|------|------|----|---|\n");
        report.push_str(&file_rows);
        report.push('\n');
    }

    if !crate_rows.is_empty() {
        report.push_str("**By crate**\n\n");
        report.push_str("| Crate | Base | PR | Δ |\n");
        report.push_str("|-------|------|----|---|\n");
        report.push_str(&crate_rows);
        report.push('\n');
    }

    report.push_str("</details>\n\n");
}

/// Add the report explanation.
fn add_explanation(report: &mut String) {
    report.push_str("### About This Report\n\n");
    report.push_str("This report tracks Clippy allow annotations for specific rules, ");
    report.push_str("showing how they've changed in this PR. ");
    report.push_str("Decreasing the number of these annotations generally improves code quality. ");
    report.push_str("Panic-inducing macros in particular should be avoided. ");
    report.push_str("In the future, this report may become a PR-blocking quality gate.\n");
}

/// Total tracked annotations in the PR's changed files, for base and PR branches.
fn tracked_totals(analysis: &AnalysisResult) -> (usize, usize) {
    let base: usize = analysis.base_counts.values().sum();
    let head: usize = analysis.head_counts.values().sum();
    (base, head)
}

/// Build the changed-file rows for the by-file table (empty string if none).
fn build_file_rows(analysis: &AnalysisResult) -> String {
    let base_counts = count_annotations_by_file(&analysis.base_annotations);
    let head_counts = count_annotations_by_file(&analysis.head_annotations);

    let mut files: Vec<String> = analysis.changed_files.iter().cloned().collect();
    files.sort();

    let mut rows = String::new();
    for file in files {
        let base = *base_counts.get(&file).unwrap_or(&0);
        let head = *head_counts.get(&file).unwrap_or(&0);
        if base == head {
            continue;
        }
        rows.push_str(&format_row(&format!("`{}`", file), base, head));
    }
    rows
}

/// Build the changed-crate rows for the by-crate table (empty string if none).
fn build_crate_rows(analysis: &AnalysisResult) -> String {
    let crates = get_all_keys(&analysis.base_crate_counts, &analysis.head_crate_counts);

    let mut rows = String::new();
    for crate_name in crates {
        let base = *analysis.base_crate_counts.get(&crate_name).unwrap_or(&0);
        let head = *analysis.head_crate_counts.get(&crate_name).unwrap_or(&0);
        if base == head {
            continue;
        }
        rows.push_str(&format_row(&format!("`{}`", crate_name), base, head));
    }
    rows
}

/// Format a single table row with counts and change indicator.
fn format_row(label: &str, base: usize, head: usize) -> String {
    let delta = format_delta(head as isize - base as isize);
    format!("| {} | {} | {} | {} |\n", label, base, head, delta)
}

/// Format a signed change with an icon: `⚠️ +N` for increases, `✅ -N` for
/// decreases, `↔ ±0` when the net is unchanged.
fn format_delta(change: isize) -> String {
    match change.cmp(&0) {
        Ordering::Greater => format!("⚠️ +{}", change),
        Ordering::Less => format!("✅ {}", change),
        Ordering::Equal => "↔ ±0".to_owned(),
    }
}

/// Count annotations by file.
fn count_annotations_by_file(
    annotations: &[crate::analyzer::ClippyAnnotation],
) -> HashMap<Rc<String>, usize> {
    let mut counts = HashMap::new();

    for anno in annotations {
        *counts.entry(anno.file.clone()).or_insert(0) += 1;
    }

    counts
}

/// Get all unique keys from two HashMaps, sorted.
fn get_all_keys<K: Clone + Ord + std::hash::Hash, V>(
    map1: &HashMap<K, V>,
    map2: &HashMap<K, V>,
) -> Vec<K> {
    let mut all_keys = HashSet::new();

    for key in map1.keys() {
        all_keys.insert(key.clone());
    }

    for key in map2.keys() {
        all_keys.insert(key.clone());
    }

    let mut keys_vec: Vec<K> = all_keys.into_iter().collect();
    keys_vec.sort();

    keys_vec
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::ClippyAnnotation;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    // Helper function to create a test AnalysisResult
    fn create_analysis_result() -> crate::analyzer::AnalysisResult {
        let mut base_counts = HashMap::new();
        let rule1 = Rc::new("clippy::unwrap_used".to_owned());
        let rule2 = Rc::new("clippy::match_bool".to_owned());
        let rule3 = Rc::new("clippy::unused_imports".to_owned());

        base_counts.insert(rule1.clone(), 5);
        base_counts.insert(rule2.clone(), 3);
        base_counts.insert(rule3.clone(), 10);

        let mut head_counts = HashMap::new();
        head_counts.insert(rule1.clone(), 3);
        head_counts.insert(rule2.clone(), 4);
        head_counts.insert(rule3.clone(), 5);

        let mut base_crate_counts = HashMap::new();
        let crate1 = Rc::new("crate1".to_owned());
        let crate2 = Rc::new("crate2".to_owned());

        base_crate_counts.insert(crate1.clone(), 8);
        base_crate_counts.insert(crate2.clone(), 10);

        let mut head_crate_counts = HashMap::new();
        head_crate_counts.insert(crate1.clone(), 5);
        head_crate_counts.insert(crate2.clone(), 12);

        let mut changed_files = HashSet::new();
        changed_files.insert("src/file1.rs".to_owned());
        changed_files.insert("src/file2.rs".to_owned());

        let file1 = Rc::new("src/file1.rs".to_owned());
        let file2 = Rc::new("src/file2.rs".to_owned());

        let base_annotations = vec![
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule2.clone(),
            },
            ClippyAnnotation {
                file: file2.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file2.clone(),
                rule: rule3.clone(),
            },
        ];

        let head_annotations = vec![
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule2.clone(),
            },
            ClippyAnnotation {
                file: file1.clone(),
                rule: rule2.clone(),
            },
            ClippyAnnotation {
                file: file2.clone(),
                rule: rule3.clone(),
            },
        ];

        crate::analyzer::AnalysisResult {
            base_annotations,
            head_annotations,
            base_counts,
            head_counts,
            changed_files,
            base_crate_counts,
            head_crate_counts,
        }
    }

    fn default_rules() -> Vec<String> {
        vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ]
    }

    #[test]
    fn test_generate_report_structure() {
        let analysis = create_analysis_result();
        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        // Title, rule table, collapsed details and explanation are present.
        assert!(report.contains("## Clippy Allow Annotation Report"));
        assert!(report.contains("| Rule | Base | PR | Δ |"));
        assert!(report.contains("<details>"));
        assert!(report.contains("By file and crate"));
        assert!(report.contains("### About This Report"));

        // Repository and base branch information appear in the headline link.
        assert!(report.contains("test-owner/test-repo"));
        assert!(report.contains("[`main`]"));
    }

    #[test]
    fn test_generate_report_headline_total() {
        let analysis = create_analysis_result();
        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        // Base total 18 → head total 12, a net decrease of 6.
        assert!(report.contains("✅ -6 (18 → 12)"));
    }

    #[test]
    fn test_generate_report_only_changed_rules() {
        let analysis = create_analysis_result();
        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        // Every tracked rule changed, so all three appear with signed deltas.
        assert!(report.contains("| clippy::unwrap_used | 5 | 3 | ✅ -2 |"));
        assert!(report.contains("| clippy::match_bool | 3 | 4 | ⚠️ +1 |"));
        assert!(report.contains("| clippy::unused_imports | 10 | 5 | ✅ -5 |"));
    }

    #[test]
    fn test_generate_report_skips_unchanged_rule() {
        let mut analysis = create_analysis_result();
        // Make match_bool unchanged (3 in both branches).
        let rule2 = Rc::new("clippy::match_bool".to_owned());
        analysis.head_counts.insert(rule2, 3);

        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        assert!(report.contains("| clippy::unwrap_used | 5 | 3 | ✅ -2 |"));
        assert!(
            !report.contains("clippy::match_bool"),
            "unchanged rule should be omitted from the table"
        );
    }

    #[test]
    fn test_generate_report_only_changed_files() {
        let analysis = create_analysis_result();
        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        // file2 changed (2 → 1); file1 did not (3 → 3) and must be omitted.
        assert!(report.contains("| `src/file2.rs` | 2 | 1 | ✅ -1 |"));
        assert!(
            !report.contains("`src/file1.rs`"),
            "unchanged file should be omitted from the table"
        );
    }

    #[test]
    fn test_generate_report_only_changed_crates() {
        let analysis = create_analysis_result();
        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        assert!(report.contains("| `crate1` | 8 | 5 | ✅ -3 |"));
        assert!(report.contains("| `crate2` | 10 | 12 | ⚠️ +2 |"));
    }

    #[test]
    fn test_generate_report_empty_changed_files() {
        let mut analysis = create_analysis_result();
        analysis.changed_files.clear();

        let report = generate_report(&analysis, &default_rules(), "test-owner/test-repo", "main");

        // No changed files means no by-file section, but the by-crate section
        // and rule table remain.
        assert!(!report.contains("**By file**"));
        assert!(report.contains("**By crate**"));
        assert!(report.contains("| Rule | Base | PR | Δ |"));
    }

    #[test]
    fn test_generate_report_with_origin_prefix() {
        let analysis = create_analysis_result();
        let report = generate_report(
            &analysis,
            &default_rules(),
            "test-owner/test-repo",
            "origin/main",
        );

        // The origin/ prefix is stripped for both the label and the URL.
        assert!(report.contains("https://github.com/test-owner/test-repo/tree/main"));
        assert!(report.contains("[`main`]"));
        assert!(!report.contains("origin/main"));
    }

    #[test]
    fn test_generate_report_new_annotations() {
        let mut analysis = create_analysis_result();

        // A rule that only appears in the head branch (0 → 2).
        let new_rule = Rc::new("clippy::new_rule".to_owned());
        analysis.head_counts.insert(new_rule, 2);

        let mut rules = default_rules();
        rules.push("clippy::new_rule".to_owned());

        let report = generate_report(&analysis, &rules, "test-owner/test-repo", "main");

        assert!(report.contains("| clippy::new_rule | 0 | 2 | ⚠️ +2 |"));
    }

    #[test]
    fn test_format_delta() {
        assert_eq!(format_delta(3), "⚠️ +3");
        assert_eq!(format_delta(-2), "✅ -2");
        assert_eq!(format_delta(0), "↔ ±0");
    }
}
