// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Main analyzer module for clippy-annotation-reporter
//!
//! This module is responsible for analyzing clippy annotations
//! in Rust code across different branches.

use crate::analyzer::annotation::{
    count_annotations_by_crate, count_annotations_by_rule, create_annotation_regex,
    find_annotations,
};
use crate::analyzer::git::{get_changed_files, GitOperations};
use anyhow::Result;
use log::{debug, info};
use octocrab::Octocrab;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

mod annotation;
mod git;

/// Map of a rule or crate name to the number of annotations counted for it.
type AnnotationCounts = HashMap<Rc<String>, usize>;

/// Represents a clippy annotation in code
#[derive(Debug, Clone)]
pub struct ClippyAnnotation {
    pub file: Rc<String>,
    pub rule: Rc<String>,
}

/// Result of annotation analysis
pub struct AnalysisResult {
    pub base_annotations: Vec<ClippyAnnotation>,
    pub head_annotations: Vec<ClippyAnnotation>,
    pub base_counts: HashMap<Rc<String>, usize>,
    pub head_counts: HashMap<Rc<String>, usize>,
    pub changed_files: HashSet<String>,
    pub base_crate_counts: HashMap<Rc<String>, usize>,
    pub head_crate_counts: HashMap<Rc<String>, usize>,
}

impl AnalysisResult {
    /// Whether the number of tracked annotations differs between the base and
    /// PR branches. When this is false there is nothing to report.
    pub fn has_changes(&self) -> bool {
        self.base_counts != self.head_counts
    }
}

pub async fn run_analysis(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<Option<AnalysisResult>> {
    let changed_files = get_changed_files(octocrab, owner, repo, pr_number).await?;

    // No Rust files changed is a valid outcome, not an error: there is simply
    // nothing to analyze.
    if changed_files.is_empty() {
        return Ok(None);
    }
    let git_ops = GitOperations::default();

    let all_files = git_ops.get_all_rust_files()?;
    let pr_analysis = analyze_annotations(&changed_files, base_branch, head_branch, rules)?;
    let (repo_base_crate_counts, repo_head_crate_counts) =
        analyze_all_files_for_crates(&all_files, base_branch, head_branch, rules)?;

    Ok(Some(AnalysisResult {
        base_annotations: pr_analysis.base_annotations,
        head_annotations: pr_analysis.head_annotations,
        base_counts: pr_analysis.base_counts,
        head_counts: pr_analysis.head_counts,
        changed_files: pr_analysis.changed_files,
        base_crate_counts: repo_base_crate_counts,
        head_crate_counts: repo_head_crate_counts,
    }))
}
/// Analyze clippy annotations in base and head branches
fn analyze_annotations(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<AnalysisResult> {
    debug!("Analyzing clippy annotations in {} files...", files.len());

    // Create a regex for matching clippy allow annotations
    let annotation_regex = create_annotation_regex(rules)?;

    let mut base_annotations = Vec::new();
    let mut head_annotations = Vec::new();
    let mut changed_files = HashSet::new();

    // Cache for rule Rc instances to avoid duplicates
    let mut rule_cache = HashMap::new();
    let git_ops = GitOperations::default();
    for file in files {
        changed_files.insert(file.clone());

        // A file added or removed in the PR won't exist in one of the branches;
        // `get_file_content` returns `None` for that, which we treat as empty.
        let base_content = git_ops
            .get_file_content(file, base_branch)?
            .unwrap_or_default();
        let head_content = git_ops
            .get_file_content(file, head_branch)?
            .unwrap_or_default();

        // Find annotations in base branch
        find_annotations(
            &mut base_annotations,
            file,
            &base_content,
            &annotation_regex,
            &mut rule_cache,
        );

        // Find annotations in head branch
        find_annotations(
            &mut head_annotations,
            file,
            &head_content,
            &annotation_regex,
            &mut rule_cache,
        );
    }

    // Count annotations by rule
    let base_counts = count_annotations_by_rule(&base_annotations);
    let head_counts = count_annotations_by_rule(&head_annotations);

    // Count annotations by crate
    let base_crate_counts = count_annotations_by_crate(&base_annotations);
    let head_crate_counts = count_annotations_by_crate(&head_annotations);

    info!(
        "Analysis complete. Found {} annotations in base branch and {} in head branch",
        base_annotations.len(),
        head_annotations.len()
    );

    Ok(AnalysisResult {
        base_annotations,
        head_annotations,
        base_counts,
        head_counts,
        changed_files,
        base_crate_counts,
        head_crate_counts,
    })
}

/// Analyze all files just for crate-level statistics
fn analyze_all_files_for_crates(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<(AnnotationCounts, AnnotationCounts)> {
    info!(
        "Analyzing all {} Rust files for crate-level statistics...",
        files.len()
    );

    let annotation_regex = create_annotation_regex(rules)?;

    let mut base_annotations = Vec::new();
    let mut head_annotations = Vec::new();
    let mut rule_cache = HashMap::new();

    let git_ops = GitOperations::default();
    for file in files {
        let base_content = git_ops
            .get_file_content(file, base_branch)?
            .unwrap_or_default();
        let head_content = git_ops
            .get_file_content(file, head_branch)?
            .unwrap_or_default();

        // Find annotations in base branch
        find_annotations(
            &mut base_annotations,
            file,
            &base_content,
            &annotation_regex,
            &mut rule_cache,
        );

        // Find annotations in head branch
        find_annotations(
            &mut head_annotations,
            file,
            &head_content,
            &annotation_regex,
            &mut rule_cache,
        );
    }

    let base_crate_counts = count_annotations_by_crate(&base_annotations);
    let head_crate_counts = count_annotations_by_crate(&head_annotations);

    Ok((base_crate_counts, head_crate_counts))
}
