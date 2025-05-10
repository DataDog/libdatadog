//! Configuration for the clippy-annotation-reporter
//!
//! This module handles all configuration-related logic including
//! command-line arguments, GitHub context, and environment variables.

use anyhow::{Context as _, Result};
use clap::Parser;
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
        self.rules
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    }
}

/// GitHub event context extracted from environment
#[derive(Debug)]
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

        println!("Event name: {}", event_name);
        println!("Event path: {}", event_path);

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
                    Some(val) => {
                        println!("Successfully found base.ref in event data: {}", val);
                        val.to_string()
                    }
                    None => {
                        println!(
                            "Warning: Could not extract base.ref as string, raw value: {:?}",
                            event_json["pull_request"]["base"]["ref"]
                        );

                        // Fallback to main if we can't extract the value
                        println!("Falling back to 'main' as base branch");
                        "main".to_string()
                    }
                };

                // Direct access to head.ref
                let head_ref = match event_json["pull_request"]["head"]["ref"].as_str() {
                    Some(val) => {
                        println!("Successfully found head.ref in event data: {}", val);
                        val.to_string()
                    }
                    None => {
                        println!("Warning: Could not extract head.ref as string");
                        // We'll use the current branch as fallback
                        String::new()
                    }
                };

                (pr_number, base_ref, head_ref)
            }
            _ => {
                // For other events, default values (will be overridden by args)
                (0, "main".to_string(), "".to_string())
            }
        };

        println!("Extracted PR number: {}", pr_number);
        println!("Extracted base branch: {}", base_ref);
        println!("Extracted head branch: {}", head_ref);

        Ok(GitHubContext {
            repository,
            pr_number,
            base_ref,
            head_ref,
        })
    }
}

/// Configuration combining command-line arguments and GitHub context
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
    pub fn new(args: Args, github_ctx: GitHubContext) -> Result<Self> {
        // Use provided values from args if available, otherwise use context
        // TODO: EK - FIX THIS CLONE
        let repository = args.clone().repo.unwrap_or(github_ctx.repository);

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
        // TODO: EK - FIX THIS CLONE
        // TODO: EK - unclear if we even need command line args for this
        let base_branch_arg = args.clone().base_branch.unwrap_or_default();

        let base_branch = if !base_branch_arg.is_empty() {
            format!("origin/{}", base_branch_arg)
        } else if !github_ctx.base_ref.is_empty() {
            format!("origin/{}", github_ctx.base_ref)
        } else {
            "origin/main".to_string()
        };

        // Set head branch (PR's head branch)
        let head_branch = if !github_ctx.head_ref.is_empty() {
            format!("origin/{}", github_ctx.head_ref)
        } else {
            env::var("GITHUB_HEAD_REF")
                .map(|ref_name| format!("origin/{}", ref_name))
                .unwrap_or_else(|_| "HEAD".to_string())
        };

        // Parse repository into owner and repo
        let parts: Vec<&str> = repository.split('/').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!(
                "Invalid repository format. Expected 'owner/repo', got '{}'",
                repository
            ));
        }

        let owner = parts[0].to_string();
        let repo = parts[1].to_string();

        // Parse rules list
        let rules = args.parse_rules();

        Ok(Config {
            repository,
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

    /// Set the command-line arguments
    pub fn with_args(mut self, args: Args) -> Self {
        self.args = Some(args);
        self
    }

    /// Set the GitHub context
    pub fn with_github_context(mut self, github_ctx: GitHubContext) -> Self {
        self.github_ctx = Some(github_ctx);
        self
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
