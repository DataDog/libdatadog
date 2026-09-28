// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use log::info;
use octocrab::Octocrab;
use std::env;

mod analyzer;
mod commenter;
mod config;
mod report_generator;

use crate::config::ConfigBuilder;
use crate::report_generator::generate_report;

#[tokio::main]
async fn main() -> Result<()> {
    let log_level = env::var("INPUT_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

    // Set RUST_LOG if not already set
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", &log_level);
    }

    env_logger::init();

    info!("Clippy Annotation Reporter starting...");

    let config = ConfigBuilder::new().build()?;

    let octocrab = Octocrab::builder()
        .personal_token(config.token.clone())
        .build()
        .context("Failed to build GitHub API client")?;

    let analysis_result = match analyzer::run_analysis(
        &octocrab,
        &config.owner,
        &config.repo,
        config.pr_number,
        &config.base_branch,
        &config.head_branch,
        &config.rules,
    )
    .await?
    {
        Some(result) => result,
        None => {
            // No Rust files changed. A prior revision of this PR may have touched
            // Rust and posted a comment, so remove any stale one.
            info!("No Rust files changed in this PR; removing any stale comment.");
            commenter::delete_comment_if_exists(
                &octocrab,
                &config.owner,
                &config.repo,
                config.pr_number,
                None,
            )
            .await?;
            return Ok(());
        }
    };

    // Nothing changed: don't post a comment, and remove any stale one left over
    // from an earlier revision of this PR.
    if !analysis_result.has_changes() {
        info!("No changes to tracked clippy annotations; removing any stale comment.");
        commenter::delete_comment_if_exists(
            &octocrab,
            &config.owner,
            &config.repo,
            config.pr_number,
            None,
        )
        .await?;
        info!("Process completed successfully!");
        return Ok(());
    }

    let report = generate_report(
        &analysis_result,
        &config.rules,
        &config.repository,
        &config.base_branch,
    );

    commenter::post_comment(
        &octocrab,
        &config.owner,
        &config.repo,
        config.pr_number,
        report,
        None,
    )
    .await?;

    info!("Process completed successfully!");

    Ok(())
}
