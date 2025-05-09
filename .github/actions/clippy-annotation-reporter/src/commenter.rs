//! Commenter module for clippy-annotation-reporter
//!
//! This module handles interactions with GitHub for commenting on PRs,
//! including finding existing comments and updating or creating comments.

use anyhow::{Context as _, Result};
use octocrab::Octocrab;

/// Post or update a comment on a PR with the given report
pub async fn post_or_update_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    report: String,
    signature: Option<&str>,
) -> Result<()> {
    // Use the provided signature or default
    let signature = signature.unwrap_or("<!-- clippy-annotation-reporter-comment -->");

    // Add the signature to the report
    let report_with_signature = format!("{}\n\n{}", report, signature);

    // Search for existing comment by the bot
    println!("Checking for existing comment on PR #{}", pr_number);
    let existing_comment =
        find_existing_comment(octocrab, owner, repo, pr_number, signature).await?;

    // Update existing comment or create a new one
    if let Some(comment_id) = existing_comment {
        println!("Updating existing comment #{}", comment_id);
        octocrab
            .issues(owner, repo)
            .update_comment(comment_id.into(), report_with_signature)
            .await
            .context("Failed to update existing comment")?;
        println!("Comment updated successfully!");
    } else {
        println!("Creating new comment on PR #{}", pr_number);
        octocrab
            .issues(owner, repo)
            .create_comment(pr_number, report_with_signature)
            .await
            .context("Failed to post comment to PR")?;
        println!("Comment created successfully!");
    }

    Ok(())
}

/// Find existing comment by the bot on a PR
pub async fn find_existing_comment(
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

/// Post a new comment to a PR (without checking for existing comments)
pub async fn post_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    content: String,
) -> Result<()> {
    octocrab
        .issues(owner, repo)
        .create_comment(pr_number, content)
        .await
        .context("Failed to post comment to PR")?;

    Ok(())
}

/// Update an existing comment
pub async fn update_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    comment_id: u64,
    content: String,
) -> Result<()> {
    octocrab
        .issues(owner, repo)
        .update_comment(comment_id.into(), content)
        .await
        .context("Failed to update comment")?;

    Ok(())
}
