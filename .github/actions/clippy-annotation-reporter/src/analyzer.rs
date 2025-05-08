//! Analyzer module for clippy-annotation-reporter
//!
//! This module handles all the logic related to analyzing clippy annotations
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
    pub line_number: usize,
    pub rule: String,
    pub line_content: String,
}

/// Result of annotation analysis
pub struct AnalysisResult {
    pub base_annotations: Vec<ClippyAnnotation>,
    pub head_annotations: Vec<ClippyAnnotation>,
    pub base_counts: HashMap<String, usize>,
    pub head_counts: HashMap<String, usize>,
    pub changed_files: HashSet<String>,
}

/// Analyzer struct for managing the analysis process
pub struct Analyzer<'a> {
    octocrab: &'a Octocrab,
    owner: String,
    repo: String,
    pr_number: u64,
    base_branch: String,
    head_branch: String,
    rules: Vec<String>,
}

impl<'a> Analyzer<'a> {
    /// Create a new analyzer instance
    pub fn new(
        octocrab: &'a Octocrab,
        owner: &str,
        repo: &str,
        pr_number: u64,
        base_branch: &str,
        head_branch: &str,
        rules: &[String],
    ) -> Self {
        Self {
            octocrab,
            owner: owner.to_string(),
            repo: repo.to_string(),
            pr_number,
            base_branch: base_branch.to_string(),
            head_branch: head_branch.to_string(),
            rules: rules.to_vec(),
        }
    }

    /// Run the full analysis and return the results
    pub async fn run(&self) -> Result<AnalysisResult> {
        // Get changed files
        let changed_files = self.get_changed_files().await?;

        if changed_files.is_empty() {
            return Err(anyhow::anyhow!("No Rust files changed in this PR"));
        }

        // Analyze annotations
        let result = self.analyze_annotations(&changed_files)?;

        Ok(result)
    }

    /// Get changed Rust files from the PR
    async fn get_changed_files(&self) -> Result<Vec<String>> {
        println!("Getting changed files from PR #{}...", self.pr_number);

        let files = self
            .octocrab
            .pulls(&self.owner, &self.repo)
            .list_files(self.pr_number)
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

    /// Analyze clippy annotations in base and head branches
    fn analyze_annotations(&self, files: &[String]) -> Result<AnalysisResult> {
        println!("Analyzing clippy annotations in {} files...", files.len());

        // Create a regex for matching clippy allow annotations
        let rule_pattern = self.rules.join("|");
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
            let base_content = match self.get_file_content(file, &self.base_branch) {
                Ok(content) => content,
                Err(e) => {
                    println!(
                        "Warning: Failed to get {} content from {}: {}",
                        file, self.base_branch, e
                    );
                    String::new()
                }
            };

            // Get file content from head branch
            let head_content = match self.get_file_content(file, &self.head_branch) {
                Ok(content) => content,
                Err(e) => {
                    println!(
                        "Warning: Failed to get {} content from {}: {}",
                        file, self.head_branch, e
                    );
                    String::new()
                }
            };

            // Find annotations in base branch
            self.find_annotations(
                &mut base_annotations,
                file,
                &base_content,
                &annotation_regex,
            );

            // Find annotations in head branch
            self.find_annotations(
                &mut head_annotations,
                file,
                &head_content,
                &annotation_regex,
            );
        }

        // Count annotations by rule
        let base_counts = self.count_annotations_by_rule(&base_annotations);
        let head_counts = self.count_annotations_by_rule(&head_annotations);

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
        })
    }

    /// Get file content from a specific branch
    fn get_file_content(&self, file: &str, branch: &str) -> Result<String> {
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
        &self,
        annotations: &mut Vec<ClippyAnnotation>,
        file: &str,
        content: &str,
        regex: &Regex,
    ) {
        for (line_number, line) in content.lines().enumerate() {
            if let Some(captures) = regex.captures(line) {
                if let Some(rule_match) = captures.get(1) {
                    let rule = rule_match.as_str().to_string();
                    annotations.push(ClippyAnnotation {
                        file: file.to_string(),
                        line_number: line_number + 1,
                        rule,
                        line_content: line.trim().to_string(),
                    });
                }
            }
        }
    }

    /// Count annotations by rule
    fn count_annotations_by_rule(
        &self,
        annotations: &[ClippyAnnotation],
    ) -> HashMap<String, usize> {
        let mut counts = HashMap::new();

        for annotation in annotations {
            *counts.entry(annotation.rule.clone()).or_insert(0) += 1;
        }

        counts
    }
}
