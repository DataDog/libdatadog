// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Report generator module for clippy-annotation-reporter
//!
//! This module handles the logic for generating formatted reports
//! based on annotation analysis results.

use crate::analyzer::AnalysisResult;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Generate a detailed report for PR comment
pub fn generate_report(
    analysis: &AnalysisResult,
    rules: &[String],
    repository: &str,
    base_branch: &str,
    head_branch: &str,
) -> String {
    let mut report = String::new();

    // Add header and branch information
    add_header(&mut report, repository, base_branch, head_branch);

    // Add rule summary section
    add_rule_summary(&mut report, analysis, rules);

    // Add file-level section
    add_file_level_section(&mut report, analysis);

    // Add crate-level section
    add_crate_level_section(&mut report, analysis);

    // Add explanation
    add_explanation(&mut report);

    report
}

/// Add report header and branch information
fn add_header(report: &mut String, repository: &str, base_branch: &str, head_branch: &str) {
    report.push_str("## Clippy Allow Annotation Report\n\n");

    // Add branch information with link to base branch
    let base_branch_for_url = base_branch.strip_prefix("origin/").unwrap_or(base_branch);

    report.push_str("Comparing clippy allow annotations between branches:\n");
    report.push_str(&format!(
        "- **Base Branch**: [{}](https://github.com/{}/tree/{})\n",
        base_branch, repository, base_branch_for_url
    ));
    report.push_str(&format!("- **PR Branch**: {}\n\n", head_branch));
}

/// Add summary table by rule
fn add_rule_summary(report: &mut String, analysis: &AnalysisResult, rules: &[String]) {
    report.push_str("### Summary by Rule\n\n");
    report.push_str("| Rule | Base Branch | PR Branch | Change |\n");
    report.push_str("|------|------------|-----------|--------|\n");

    let mut total_base = 0;
    let mut total_head = 0;

    // Add row for each rule
    for rule in rules {
        let base_count = *analysis.base_counts.get(rule).unwrap_or(&0);
        let head_count = *analysis.head_counts.get(rule).unwrap_or(&0);

        total_base += base_count;
        total_head += head_count;

        add_table_row(report, rule, base_count, head_count);
    }

    // Add total row
    add_table_row(report, "**Total**", total_base, total_head);
    report.push('\n');
}

/// Add section showing annotation counts by file
fn add_file_level_section(report: &mut String, analysis: &AnalysisResult) {
    if analysis.changed_files.is_empty() {
        return;
    }

    report.push_str("### Annotation Counts by File\n\n");
    report.push_str("| File | Base Branch | PR Branch | Change |\n");
    report.push_str("|------|------------|-----------|--------|\n");

    // Count annotations by file
    let base_file_counts = count_annotations_by_file(&analysis.base_annotations);
    let head_file_counts = count_annotations_by_file(&analysis.head_annotations);

    // Get sorted list of changed files
    let mut all_files: Vec<String> = analysis.changed_files.iter().cloned().collect();
    all_files.sort();

    // Add row for each file
    for file in all_files {
        let base_count = *base_file_counts.get(&file).unwrap_or(&0);
        let head_count = *head_file_counts.get(&file).unwrap_or(&0);

        // Skip files with no annotations in either branch
        if base_count == 0 && head_count == 0 {
            continue;
        }

        add_table_row(report, &format!("`{}`", file), base_count, head_count);
    }

    report.push('\n');
}

/// Add section showing annotation stats by crate
fn add_crate_level_section(report: &mut String, analysis: &AnalysisResult) {
    report.push_str("### Annotation Stats by Crate\n\n");
    report.push_str("| Crate | Base Branch | PR Branch | Change |\n");
    report.push_str("|-------|------------|-----------|--------|\n");

    // Get all crates from both base and head
    let all_crates = get_all_keys(&analysis.base_crate_counts, &analysis.head_crate_counts);

    let mut total_base = 0;
    let mut total_head = 0;

    // Add row for each crate
    for crate_name in all_crates {
        let base_count = *analysis.base_crate_counts.get(&crate_name).unwrap_or(&0);
        let head_count = *analysis.head_crate_counts.get(&crate_name).unwrap_or(&0);

        // Skip crates with no annotations in either branch
        if base_count == 0 && head_count == 0 {
            continue;
        }

        total_base += base_count;
        total_head += head_count;

        add_table_row(report, &format!("`{}`", crate_name), base_count, head_count);
    }

    // Add total row
    add_table_row(report, "**Total**", total_base, total_head);
    report.push('\n');
}

/// Add report explanation footer
fn add_explanation(report: &mut String) {
    report.push_str("### About This Report\n\n");
    report.push_str("This report tracks Clippy allow annotations for specific rules, ");
    report.push_str("showing how they've changed in this PR. ");
    report
        .push_str("Decreasing the number of these annotations generally improves code quality.\n");
}

/// Add a table row with counts and change
fn add_table_row(report: &mut String, label: &str, base_count: usize, head_count: usize) {
    let change = head_count as isize - base_count as isize;

    // Skip rows with no change and no counts
    if change == 0 && base_count == 0 && head_count == 0 {
        return;
    }

    // Calculate percentage change
    let percent_change = calculate_percent_change(base_count, change);

    // Format the change string with percentage
    let change_str = format_change_string(change, percent_change);

    report.push_str(&format!(
        "| {} | {} | {} | {} |\n",
        label, base_count, head_count, change_str
    ));
}

/// Calculate percentage change
fn calculate_percent_change(base_count: usize, change: isize) -> f64 {
    if base_count > 0 {
        (change as f64 / base_count as f64) * 100.0
    } else if change > 0 {
        f64::INFINITY
    } else {
        0.0
    }
}

/// Format change string with appropriate icon and percentage
fn format_change_string(change: isize, percent_change: f64) -> String {
    if change > 0 {
        if percent_change.is_infinite() {
            format!("⚠️ +{} (N/A)", change)
        } else {
            format!("⚠️ +{} (+{:.1}%)", change, percent_change)
        }
    } else if change < 0 {
        format!("✅ {} ({:.1}%)", change, percent_change)
    } else {
        "No change (0%)".to_owned()
    }
}

/// Count annotations by file
fn count_annotations_by_file(
    annotations: &[crate::analyzer::ClippyAnnotation],
) -> HashMap<Rc<String>, usize> {
    let mut counts = HashMap::new();

    for anno in annotations {
        *counts.entry(anno.file.clone()).or_insert(0) += 1;
    }

    counts
}

/// Get all unique keys from two HashMaps, sorted
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

    #[test]
    fn test_generate_report_basic() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify the report contains expected sections
        assert!(report.contains("## Clippy Allow Annotation Report"));
        assert!(report.contains("### Summary by Rule"));
        assert!(report.contains("### Annotation Counts by File"));
        assert!(report.contains("### Annotation Stats by Crate"));
        assert!(report.contains("### About This Report"));

        // Verify the report contains repository and branch information
        assert!(report.contains("test-owner/test-repo"));
        assert!(report.contains("main"));
        assert!(report.contains("feature-branch"));
    }

    #[test]
    fn test_generate_report_rule_summary() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify rule summary contains all rules
        assert!(report.contains("clippy::unwrap_used"));
        assert!(report.contains("clippy::match_bool"));
        assert!(report.contains("clippy::unused_imports"));

        // Verify counts and changes
        assert!(report.contains("5")); // Base count for unwrap_used
        assert!(report.contains("3")); // Head count for unwrap_used
        assert!(report.contains("-2")); // Change for unwrap_used

        assert!(report.contains("3")); // Base count for match_bool
        assert!(report.contains("4")); // Head count for match_bool
        assert!(report.contains("+1")); // Change for match_bool

        assert!(report.contains("10")); // Base count for unused_imports
        assert!(report.contains("5")); // Head count for unused_imports
        assert!(report.contains("-5")); // Change for unused_imports
    }

    #[test]
    fn test_generate_report_file_section() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify file section contains the changed files
        assert!(report.contains("src/file1.rs"));
        assert!(report.contains("src/file2.rs"));

        // Verify file counts for file1.rs
        // In the base branch, file1.rs has 3 annotations (2 unwrap_used, 1 match_bool)
        // In the head branch, file1.rs has 3 annotations (1 unwrap_used, 2 match_bool)
        let file1_pattern = r"`src/file1\.rs`\s*\|\s*3\s*\|\s*3\s*\|\s*No change";
        assert!(
            report.contains("| `src/file1.rs` | 3 | 3 |")
                || regex::Regex::new(file1_pattern).unwrap().is_match(&report),
            "File1 count information not found in report"
        );

        // Verify file counts for file2.rs
        // In the base branch, file2.rs has 2 annotations (1 unwrap_used, 1 unused_imports)
        // In the head branch, file2.rs has 1 annotation (1 unused_imports)
        let file2_pattern = r"`src/file2\.rs`\s*\|\s*2\s*\|\s*1\s*\|\s*.*-1";
        assert!(
            report.contains("| `src/file2.rs` | 2 | 1 |")
                || regex::Regex::new(file2_pattern).unwrap().is_match(&report),
            "File2 count information not found in report"
        );

        // Make sure the change column has the correct indicators
        assert!(
            report.contains("No change") || report.contains("(0%)"),
            "No change indicator missing for file1"
        );
        assert!(
            report.contains("✅ -1"),
            "Decrease indicator missing for file2"
        );
    }

    #[test]
    fn test_generate_report_crate_section() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify crate section contains the crates
        assert!(report.contains("`crate1`"));
        assert!(report.contains("`crate2`"));

        // Verify crate counts
        // Base count for crate1: 8, Head count: 5
        assert!(report.contains("8"));
        assert!(report.contains("5"));
        assert!(report.contains("-3")); // Change

        // Base count for crate2: 10, Head count: 12
        assert!(report.contains("10"));
        assert!(report.contains("12"));
        assert!(report.contains("+2")); // Change
    }

    #[test]
    fn test_generate_report_empty_changed_files() {
        let mut analysis = create_analysis_result();
        analysis.changed_files.clear();

        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify that the file-level section is not included when there are no changed files
        assert!(!report.contains("### Annotation Counts by File"));

        // But other sections should still be present
        assert!(report.contains("### Summary by Rule"));
        assert!(report.contains("### Annotation Stats by Crate"));
    }

    #[test]
    fn test_generate_report_formatting() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify positive changes are formatted with ⚠️
        assert!(report.contains("⚠️ +1"));

        // Verify negative changes are formatted with ✅
        assert!(report.contains("✅ -2"));

        // Verify total row exists
        assert!(report.contains("**Total**"));
    }

    #[test]
    fn test_generate_report_with_origin_prefix() {
        let analysis = create_analysis_result();
        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
        ];

        // Test with origin/ prefix in base branch
        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "origin/main",
            "feature-branch",
        );

        // Verify the origin/ prefix is removed in the link URL
        assert!(report.contains("https://github.com/test-owner/test-repo/tree/main"));
        assert!(report.contains("origin/main"));
    }

    #[test]
    fn test_generate_report_new_annotations() {
        // Create an analysis where annotations are added in the head branch
        let mut analysis = create_analysis_result();

        // Add a new rule that only appears in the head counts
        let new_rule = Rc::new("clippy::new_rule".to_owned());
        analysis.head_counts.insert(new_rule.clone(), 2);

        let rules = vec![
            "clippy::unwrap_used".to_owned(),
            "clippy::match_bool".to_owned(),
            "clippy::unused_imports".to_owned(),
            "clippy::new_rule".to_owned(),
        ];

        let report = generate_report(
            &analysis,
            &rules,
            "test-owner/test-repo",
            "main",
            "feature-branch",
        );

        // Verify that new rule appears with appropriate formatting
        assert!(report.contains("clippy::new_rule"));
        assert!(report.contains("0")); // Base count should be 0
        assert!(report.contains("2")); // Head count should be 2
        assert!(report.contains("⚠️ +2")); // Change should be +2

        // Also verify N/A for the percentage since base count is 0
        assert!(report.contains("N/A"));
    }
}
