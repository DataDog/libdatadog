//! Analyzer module for clippy-annotation-reporter
//!
//! This module handles functions for analyzing clippy annotations
//! including getting changed files, comparing branches, and producing analysis results.

use anyhow::{Context as _, Result};
use octocrab::Octocrab;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::process::Command;

/// Represents a clippy annotation in code
#[derive(Debug, Clone)]
pub struct ClippyAnnotation {
    pub file: String,
    pub rule: String,
}

/// Result of annotation analysis
pub struct AnalysisResult {
    pub base_annotations: Vec<ClippyAnnotation>,
    pub head_annotations: Vec<ClippyAnnotation>,
    pub base_counts: HashMap<String, usize>,
    pub head_counts: HashMap<String, usize>,
    pub changed_files: HashSet<String>,
    pub base_crate_counts: HashMap<String, usize>,
    pub head_crate_counts: HashMap<String, usize>,
}

/// Run the full analysis process
pub async fn run_analysis(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<AnalysisResult> {
    let changed_files = get_changed_files(octocrab, owner, repo, pr_number).await?;

    if changed_files.is_empty() {
        return Err(anyhow::anyhow!("No Rust files changed in this PR"));
    }

    let all_files = get_all_rust_files()?;
    let pr_analysis = analyze_annotations(&changed_files, base_branch, head_branch, rules)?;
    let (repo_base_crate_counts, repo_head_crate_counts) =
        analyze_all_files_for_crates(&all_files, base_branch, head_branch, rules)?;

    Ok(AnalysisResult {
        base_annotations: pr_analysis.base_annotations,
        head_annotations: pr_analysis.head_annotations,
        base_counts: pr_analysis.base_counts,
        head_counts: pr_analysis.head_counts,
        changed_files: pr_analysis.changed_files,
        base_crate_counts: repo_base_crate_counts,
        head_crate_counts: repo_head_crate_counts,
    })
}
/// Analyze clippy annotations in base and head branches
fn analyze_annotations(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<AnalysisResult> {
    println!("Analyzing clippy annotations in {} files...", files.len());

    // Create a regex for matching clippy allow annotations
    let rule_pattern = rules.join("|");
    let annotation_regex = Regex::new(&format!(
        r"#\s*\[\s*allow\s*\(\s*clippy\s*::\s*({})\s*\)\s*\]",
        rule_pattern
    ))
    .context("Failed to compile annotation regex")?;

    let mut base_annotations = Vec::new();
    let mut head_annotations = Vec::new();
    let mut changed_files = HashSet::new();

    // Process each file
    for file in files {
        changed_files.insert(file.clone());

        // Get file content from base branch
        let base_content = match get_file_content(file, base_branch) {
            Ok(content) => content,
            Err(e) => {
                println!(
                    "Warning: Failed to get {} content from {}: {}",
                    file, base_branch, e
                );
                String::new()
            }
        };

        // Get file content from head branch
        let head_content = match get_file_content(file, head_branch) {
            Ok(content) => content,
            Err(e) => {
                println!(
                    "Warning: Failed to get {} content from {}: {}",
                    file, head_branch, e
                );
                String::new()
            }
        };

        // Find annotations in base branch
        find_annotations(
            &mut base_annotations,
            file,
            &base_content,
            &annotation_regex,
        );

        // Find annotations in head branch
        find_annotations(
            &mut head_annotations,
            file,
            &head_content,
            &annotation_regex,
        );
    }

    // Count annotations by rule
    let base_counts = count_annotations_by_rule(&base_annotations);
    let head_counts = count_annotations_by_rule(&head_annotations);

    // Count annotations by crate
    let base_crate_counts = count_annotations_by_crate(&base_annotations);
    let head_crate_counts = count_annotations_by_crate(&head_annotations);

    println!(
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

/// Get changed Rust files from the PR
async fn get_changed_files(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<String>> {
    println!("Getting changed files from PR #{}...", pr_number);

    let files = octocrab
        .pulls(owner, repo)
        .list_files(pr_number)
        .await
        .context("Failed to list PR files")?;

    // Filter for Rust files only
    let rust_files: Vec<String> = files
        .items
        .into_iter()
        .filter(|file| file.filename.ends_with(".rs"))
        .map(|file| file.filename)
        .collect();

    println!("Found {} changed Rust files", rust_files.len());

    Ok(rust_files)
}

/// Get file content from a specific branch
fn get_file_content(file: &str, branch: &str) -> Result<String> {
    println!("Getting content for {} from {}", file, branch);

    let output = Command::new("git")
        .args(["show", &format!("{}:{}", branch, file)])
        .output()
        .context(format!("Failed to execute git show command for {}", file))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git show command failed: {}", stderr);
    }

    let content =
        String::from_utf8(output.stdout).context("Failed to parse file content as UTF-8")?;

    Ok(content)
}

/// Find clippy annotations in file content
fn find_annotations(
    annotations: &mut Vec<ClippyAnnotation>,
    file: &str,
    content: &str,
    regex: &Regex,
) {
    for (_line_number, line) in content.lines().enumerate() {
        if let Some(captures) = regex.captures(line) {
            if let Some(rule_match) = captures.get(1) {
                let rule = rule_match.as_str().to_string();
                annotations.push(ClippyAnnotation {
                    file: file.to_string(),
                    rule,
                });
            }
        }
    }
}

/// Count annotations by rule
fn count_annotations_by_rule(annotations: &[ClippyAnnotation]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();

    for annotation in annotations {
        *counts.entry(annotation.rule.clone()).or_insert(0) += 1;
    }

    counts
}

/// Get crate information for a given file path
fn get_crate_for_file(file_path: &str) -> String {
    // Simple heuristic: use the first directory as the crate name
    // For files in src/ directory, use the parent directory
    // For files in the root, use "root"

    let path_parts: Vec<&str> = file_path.split('/').collect();

    if path_parts.is_empty() {
        return "unknown".to_string();
    }

    // Handle common project structures
    if path_parts.len() > 1 {
        // If it's in "src" or "tests" folder, use the parent directory
        if path_parts[0] == "src" || path_parts[0] == "tests" {
            return "root".to_string();
        }

        // If it's in a nested crate structure like crates/foo/src
        if path_parts[0] == "crates" && path_parts.len() > 2 {
            return path_parts[1].to_string();
        }

        // If it's in a workspace pattern like foo/src
        if path_parts.len() > 1 && (path_parts[1] == "src" || path_parts[1] == "tests") {
            return path_parts[0].to_string();
        }
    }

    // Default: use first directory name
    path_parts[0].to_string()
}

/// Count annotations by crate
fn count_annotations_by_crate(annotations: &[ClippyAnnotation]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();

    for annotation in annotations {
        let crate_name = get_crate_for_file(&annotation.file);
        *counts.entry(crate_name).or_insert(0) += 1;
    }

    counts
}

/// Get all Rust files in the repository
fn get_all_rust_files() -> Result<Vec<String>> {
    println!("Getting all Rust files in the repository...");

    // Use git ls-files to get all tracked Rust files
    let output = Command::new("git")
        .args(["ls-files", "*.rs"])
        .output()
        .context("Failed to execute git ls-files command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git ls-files command failed: {}", stderr);
    }

    let files = String::from_utf8(output.stdout).context("Failed to parse git ls-files output")?;

    let rust_files: Vec<String> = files.lines().map(|line| line.to_string()).collect();

    println!("Found {} Rust files in total", rust_files.len());

    Ok(rust_files)
}

/// Analyze all files just for crate-level statistics
fn analyze_all_files_for_crates(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<(HashMap<String, usize>, HashMap<String, usize>)> {
    println!(
        "Analyzing all {} Rust files for crate-level statistics...",
        files.len()
    );

    // Create regex
    let rule_pattern = rules.join("|");
    let annotation_regex = Regex::new(&format!(
        r"#\s*\[\s*allow\s*\(\s*clippy\s*::\s*({})\s*\)\s*\]",
        rule_pattern
    ))
    .context("Failed to compile annotation regex")?;

    let mut base_annotations = Vec::new();
    let mut head_annotations = Vec::new();

    // Process each file
    for file in files {
        // Get file content from base branch
        let base_content = match get_file_content(file, base_branch) {
            Ok(content) => content,
            Err(e) => {
                // Skip errors for files that might not exist in one branch
                if !e.to_string().contains("did not match any file") {
                    println!(
                        "Warning: Failed to get {} content from {}: {}",
                        file, base_branch, e
                    );
                }
                String::new()
            }
        };

        // Get file content from head branch
        let head_content = match get_file_content(file, head_branch) {
            Ok(content) => content,
            Err(e) => {
                // Skip errors for files that might not exist in one branch
                if !e.to_string().contains("did not match any file") {
                    println!(
                        "Warning: Failed to get {} content from {}: {}",
                        file, head_branch, e
                    );
                }
                String::new()
            }
        };

        // Find annotations in base branch
        find_annotations(
            &mut base_annotations,
            file,
            &base_content,
            &annotation_regex,
        );

        // Find annotations in head branch
        find_annotations(
            &mut head_annotations,
            file,
            &head_content,
            &annotation_regex,
        );
    }

    let base_crate_counts = count_annotations_by_crate(&base_annotations);
    let head_crate_counts = count_annotations_by_crate(&head_annotations);

    Ok((base_crate_counts, head_crate_counts))
}
