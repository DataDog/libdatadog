// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Configuration for the clippy-annotation-reporter
//!
//! This module handles all configuration-related logic including
//! command-line arguments, GitHub context, and environment variables.

use anyhow::{Context as _, Result};
use clap::Parser;
use log::{debug, warn};
use serde_json::Value;
use std::env;
use std::fs;

/// Command-line arguments for the clippy-annotation-reporter
#[derive(Parser, Clone, Debug)]
#[command(name = "clippy-annotation-reporter")]
#[command(about = "Reports changes in clippy allow annotations")]
pub struct Args {
    /// GitHub token for API access
    #[arg(long)]
    pub token: String,
    /// Comma-separated list of clippy rules to track
    #[arg(
        long,
        default_value = "unwrap_used,expect_used,todo,unimplemented,panic,unreachable"
    )]
    pub rules: String,
    /// GitHub repository (owner/repo) - defaults to current repository
    #[arg(long)]
    pub repo: Option<String>,
    /// Pull request number - defaults to PR from event context
    #[arg(long)]
    pub pr: Option<u64>,
    /// Base branch to compare against (defaults to the PR's base branch)
    #[arg(long)]
    pub base_branch: Option<String>,
}

impl Args {
    /// Create a new Args instance from command-line arguments
    pub fn from_cli() -> Result<Self> {
        let mut args = Self::parse();

        if args.token.is_empty() {
            args.token = env::var("GITHUB_TOKEN").map_err(|_| {
                anyhow::anyhow!("No token provided and GITHUB_TOKEN environment variable not set")
            })?;
        }

        Ok(args)
    }

    /// Parse the rules list from the comma-separated string
    pub fn parse_rules(&self) -> Vec<String> {
        self.rules.split(',').map(|s| s.trim().to_owned()).collect()
    }
}

/// GitHub event context extracted from environment
#[derive(Clone, Debug)]
pub struct GitHubContext {
    pub repository: String,
    pub pr_number: u64,
    pub base_ref: String,
    pub head_ref: String,
}

impl GitHubContext {
    /// Try to extract GitHub context from environment variables and event file
    pub fn from_env() -> Result<Self> {
        // Get repository from env
        let repository = env::var("GITHUB_REPOSITORY")
            .context("GITHUB_REPOSITORY environment variable not set")?;

        // Get event name (pull_request, push, etc.)
        let event_name = env::var("GITHUB_EVENT_NAME")
            .context("GITHUB_EVENT_NAME environment variable not set")?;

        // For PR events, get PR number and refs from event payload
        let event_path = env::var("GITHUB_EVENT_PATH")
            .context("GITHUB_EVENT_PATH environment variable not set")?;

        let event_data =
            fs::read_to_string(event_path).context("Failed to read GitHub event file")?;

        let event_json: Value =
            serde_json::from_str(&event_data).context("Failed to parse GitHub event JSON")?;

        // Extract values from event JSON
        let (pr_number, base_ref, head_ref) = match event_name.as_str() {
            "pull_request" | "pull_request_target" => {
                let pr_number = event_json["pull_request"]["number"]
                    .as_u64()
                    .context("Could not find pull_request.number in event data")?;

                // Direct access to base.ref
                let base_ref = match event_json["pull_request"]["base"]["ref"].as_str() {
                    Some(val) => val.to_owned(),
                    None => {
                        warn!(
                            "Could not extract base.ref as string, Falling back to main as base branch",
                        );

                        "main".to_owned()
                    }
                };

                // Direct access to head.ref
                let head_ref = match event_json["pull_request"]["head"]["ref"].as_str() {
                    Some(val) => val.to_owned(),
                    None => {
                        warn!("Warning: Could not extract head.ref as string");

                        String::new()
                    }
                };

                (pr_number, base_ref, head_ref)
            }
            _ => {
                // For other events, default values (will be overridden by args)
                (0, "main".to_owned(), "".to_owned())
            }
        };

        debug!(
            "Extracted PR: {}, base: {}, head: {}",
            pr_number, base_ref, head_ref
        );

        Ok(GitHubContext {
            repository,
            pr_number,
            base_ref,
            head_ref,
        })
    }
}

/// Configuration combining command-line arguments and GitHub context
#[derive(Debug)]
pub struct Config {
    pub repository: String,
    pub pr_number: u64,
    pub base_branch: String,
    pub head_branch: String,
    pub rules: Vec<String>,
    pub token: String,
    pub owner: String,
    pub repo: String,
}

impl Config {
    /// Create a new configuration from command-line arguments and GitHub context
    pub fn new(mut args: Args, github_ctx: GitHubContext) -> Result<Self> {
        // Check for empty token and try to get from environment if needed
        if args.token.is_empty() {
            args.token = env::var("GITHUB_TOKEN").map_err(|_| {
                anyhow::anyhow!("No token provided and GITHUB_TOKEN environment variable not set")
            })?;
        }

        // Use provided values from args if available, otherwise use context
        let repository = args.repo.as_ref().unwrap_or(&github_ctx.repository);

        let pr_number = match args.pr {
            Some(pr) => pr,
            None => {
                if github_ctx.pr_number == 0 {
                    return Err(anyhow::anyhow!(
                        "No PR number found in event context. Please provide --pr argument."
                    ));
                }
                github_ctx.pr_number
            }
        };

        // Set base branch (default to the PR's base branch or 'main')
        let base_branch = match args.base_branch.as_ref() {
            Some(branch) => {
                if !branch.is_empty() {
                    format!("origin/{}", branch)
                } else if !github_ctx.base_ref.is_empty() {
                    format!("origin/{}", github_ctx.base_ref)
                } else {
                    "origin/main".to_owned()
                }
            }
            _ => "origin/main".to_owned(),
        };

        // Set head branch (PR's head branch)
        let head_branch = if !github_ctx.head_ref.is_empty() {
            format!("origin/{}", github_ctx.head_ref)
        } else {
            env::var("GITHUB_HEAD_REF")
                .map(|ref_name| format!("origin/{}", ref_name))
                .unwrap_or_else(|_| "HEAD".to_owned())
        };

        // Parse repository into owner and repo
        let parts: Vec<&str> = repository.split('/').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!(
                "Invalid repository format. Expected 'owner/repo', got '{}'",
                repository
            ));
        }

        let owner = parts[0].to_owned();
        let repo = parts[1].to_owned();

        let rules = args.parse_rules();

        Ok(Config {
            repository: repository.to_owned(),
            pr_number,
            base_branch,
            head_branch,
            rules,
            owner,
            repo,
            token: args.token,
        })
    }
}

/// Builder for creating Config instances
pub struct ConfigBuilder {
    args: Option<Args>,
    github_ctx: Option<GitHubContext>,
}

impl ConfigBuilder {
    /// Create a new empty builder
    pub fn new() -> Self {
        Self {
            args: None,
            github_ctx: None,
        }
    }

    /// Build the Config instance, using defaults for any unset values
    pub fn build(self) -> Result<Config> {
        // Get command line arguments if not provided
        let args = match self.args {
            Some(args) => args,
            None => Args::from_cli()?,
        };

        // Get GitHub context if not provided
        let github_ctx = match self.github_ctx {
            Some(ctx) => ctx,
            None => GitHubContext::from_env()?,
        };

        Config::new(args, github_ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Helper function to create a mock GitHub event file
    fn create_mock_event_file(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("event.json");
        let mut file = File::create(&file_path).unwrap();
        write!(file, "{}", content).unwrap();
        (dir, file_path.to_string_lossy().to_string())
    }

    // Helper to set and reset environment variables safely
    struct EnvGuard {
        key: String,
        original_value: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &str, value: &str) -> Self {
            let original_value = env::var(key).ok();
            env::set_var(key, value);
            Self {
                key: key.to_string(),
                original_value,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original_value {
                Some(val) => env::set_var(&self.key, val),
                None => env::remove_var(&self.key),
            }
        }
    }

    // Test that directly creates Args without the builder
    #[test]
    fn test_args_parse_rules() {
        let args = Args {
            token: "dummy_token".to_string(),
            rules: "unwrap_used,expect_used,panic".to_string(),
            repo: None,
            pr: None,
            base_branch: None,
        };

        let rules = args.parse_rules();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0], "unwrap_used");
        assert_eq!(rules[1], "expect_used");
        assert_eq!(rules[2], "panic");
    }

    #[test]
    fn test_args_parse_rules_with_spaces() {
        let args = Args {
            token: "dummy_token".to_string(),
            rules: "unwrap_used, expect_used , panic".to_string(),
            repo: None,
            pr: None,
            base_branch: None,
        };

        let rules = args.parse_rules();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0], "unwrap_used");
        assert_eq!(rules[1], "expect_used");
        assert_eq!(rules[2], "panic");
    }

    // Test that uses environment variables for GitHub context
    #[test]
    fn test_github_context_from_env() {
        // Setup mock environment and event file
        let event_json = r#"
        {
            "pull_request": {
                "number": 123,
                "base": {
                    "ref": "main"
                },
                "head": {
                    "ref": "feature-branch"
                }
            }
        }
        "#;

        let (dir, event_path) = create_mock_event_file(event_json);

        // Set required environment variables
        let _guard1 = EnvGuard::new("GITHUB_REPOSITORY", "owner/repo");
        let _guard2 = EnvGuard::new("GITHUB_EVENT_NAME", "pull_request");
        let _guard3 = EnvGuard::new("GITHUB_EVENT_PATH", &event_path);

        let ctx = GitHubContext::from_env().unwrap();

        assert_eq!(ctx.repository, "owner/repo");
        assert_eq!(ctx.pr_number, 123);
        assert_eq!(ctx.base_ref, "main");
        assert_eq!(ctx.head_ref, "feature-branch");

        drop(dir);
    }

    // Test that directly creates Config
    #[test]
    fn test_config_new() {
        let args = Args {
            token: "test_token".to_string(),
            rules: "unwrap_used,expect_used".to_string(),
            repo: Some("custom_owner/custom_repo".to_string()),
            pr: Some(456),
            base_branch: Some("develop".to_string()),
        };

        let github_ctx = GitHubContext {
            repository: "default_owner/default_repo".to_string(),
            pr_number: 123,
            base_ref: "main".to_string(),
            head_ref: "feature".to_string(),
        };

        let config = Config::new(args, github_ctx).unwrap();

        // Check that args values are used when provided
        assert_eq!(config.repository, "custom_owner/custom_repo");
        assert_eq!(config.pr_number, 456);
        assert_eq!(config.base_branch, "origin/develop");
        assert_eq!(config.head_branch, "origin/feature");
        assert_eq!(config.owner, "custom_owner");
        assert_eq!(config.repo, "custom_repo");
        assert_eq!(config.rules.len(), 2);
        assert_eq!(config.rules[0], "unwrap_used");
        assert_eq!(config.rules[1], "expect_used");
        assert_eq!(config.token, "test_token");
    }

    // Test using mocked environment for token
    #[test]
    fn test_empty_token_uses_env_token() {
        // Create a mock GitHub event file
        let event_json =
            r#"{"pull_request":{"number":123,"base":{"ref":"main"},"head":{"ref":"feature"}}}"#;
        let (dir, event_path) = create_mock_event_file(event_json);

        // Set up GitHub context environment
        let _repo_guard = EnvGuard::new("GITHUB_REPOSITORY", "owner/repo");
        let _event_guard = EnvGuard::new("GITHUB_EVENT_NAME", "pull_request");
        let _path_guard = EnvGuard::new("GITHUB_EVENT_PATH", &event_path);

        // Set the token environment variable
        let _token_guard = EnvGuard::new("GITHUB_TOKEN", "env_token");

        // Create args and context directly
        let args = Args {
            token: "".to_string(), // Empty token should trigger using env var
            rules: "unwrap_used".to_string(),
            repo: Some("owner/repo".to_string()),
            pr: Some(123),
            base_branch: None,
        };

        let github_ctx = GitHubContext::from_env().unwrap();

        // Test Config::new directly
        let config = Config::new(args, github_ctx).unwrap();

        // Should use token from environment
        assert_eq!(config.token, "env_token");

        drop(dir);
    }

    #[test]
    fn test_config_new_invalid_repo_format() {
        let args = Args {
            token: "test_token".to_string(),
            rules: "unwrap_used".to_string(),
            repo: Some("invalid-format".to_string()),
            pr: Some(123),
            base_branch: None,
        };

        let github_ctx = GitHubContext {
            repository: "default_owner/default_repo".to_string(),
            pr_number: 0,
            base_ref: "".to_string(),
            head_ref: "".to_string(),
        };

        let result = Config::new(args, github_ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid repository format"));
    }
}
