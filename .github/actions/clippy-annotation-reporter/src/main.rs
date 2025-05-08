use anyhow::{Context as _, Result};
use octocrab::Octocrab;

mod analyzer;
mod config;
mod report_generator;

use crate::analyzer::Analyzer;
use crate::config::ConfigBuilder;
use crate::report_generator::generate_report;

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
