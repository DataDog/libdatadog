use anyhow::{Context as _, Result};
use clap::Parser;
use octocrab::Octocrab;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::process::Command;

mod config;

use crate::config::ConfigBuilder;
use config::{Args, Config, GitHubContext};

/// Represents a clippy annotation in code
#[derive(Debug, Clone)]
struct ClippyAnnotation {
    file: String,
    rule: String,
    line_content: String,
}

/// Result of annotation analysis
struct AnnotationAnalysis {
    base_annotations: Vec<ClippyAnnotation>,
    head_annotations: Vec<ClippyAnnotation>,
    base_counts: HashMap<String, usize>,
    head_counts: HashMap<String, usize>,
    changed_files: HashSet<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();

    println!("Clippy Annotation Reporter starting...");

    // Create configuration
    let config = ConfigBuilder::new().build()?;

    // Initialize GitHub API client with token
    let octocrab = Octocrab::builder()
        .personal_token(config.token.clone())
        .build()
        .context("Failed to build GitHub API client")?;

    // 1. Get changed files in the PR
    println!("Getting changed files from PR #{}", config.pr_number);
    let changed_files = get_changed_files(&octocrab, &config.owner, &config.repo, config.pr_number)
        .await
        .context("Failed to get changed files from PR")?;

    if changed_files.is_empty() {
        println!("No Rust files changed in this PR");
        return Ok(());
    }

    // 2. Analyze annotations in base and head branches
    println!("Analyzing clippy annotations...");
    let analysis = analyze_annotations(
        &changed_files,
        &config.base_branch,
        &config.head_branch,
        &config.rules,
    )
    .context("Failed to analyze annotations")?;

    // 3. Generate a report
    let report = generate_report(
        &analysis,
        &config.rules,
        &config.repository,
        &config.base_branch,
        &config.head_branch,
    );

    // 4. Post the report as a comment or update existing comment

    // Create a unique signature for the bot's comments
    let bot_signature = "<!-- clippy-annotation-reporter-comment -->";
    let report_with_signature = format!("{}\n\n{}", report, bot_signature);

    // Search for existing comment by the bot
    println!("Checking for existing comment on PR #{}", config.pr_number);
    let existing_comment = find_existing_comment(
        &octocrab,
        &config.owner,
        &config.repo,
        config.pr_number,
        bot_signature,
    )
    .await?;

    // Update existing comment or create a new one
    if let Some(comment_id) = existing_comment {
        println!("Updating existing comment #{}", comment_id);
        octocrab
            .issues(&config.owner, &config.repo)
            .update_comment(comment_id.into(), report_with_signature)
            .await
            .context("Failed to update existing comment")?;
        println!("Comment updated successfully!");
    } else {
        println!("Creating new comment on PR #{}", config.pr_number);
        octocrab
            .issues(&config.owner, &config.repo)
            .create_comment(config.pr_number, report_with_signature)
            .await
            .context("Failed to post comment to PR")?;
        println!("Comment created successfully!");
    }

    Ok(())
}

/// Find existing comment by the bot on a PR
async fn find_existing_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    signature: &str,
) -> Result<Option<u64>> {
    // Get all comments on the PR
    let mut page = octocrab
        .issues(owner, repo)
        .list_comments(pr_number)
        .per_page(100)
        .send()
        .await
        .context("Failed to list PR comments")?;

    // Process current and subsequent pages
    loop {
        for comment in &page {
            if comment
                .body
                .as_ref()
                .map_or(false, |body| body.contains(signature))
            {
                return Ok(Some(*comment.id));
            }
        }

        // Try to get the next page if it exists
        match octocrab.get_page(&page.next).await {
            Ok(Some(next_page)) => {
                page = next_page;
            }
            Ok(None) => {
                // No more pages
                break;
            }
            Err(e) => {
                println!("Warning: Failed to fetch next page of comments: {}", e);
                break;
            }
        }
    }

    // No matching comment found
    Ok(None)
}

/// Get changed Rust files from the PR
async fn get_changed_files(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<String>> {
    let files = octocrab
        .pulls(owner, repo)
        .list_files(pr_number)
        .await
        .context("Failed to list PR files")?;

    // Filter for Rust files only
    let rust_files = files
        .items
        .into_iter()
        .filter(|file| file.filename.ends_with(".rs"))
        .map(|file| file.filename)
        .collect();

    Ok(rust_files)
}

/// Analyze clippy annotations in base and head branches
fn analyze_annotations(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String],
) -> Result<AnnotationAnalysis> {
    // Create a regex for matching clippy allow annotations
    // This will capture the rule name in the first capture group
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

    Ok(AnnotationAnalysis {
        base_annotations,
        head_annotations,
        base_counts,
        head_counts,
        changed_files,
    })
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

    // Debug count
    let count = content.matches("#[allow(clippy::").count();
    println!(
        "Found {} clippy allow annotations in {}:{}",
        count, branch, file
    );

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
                    line_content: line.trim().to_string(),
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

/// Generate a detailed report for PR comment
fn generate_report(
    analysis: &AnnotationAnalysis,
    rules: &[String],
    repository: &str,
    base_branch: &str,
    head_branch: &str,
) -> String {
    let mut report = String::from("## Clippy Allow Annotation Report\n\n");

    // Add branch information with link to base branch
    report.push_str("Comparing clippy allow annotations between branches:\n");
    report.push_str(&format!(
        "- **Base Branch**: [{}](https://github.com/{}/tree/{})\n",
        base_branch, repository, base_branch
    ));
    report.push_str(&format!("- **PR Branch**: {}\n\n", head_branch));

    // Summary table by rule
    report.push_str("### Summary by Rule\n\n");
    report.push_str("| Rule | Base Branch | PR Branch | Change |\n");
    report.push_str("|------|------------|-----------|--------|\n");

    let mut total_base = 0;
    let mut total_head = 0;

    for rule in rules {
        let base_count = *analysis.base_counts.get(rule).unwrap_or(&0);
        let head_count = *analysis.head_counts.get(rule).unwrap_or(&0);
        let change = head_count as isize - base_count as isize;

        total_base += base_count;
        total_head += head_count;

        // Calculate percentage change
        let percent_change = if base_count > 0 {
            (change as f64 / base_count as f64) * 100.0
        } else if change > 0 {
            // If base count is 0 and there's an increase, it's an infinite increase
            // but we'll display it as N/A or a large number
            f64::INFINITY
        } else {
            // No change if both are 0
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
            rule, base_count, head_count, change_str
        ));
    }

    // Add total row with percentage
    let total_change = total_head as isize - total_base as isize;
    let total_percent_change = if total_base > 0 {
        (total_change as f64 / total_base as f64) * 100.0
    } else if total_change > 0 {
        f64::INFINITY
    } else {
        0.0
    };

    let total_change_str = if total_change > 0 {
        if total_percent_change.is_infinite() {
            format!("⚠️ +{} (N/A)", total_change)
        } else {
            format!("⚠️ +{} (+{:.1}%)", total_change, total_percent_change)
        }
    } else if total_change < 0 {
        format!("✅ {} ({:.1}%)", total_change, total_percent_change)
    } else {
        "No change (0%)".to_string()
    };

    report.push_str(&format!(
        "| **Total** | **{}** | **{}** | **{}** |\n\n",
        total_base, total_head, total_change_str
    ));

    // File-level annotation counts with percentage change
    if !analysis.changed_files.is_empty() {
        report.push_str("### Annotation Counts by File\n\n");
        report.push_str("| File | Base Branch | PR Branch | Change |\n");
        report.push_str("|------|------------|-----------|--------|\n");

        // Count annotations by file in base branch
        let mut base_file_counts = HashMap::new();
        for anno in &analysis.base_annotations {
            *base_file_counts.entry(anno.file.clone()).or_insert(0) += 1;
        }

        // Count annotations by file in head branch
        let mut head_file_counts = HashMap::new();
        for anno in &analysis.head_annotations {
            *head_file_counts.entry(anno.file.clone()).or_insert(0) += 1;
        }

        // Get a sorted list of all files
        let mut all_files: Vec<String> = analysis.changed_files.iter().cloned().collect();
        all_files.sort();

        // Generate table rows
        for file in all_files {
            let base_count = *base_file_counts.get(&file).unwrap_or(&0);
            let head_count = *head_file_counts.get(&file).unwrap_or(&0);
            let change = head_count as isize - base_count as isize;

            // Skip files with no changes in annotation count
            if change == 0 && base_count == 0 && head_count == 0 {
                continue;
            }

            // Calculate percentage change for file
            let percent_change = if base_count > 0 {
                (change as f64 / base_count as f64) * 100.0
            } else if change > 0 {
                f64::INFINITY
            } else {
                0.0
            };

            // Format the change string with percentage for file
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
                file, base_count, head_count, change_str
            ));
        }

        report.push_str("\n");
    }

    // Add explanation
    report.push_str("### About This Report\n\n");
    report.push_str("This report tracks Clippy allow annotations for specific rules, ");
    report.push_str("showing how they've changed in this PR. ");
    report
        .push_str("Decreasing the number of these annotations generally improves code quality.\n");

    report
}
