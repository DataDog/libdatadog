//! Report generator module for clippy-annotation-reporter
//!
//! This module handles the logic for generating formatted reports
//! based on annotation analysis results.

use crate::analyzer::AnalysisResult;
use std::collections::{HashMap, HashSet};

/// Generate a detailed report for PR comment
pub fn generate_report(
    analysis: &AnalysisResult,
    rules: &[String],
    repository: &str,
    base_branch: &str,
    head_branch: &str,
) -> String {
    let mut report = String::from("## Clippy Allow Annotation Report\n\n");

    // Add branch information with link to base branch
    let base_branch_for_url = base_branch.strip_prefix("origin/").unwrap_or(base_branch);

    report.push_str("Comparing clippy allow annotations between branches:\n");
    report.push_str(&format!(
        "- **Base Branch**: [{}](https://github.com/{}/tree/{})\n",
        base_branch, repository, base_branch_for_url
    ));
    report.push_str(&format!("- **PR Branch**: {}\n\n", head_branch));

    // Summary table by rule
    report.push_str("### Summary by Rule\n\n");
    report.push_str("| Rule | Base Branch | PR Branch | Change |\n");
    report.push_str("|------|------------|-----------|--------|\n");

    // ... existing rule summary code ...

    // File-level annotation counts with percentage change
    if !analysis.changed_files.is_empty() {
        report.push_str("### Annotation Counts by File\n\n");
        report.push_str("| File | Base Branch | PR Branch | Change |\n");
        report.push_str("|------|------------|-----------|--------|\n");

        // ... existing file-level code ...
    }

    // NEW SECTION: Crate-level statistics
    report.push_str("### Annotation Stats by Crate\n\n");
    report.push_str("| Crate | Base Branch | PR Branch | Change |\n");
    report.push_str("|-------|------------|-----------|--------|\n");

    // Get all crates from both base and head
    let mut all_crates = HashSet::new();
    for crate_name in analysis.base_crate_counts.keys() {
        all_crates.insert(crate_name.clone());
    }
    for crate_name in analysis.head_crate_counts.keys() {
        all_crates.insert(crate_name.clone());
    }

    // Sort crates alphabetically
    let mut crates: Vec<String> = all_crates.into_iter().collect();
    crates.sort();

    let mut total_base = 0;
    let mut total_head = 0;

    for crate_name in crates {
        let base_count = *analysis.base_crate_counts.get(&crate_name).unwrap_or(&0);
        let head_count = *analysis.head_crate_counts.get(&crate_name).unwrap_or(&0);
        let change = head_count as isize - base_count as isize;

        total_base += base_count;
        total_head += head_count;

        // Skip crates with no annotations in either branch
        if base_count == 0 && head_count == 0 {
            continue;
        }

        // Calculate percentage change
        let percent_change = if base_count > 0 {
            (change as f64 / base_count as f64) * 100.0
        } else if change > 0 {
            f64::INFINITY
        } else {
            0.0
        };

        // Format the change string with percentage
        let change_str = if change > 0 {
            if percent_change.is_infinite() {
                format!("⚠️ +{} (N/A)", change)
            } else {
                format!("⚠️ +{} (+{:.1}%)", change, percent_change)
            }
        } else if change < 0 {
            format!("✅ {} ({:.1}%)", change, percent_change)
        } else {
            "No change (0%)".to_string()
        };

        report.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            crate_name, base_count, head_count, change_str
        ));
    }

    report.push_str("\n");

    // Add explanation
    report.push_str("### About This Report\n\n");
    report.push_str("This report tracks Clippy allow annotations for specific rules, ");
    report.push_str("showing how they've changed in this PR. ");
    report
        .push_str("Decreasing the number of these annotations generally improves code quality.\n");

    report
}
