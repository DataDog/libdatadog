// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CLI tool to add file attributes to JUnit XML test reports

use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::path::PathBuf;
use tools::junit_file_attributes::{build_target_lookup, process_junit_xml};

#[derive(Parser, Debug)]
#[command(name = "add_junit_file_attributes")]
#[command(about = "Add file attributes to JUnit XML test reports using cargo metadata")]
struct Args {
    /// Input JUnit XML file
    input: PathBuf,

    /// Output JUnit XML file (defaults to overwriting input)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Path to Cargo.toml (defaults to finding workspace root)
    #[arg(short = 'C', long)]
    manifest_path: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Build target lookup from cargo metadata
    let (targets, workspace_root) = build_target_lookup(args.manifest_path.as_deref())?;

    // Read input file
    let input = fs::read_to_string(&args.input)
        .with_context(|| format!("failed to read {}", args.input.display()))?;

    // Process the XML
    let output = process_junit_xml(&input, &targets, &workspace_root)?;

    // Write output
    let output_path = args.output.as_ref().unwrap_or(&args.input);
    fs::write(output_path, output)
        .with_context(|| format!("failed to write {}", output_path.display()))?;

    Ok(())
}
