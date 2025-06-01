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
    let page = octocrab
        .issues(owner, repo)
        .list_comments(pr_number)
        .per_page(100)
        .send()
        .await
        .context("Failed to list PR comments")?;

    let all_pages = octocrab
        .all_pages(page)
        .await
        .context("Failed to fetch all pages of comments")?;

    for comment in all_pages {
        if comment
            .body
            .as_ref()
            .is_some_and(|body| body.contains(signature))
        {
            return Ok(Some(*comment.id));
        }
    }

    // No matching comment found
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Uri;
    use httpmock::prelude::*;
    use serde_json::json;
    use std::str::FromStr;

    /// Helper function to create an Octocrab instance that uses our mock server
    fn create_test_octocrab(server: &MockServer) -> Octocrab {
        let uri = Uri::from_str(&server.base_url()).unwrap();
        Octocrab::builder().base_uri(uri).unwrap().build().unwrap()
    }

    /// Helper function to create a standard user object for responses
    fn standard_user() -> serde_json::Value {
        json!({
            "login": "octocat",
            "id": 1,
            "node_id": "MDQ6VXNlcjE=",
            "avatar_url": "https://github.com/images/error/octocat_happy.gif",
            "gravatar_id": "",
            "url": "https://api.github.com/users/octocat",
            "html_url": "https://github.com/octocat",
            "followers_url": "https://api.github.com/users/octocat/followers",
            "following_url": "https://api.github.com/users/octocat/following{/other_user}",
            "gists_url": "https://api.github.com/users/octocat/gists{/gist_id}",
            "starred_url": "https://api.github.com/users/octocat/starred{/owner}{/repo}",
            "subscriptions_url": "https://api.github.com/users/octocat/subscriptions",
            "organizations_url": "https://api.github.com/users/octocat/orgs",
            "repos_url": "https://api.github.com/users/octocat/repos",
            "events_url": "https://api.github.com/users/octocat/events{/privacy}",
            "received_events_url": "https://api.github.com/users/octocat/received_events",
            "type": "User",
            "site_admin": false
        })
    }

    /// Helper function to create a standard comment object for responses
    fn create_comment_json(id: u64, body: &str) -> serde_json::Value {
        json!({
            "id": id,
            "node_id": "MDExOlB1bGxSZXF1ZXN0Q29tbWVudHt9",
            "html_url": format!("https://github.com/test-owner/test-repo/pull/123#issuecomment-{}", id),
            "body": body,
            "user": standard_user(),
            "created_at": "2023-01-01T00:00:00Z",
            "updated_at": "2023-01-01T00:00:00Z",
            "url": "https://api.github.com/repos/test-owner/test-repo/issues/comments/123",
            "author_association": "COLLABORATOR"
        })
    }

    #[tokio::test]
    async fn test_post_comment_create_new() {
        // Create a mock server
        let server = MockServer::start();

        // Mock the list comments endpoint (used internally by find_existing_comment)
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([])); // No existing comments
        });

        // Mock the create comment endpoint
        let create_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .json_body(json!({
                    "body": "Test report\n\n<!-- test-signature -->"
                }));

            then.status(201)
                .header("content-type", "application/json")
                .json_body(create_comment_json(
                    456,
                    "Test report\n\n<!-- test-signature -->",
                ));
        });

        let octocrab = create_test_octocrab(&server);

        // Call the public function we're testing
        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Test report".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        // Verify the result
        assert!(result.is_ok());
        list_comments_mock.assert();
        create_comment_mock.assert();
    }

    #[tokio::test]
    async fn test_post_comment_update_existing() {
        // Create a mock server
        let server = MockServer::start();

        // Mock the list comments endpoint
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([create_comment_json(
                    456,
                    "Old report\n\n<!-- test-signature -->"
                )]));
        });

        // Mock the update comment endpoint with the exact path and body format
        let update_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/comments/456")
                .json_body(json!({
                    "body": "Updated report\n\n<!-- test-signature -->"
                }));

            then.status(200)
                .header("content-type", "application/json")
                .json_body(create_comment_json(
                    456,
                    "Updated report\n\n<!-- test-signature -->",
                ));
        });

        let octocrab = create_test_octocrab(&server);

        // Call the public function we're testing
        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Updated report".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        // Verify the result
        assert!(result.is_ok());
        list_comments_mock.assert();
        update_comment_mock.assert();
    }

    #[tokio::test]
    async fn test_post_comment_with_pagination() {
        // Create a mock server
        let server = MockServer::start();

        // Second page mock - ONLY match requests WITH page=2. Needs to be registered first.
        let second_page_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("page", "2");

            then.status(200)
                .header("content-type", "application/json")
                .header("Link", format!(
                    "<{}/repos/test-owner/test-repo/issues/123/comments?page=1&per_page=100>; rel=\"prev\", <{}/repos/test-owner/test-repo/issues/123/comments?per_page=100>; rel=\"first\"",
                    server.base_url(), server.base_url()
                ))
                .json_body(json!([
                create_comment_json(200, "Second page comment\n\n<!-- test-signature -->")
            ]));
        });

        // First page mock
        let first_page_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .header("Link", format!(
                    "<{}/repos/test-owner/test-repo/issues/123/comments?page=2&per_page=100>; rel=\"next\", <{}/repos/test-owner/test-repo/issues/123/comments?page=2&per_page=100>; rel=\"last\"",
                    server.base_url(), server.base_url()
                ))
                .json_body(json!([
                create_comment_json(100, "First page comment")
            ]));
        });

        // Update comment mock
        let update_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/comments/200")
                .json_body(json!({
                    "body": "Test with pagination\n\n<!-- test-signature -->"
                }));

            then.status(200)
                .header("content-type", "application/json")
                .json_body(create_comment_json(
                    200,
                    "Test with pagination\n\n<!-- test-signature -->",
                ));
        });

        let octocrab = create_test_octocrab(&server);

        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Test with pagination".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        assert!(result.is_ok());
        assert!(first_page_mock.hits() > 0, "First page should be requested");
        assert!(
            second_page_mock.hits() > 0,
            "Second page should be requested"
        );
        assert!(
            update_comment_mock.hits() > 0,
            "Update comment endpoint should be called"
        );
    }

    #[tokio::test]
    async fn test_post_comment_with_error() {
        let server = MockServer::start();

        // Mock the list comments endpoint with a server error
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(500)
                .header("content-type", "application/json")
                .json_body(json!({
                    "message": "Internal server error"
                }));
        });

        let octocrab = create_test_octocrab(&server);

        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Test report".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        assert!(result.is_err());
        assert!(
            list_comments_mock.hits() >= 1,
            "Expected the mock to be called at least once"
        );
    }

    #[tokio::test]
    async fn test_post_comment_default_signature() {
        let server = MockServer::start();

        // Mock the list comments endpoint
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([]));
        });

        // Mock the create comment endpoint - checking for default signature
        let create_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .json_body(json!({
                    "body": "Test report\n\n<!-- clippy-annotation-reporter-comment -->"
                }));

            then.status(201)
                .header("content-type", "application/json")
                .json_body(create_comment_json(
                    456,
                    "Test report\n\n<!-- clippy-annotation-reporter-comment -->",
                ));
        });

        let octocrab = create_test_octocrab(&server);

        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Test report".to_string(),
            None,
        )
        .await;

        assert!(result.is_ok());
        list_comments_mock.assert();
        create_comment_mock.assert();
    }

    #[tokio::test]
    async fn test_post_comment_error_on_create() {
        // Create a mock server
        let server = MockServer::start();

        // Mock the list comments endpoint (no existing comments)
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([]));
        });

        // Mock the create comment endpoint with an error
        let create_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .json_body(json!({
                    "body": "Test report\n\n<!-- test-signature -->"
                }));

            then.status(422)
                .header("content-type", "application/json")
                .json_body(json!({
                    "message": "Validation failed",
                    "errors": [
                        {
                            "resource": "Issue",
                            "field": "body",
                            "code": "invalid"
                        }
                    ]
                }));
        });

        let octocrab = create_test_octocrab(&server);

        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Test report".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        assert!(result.is_err());
        list_comments_mock.assert();
        create_comment_mock.assert();
    }

    #[tokio::test]
    async fn test_post_comment_error_on_update() {
        let server = MockServer::start();

        // Mock the list comments endpoint
        let list_comments_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/issues/123/comments")
                .query_param("per_page", "100");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([create_comment_json(
                    456,
                    "Old report\n\n<!-- test-signature -->"
                )]));
        });

        // Mock the update comment endpoint with an error
        let update_comment_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/test-owner/test-repo/issues/comments/456")
                .json_body(json!({
                    "body": "Updated report\n\n<!-- test-signature -->"
                }));

            then.status(403)
                .header("content-type", "application/json")
                .json_body(json!({
                    "message": "Forbidden",
                    "documentation_url": "https://docs.github.com/rest/issues/comments"
                }));
        });

        let octocrab = create_test_octocrab(&server);

        let result = post_comment(
            &octocrab,
            "test-owner",
            "test-repo",
            123,
            "Updated report".to_string(),
            Some("<!-- test-signature -->"),
        )
        .await;

        assert!(result.is_err());
        list_comments_mock.assert();
        update_comment_mock.assert();
    }
}
