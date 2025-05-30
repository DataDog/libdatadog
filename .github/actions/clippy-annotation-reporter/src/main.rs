// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use log::info;
use octocrab::Octocrab;

mod analyzer;
mod commenter;
mod config;
mod report_generator;

use crate::config::ConfigBuilder;
use crate::report_generator::generate_report;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    info!("Clippy Annotation Reporter starting...");

    let config = ConfigBuilder::new().build()?;

    // TODO: EK - Should we use context here?
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
    .await
    {
        Ok(result) => result,
        Err(e) => {
            if e.to_string().contains("No Rust files changed") {
                info!("No Rust files changed in this PR, nothing to analyze.");
                return Ok(());
            }
            return Err(e);
        }
    };

    let report = generate_report(
        &analysis_result,
        &config.rules,
        &config.repository,
        &config.base_branch,
        &config.head_branch,
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
