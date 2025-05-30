// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Commenter module for clippy-annotation-reporter
//!
//! This module handles interactions with GitHub for commenting on PRs,
//! including finding existing comments and updating or creating comments.

use anyhow::{Context as _, Result};
use log::{error, info};
use octocrab::Octocrab;

/// Post or update a comment on a PR with the given report
pub async fn post_comment(
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
    info!("Checking for existing comment on PR #{}", pr_number);
    let existing_comment =
        find_existing_comment(octocrab, owner, repo, pr_number, signature).await?;

    // Update existing comment or create a new one
    if let Some(comment_id) = existing_comment {
        info!("Updating existing comment #{}", comment_id);
        octocrab
            .issues(owner, repo)
            .update_comment(comment_id.into(), report_with_signature)
            .await
            .context("Failed to update existing comment")?;
    } else {
        info!("Creating new comment on PR #{}", pr_number);
        octocrab
            .issues(owner, repo)
            .create_comment(pr_number, report_with_signature)
            .await
            .context("Failed to post comment to PR")?;
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
                .is_some_and(|body| body.contains(signature))
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
                // TODO: EK - Handle error more gracefully
                error!("Warning: Failed to fetch next page of comments: {}", e);
                break;
            }
        }
    }

    // No matching comment found
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use mockall::predicate::*;
    use mockall::*;

    // Create our own simplified Comment type for testing
    struct TestComment {
        id: u64,
        body: Option<String>,
    }

    // Mock for functions that interact with GitHub
    mock! {
        pub GitHub {
            async fn list_comments(&self, owner: &str, repo: &str, pr_number: u64) -> Result<Vec<TestComment>>;
            async fn get_next_page(&self, url: Option<String>) -> Result<Option<Vec<TestComment>>>;
            async fn create_comment(&self, owner: &str, repo: &str, pr_number: u64, body: String) -> Result<()>;
            async fn update_comment(&self, owner: &str, repo: &str, comment_id: u64, body: String) -> Result<()>;
        }
    }

    #[tokio::test]
    async fn test_post_or_update_comment_existing() {
        let mut mock = MockGitHub::new();

        // Signature to look for
        let signature = "<!-- test-signature -->";
        let report = "Test report";
        let report_with_signature = format!("{}\n\n{}", report, signature);

        // mock the expected call that will look for an existing comment
        mock.expect_list_comments()
            .with(eq("owner"), eq("repo"), eq(123))
            .returning(|_, _, _| {
                Ok(vec![
                    TestComment {
                        id: 456,
                        body: Some("Some other comment".to_string()),
                    },
                    TestComment {
                        id: 789,
                        body: Some("A comment with <!-- test-signature --> in it".to_string()),
                    },
                ])
            });

        // mock the expected call that will update the comment
        mock.expect_update_comment()
            .with(eq("owner"), eq("repo"), eq(789), eq(report_with_signature))
            .returning(|_, _, _, _| Ok(()));

        let result = test_post_or_update_comment(
            &mock,
            "owner",
            "repo",
            123,
            report.to_owned(),
            Some(signature),
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_post_or_update_comment_new() {
        let mut mock = MockGitHub::new();

        // Signature to look for
        let signature = "<!-- test-signature -->";
        let report = "Test report";
        let report_with_signature = format!("{}\n\n{}", report, signature);

        // Mock the function to look for an existing comment
        mock.expect_list_comments()
            .with(eq("owner"), eq("repo"), eq(123))
            .returning(|_, _, _| {
                Ok(vec![TestComment {
                    id: 456,
                    body: Some("Some other comment".to_string()),
                }])
            });

        // Mock the function to not find the comment in the first page
        mock.expect_get_next_page().returning(|_| Ok(None));

        // mock the call to create a comment
        mock.expect_create_comment()
            .with(eq("owner"), eq("repo"), eq(123), eq(report_with_signature))
            .returning(|_, _, _, _| Ok(()));

        let result = test_post_or_update_comment(
            &mock,
            "owner",
            "repo",
            123,
            report.to_owned(),
            Some(signature),
        )
        .await;

        assert!(result.is_ok());
    }

    // Test-specific function to simulate the behavior of post_or_update_comment
    async fn test_post_or_update_comment(
        github: &MockGitHub,
        owner: &str,
        repo: &str,
        pr_number: u64,
        report: String,
        signature: Option<&str>,
    ) -> Result<()> {
        // Use provided signature or default
        let signature = signature.unwrap_or("<!-- clippy-annotation-reporter-comment -->");

        // Add signature to report
        let report_with_signature = format!("{}\n\n{}", report, signature);

        // Find existing comment with the signature
        let existing_comment_id =
            test_find_existing_comment(github, owner, repo, pr_number, signature).await?;

        // Update existing comment or create new one
        if let Some(comment_id) = existing_comment_id {
            github
                .update_comment(owner, repo, comment_id, report_with_signature)
                .await?;
        } else {
            github
                .create_comment(owner, repo, pr_number, report_with_signature)
                .await?;
        }

        Ok(())
    }

    // Test-specific function to simulate the behavior of find_existing_comment
    async fn test_find_existing_comment(
        github: &MockGitHub,
        owner: &str,
        repo: &str,
        pr_number: u64,
        signature: &str,
    ) -> Result<Option<u64>> {
        // Get comments from the first page
        let comments = github.list_comments(owner, repo, pr_number).await?;

        // Check if any comment contains our signature
        for comment in &comments {
            if comment
                .body
                .as_ref()
                .map_or(false, |body| body.contains(signature))
            {
                return Ok(Some(comment.id));
            }
        }

        // Check if there are more pages
        let next_page = github.get_next_page(Some("next_url".to_string())).await?;

        if let Some(next_comments) = next_page {
            // Check comments in the next page
            for comment in &next_comments {
                if comment
                    .body
                    .as_ref()
                    .map_or(false, |body| body.contains(signature))
                {
                    return Ok(Some(comment.id));
                }
            }
        }

        // No matching comment found
        Ok(None)
    }
}
