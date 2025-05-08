//! Commenter module for clippy-annotation-reporter
//!
//! This module handles interactions with GitHub for commenting on PRs,
//! including finding existing comments and updating or creating comments.

use anyhow::{Context as _, Result};
use octocrab::models::issues::Comment;
use octocrab::Octocrab;

/// Handles GitHub comment operations
pub struct Commenter<'a> {
    octocrab: &'a Octocrab,
    owner: String,
    repo: String,
    pr_number: u64,
    signature: String,
}

impl<'a> Commenter<'a> {
    /// Create a new commenter instance
    pub fn new(octocrab: &'a Octocrab, owner: &str, repo: &str, pr_number: u64) -> Self {
        Self {
            octocrab,
            owner: owner.to_string(),
            repo: repo.to_string(),
            pr_number,
            signature: "<!-- clippy-annotation-reporter-comment -->".to_string(),
        }
    }

    /// Set a custom signature for identifying the bot's comments
    pub fn with_signature(mut self, signature: &str) -> Self {
        self.signature = signature.to_string();
        self
    }

    /// Post or update a comment on the PR with the given report
    pub async fn run(&self, report: String) -> Result<()> {
        // Add the signature to the report
        let report_with_signature = format!("{}\n\n{}", report, self.signature);

        // Search for existing comment by the bot
        println!("Checking for existing comment on PR #{}", self.pr_number);
        let existing_comment = self.find_existing_comment().await?;

        // Update existing comment or create a new one
        if let Some(comment_id) = existing_comment {
            println!("Updating existing comment #{}", comment_id);
            self.octocrab
                .issues(&self.owner, &self.repo)
                .update_comment(comment_id.into(), report_with_signature)
                .await
                .context("Failed to update existing comment")?;
            println!("Comment updated successfully!");
        } else {
            println!("Creating new comment on PR #{}", self.pr_number);
            self.octocrab
                .issues(&self.owner, &self.repo)
                .create_comment(self.pr_number, report_with_signature)
                .await
                .context("Failed to post comment to PR")?;
            println!("Comment created successfully!");
        }

        Ok(())
    }

    /// Find existing comment by the bot on a PR
    async fn find_existing_comment(&self) -> Result<Option<u64>> {
        // Get all comments on the PR
        let mut page = self
            .octocrab
            .issues(&self.owner, &self.repo)
            .list_comments(self.pr_number)
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
                    .map_or(false, |body| body.contains(&self.signature))
                {
                    return Ok(Some(*comment.id));
                }
            }

            // Try to get the next page if it exists
            match self.octocrab.get_page(&page.next).await {
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
}
