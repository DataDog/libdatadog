use anyhow::{Context as _, Result};
use octocrab::Octocrab;
use std::collections::HashMap;

mod analyzer;
mod config;

use crate::analyzer::{AnalysisResult, Analyzer};
use crate::config::ConfigBuilder;

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

    // Create analyzer and run analysis
    let analyzer = Analyzer::new(
        &octocrab,
        &config.owner,
        &config.repo,
        config.pr_number,
        &config.base_branch,
        &config.head_branch,
        &config.rules,
    );

    // Run the analysis
    let analysis_result = match analyzer.run().await {
        Ok(result) => result,
        Err(e) => {
            if e.to_string().contains("No Rust files changed") {
                println!("No Rust files changed in this PR, nothing to analyze.");
                return Ok(());
            }
            return Err(e);
        }
    };
    // 3. Generate a report
    let report = generate_report(
        &analysis_result,
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

/// Generate a detailed report for PR comment
fn generate_report(
    analysis: &AnalysisResult,
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
