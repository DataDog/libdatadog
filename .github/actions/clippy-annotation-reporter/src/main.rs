use anyhow::{Context as _, Result};
use octocrab::Octocrab;

mod analyzer;
mod commenter;
mod config;
mod report_generator;

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

    // 3. Perform analysis
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

    // Post or update the report as a PR comment
    let commenter =
        commenter::Commenter::new(&octocrab, &config.owner, &config.repo, config.pr_number);

    commenter.run(report).await?;

    println!("Process completed successfully!");

    Ok(())
}
