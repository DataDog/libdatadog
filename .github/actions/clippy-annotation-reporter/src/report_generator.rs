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
