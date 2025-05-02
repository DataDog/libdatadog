use anyhow::{Context as _, Result};
use clap::Parser;
use octocrab::Octocrab;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(name = "clippy-annotation-reporter")]
#[command(about = "Reports changes in clippy allow annotations")]
struct Args {
    /// GitHub token for API access
    #[arg(long)]
    token: String,

    /// Comma-separated list of clippy rules to track
    #[arg(long, default_value = "unwrap_used,expect_used,todo,unimplemented,panic,unreachable")]
    rules: String,

    /// GitHub repository (owner/repo) - defaults to current repository
    #[arg(long)]
    repo: Option<String>,

    /// Pull request number - defaults to PR from event context
    #[arg(long)]
    pr: Option<u64>,

    /// Base branch to compare against (defaults to the PR's base branch)
    #[arg(long)]
    base_branch: Option<String>,
}

/// GitHub event context extracted from environment
struct GitHubContext {
    repository: String,
    pr_number: u64,
    event_name: String,
    base_ref: String,
    head_ref: String,
}

impl GitHubContext {
    /// Try to extract GitHub context from environment variables and event file
    fn from_env() -> Result<Self> {
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

        let event_data = fs::read_to_string(event_path)
            .context("Failed to read GitHub event file")?;

        let event_json: Value = serde_json::from_str(&event_data)
            .context("Failed to parse GitHub event JSON")?;

        // Extract values from event JSON
        let (pr_number, base_ref, head_ref) = match event_name.as_str() {
            "pull_request" | "pull_request_target" => {
                let pr_number = event_json["pull_request"]["number"].as_u64()
                    .context("Could not find pull_request.number in event data")?;

                let base_ref = event_json["pull_request"]["base"]["ref"].as_str()
                    .context("Could not find pull_request.base.ref in event data")?
                    .to_string();
                println!("base ref is: {}", base_ref);
                let head_ref = event_json["pull_request"]["head"]["ref"].as_str()
                    .context("Could not find pull_request.head.ref in event data")?
                    .to_string();

                (pr_number, base_ref, head_ref)
            },
            _ => {
                // For other events, default values (will be overridden by args)
                (0, "main".to_string(), "".to_string())
            }
        };

        Ok(GitHubContext {
            repository,
            pr_number,
            event_name,
            base_ref,
            head_ref,
        })
    }
}

/// Represents a clippy annotation in code
#[derive(Debug, Clone)]
struct ClippyAnnotation {
    file: String,
    rule: String,
    line_content: String,
}

/// Result of annotation analysis
struct AnnotationAnalysis {
    base_annotations: Vec<ClippyAnnotation>,
    head_annotations: Vec<ClippyAnnotation>,
    base_counts: HashMap<String, usize>,
    head_counts: HashMap<String, usize>,
    changed_files: HashSet<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();

    // Parse command line arguments
    let mut args = Args::parse();

    // Check for token in environment if not provided as argument
    if args.token.is_empty() {
        args.token = env::var("GITHUB_TOKEN")
            .context("No token provided and GITHUB_TOKEN environment variable not set")?;
    }

    println!("Clippy Annotation Reporter starting...");

    // Try to get GitHub context from environment
    let github_ctx = GitHubContext::from_env()?;

    // Use provided values from args if available, otherwise use context
    let repository = args.repo.unwrap_or(github_ctx.repository);
    let pr_number = match args.pr {
        Some(pr) => pr,
        None => {
            if github_ctx.pr_number == 0 {
                anyhow::bail!("No PR number found in event context. Please provide --pr argument.");
            }
            github_ctx.pr_number
        }
    };

    // TODO: EK - Unsure if we even need command line args for base branch
    let base_branch_arg = args.base_branch.clone().unwrap_or_else(|| "".to_owned());

    let base_branch = if base_branch_arg.is_empty() {
        if !github_ctx.base_ref.is_empty() {
            format!("origin/{}", github_ctx.base_ref)
        } else {
            "origin/main".to_string()
        }
    } else {
        format!("origin/{}", base_branch_arg)
    };

    println!("base branch is: {}", base_branch);

    // Set base branch (default to the PR's base branch or 'main')
    // let base_branch = args.base_branch.unwrap_or_else(|| {
    //     if !github_ctx.base_ref.is_empty() {
    //         format!("origin/{}", github_ctx.base_ref)
    //     } else {
    //         "origin/main".to_string()
    //     }
    // });
    // let input_base_branch = args.base_branch;
    // println!("Input base branch: {:?}", input_base_branch);
    // let base_branch = "origin/main";

    // Set head branch (PR's head branch)
    let head_branch = if !github_ctx.head_ref.is_empty() {
        format!("origin/{}", github_ctx.head_ref)
    } else {
        env::var("GITHUB_HEAD_REF")
            .map(|ref_name| format!("origin/{}", ref_name))
            .unwrap_or_else(|_| "HEAD".to_string())
    };

    println!("Repository: {}", repository);
    println!("PR Number: {}", pr_number);
    println!("Base Branch: {}", base_branch);
    println!("Head Branch: {}", head_branch);
    println!("Event Type: {}", github_ctx.event_name);

    // Parse the rules list
    let rules: Vec<String> = args.rules.split(',').map(|s| s.trim().to_string()).collect();
    println!("Tracking rules: {}", args.rules);

    // Split repository into owner and name
    let repo_parts: Vec<&str> = repository.split('/').collect();
    if repo_parts.len() != 2 {
        anyhow::bail!("Invalid repository format. Expected 'owner/repo', got '{}'", repository);
    }
    let owner = repo_parts[0];
    let repo = repo_parts[1];

    // Initialize GitHub API client with token
    let octocrab = Octocrab::builder()
        .personal_token(args.token.clone())
        .build()
        .context("Failed to build GitHub API client")?;

    // 1. Get changed files in the PR
    println!("Getting changed files from PR #{}", pr_number);
    let changed_files = get_changed_files(&octocrab, owner, repo, pr_number).await
        .context("Failed to get changed files from PR")?;

    if changed_files.is_empty() {
        println!("No Rust files changed in this PR");
        return Ok(());
    }

    // 2. Analyze annotations in base and head branches
    println!("Analyzing clippy annotations...");
    let analysis = analyze_annotations(&changed_files, &base_branch, &head_branch, &rules)
        .context("Failed to analyze annotations")?;

    // 3. Generate a report
    let report = generate_report(&analysis, &rules, &base_branch);

    // Create a unique signature for the bot's comments
    let bot_signature = "<!-- clippy-annotation-reporter-comment -->";
    let report_with_signature = format!("{}\n\n{}", report, bot_signature);

    // Search for existing comment by the bot
    println!("Checking for existing comment on PR #{}", pr_number);
    let existing_comment = find_existing_comment(
        &octocrab, owner, repo, pr_number, bot_signature
    ).await?;

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
async fn find_existing_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    signature: &str
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
            if comment.body.as_ref().map_or(false, |body| body.contains(signature)) {
                return Ok(Some(*comment.id));
            }
        }

        // Try to get the next page if it exists
        match octocrab.get_page(&page.next).await {
            Ok(Some(next_page)) => {
                page = next_page;
            },
            Ok(None) => {
                // No more pages
                break;
            },
            Err(e) => {
                println!("Warning: Failed to fetch next page of comments: {}", e);
                break;
            }
        }
    }

    // No matching comment found
    Ok(None)
}

/// Get changed Rust files from the PR
async fn get_changed_files(octocrab: &Octocrab, owner: &str, repo: &str, pr_number: u64) -> Result<Vec<String>> {
    let files = octocrab
        .pulls(owner, repo)
        .list_files(pr_number)
        .await
        .context("Failed to list PR files")?;

    // Filter for Rust files only
    let rust_files = files
        .items
        .into_iter()
        .filter(|file| file.filename.ends_with(".rs"))
        .map(|file| file.filename)
        .collect();

    Ok(rust_files)
}

/// Analyze clippy annotations in base and head branches
fn analyze_annotations(
    files: &[String],
    base_branch: &str,
    head_branch: &str,
    rules: &[String]
) -> Result<AnnotationAnalysis> {
    // Create a regex for matching clippy allow annotations
    // This will capture the rule name in the first capture group
    let rule_pattern = rules.join("|");
    let annotation_regex = Regex::new(&format!(
        r"#\s*\[\s*allow\s*\(\s*clippy\s*::\s*({})\s*\)\s*\]", rule_pattern
    )).context("Failed to compile annotation regex")?;

    let mut base_annotations = Vec::new();
    let mut head_annotations = Vec::new();
    let mut changed_files = HashSet::new();

    // Process each file
    for file in files {
        changed_files.insert(file.clone());

        // Get file content from base branch
        let base_content = match get_file_content(file, base_branch) {
            Ok(content) => content,
            Err(e) => {
                println!("Warning: Failed to get {} content from {}: {}", file, base_branch, e);
                String::new()
            }
        };

        // Get file content from head branch
        let head_content = match get_file_content(file, head_branch) {
            Ok(content) => content,
            Err(e) => {
                println!("Warning: Failed to get {} content from {}: {}", file, head_branch, e);
                String::new()
            }
        };

        // Find annotations in base branch
        find_annotations(&mut base_annotations, file, &base_content, &annotation_regex);

        // Find annotations in head branch
        find_annotations(&mut head_annotations, file, &head_content, &annotation_regex);
    }

    // Count annotations by rule
    let base_counts = count_annotations_by_rule(&base_annotations);
    let head_counts = count_annotations_by_rule(&head_annotations);

    Ok(AnnotationAnalysis {
        base_annotations,
        head_annotations,
        base_counts,
        head_counts,
        changed_files,
    })
}

/// Get file content from a specific branch
fn get_file_content(file: &str, branch: &str) -> Result<String> {
    println!("Getting content for {} from {}", file, branch);

    let output = Command::new("git")
        .args(["show", &format!("{}:{}", branch, file)])
        .output()
        .context(format!("Failed to execute git show command for {}", file))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git show command failed: {}", stderr);
    }

    let content = String::from_utf8(output.stdout)
        .context("Failed to parse file content as UTF-8")?;

    // Debug count
    let count = content.matches("#[allow(clippy::").count();
    println!("Found {} clippy allow annotations in {}:{}", count, branch, file);

    Ok(content)
}

/// Find clippy annotations in file content
fn find_annotations(
    annotations: &mut Vec<ClippyAnnotation>,
    file: &str,
    content: &str,
    regex: &Regex
) {
    for (_line_number, line) in content.lines().enumerate() {
        if let Some(captures) = regex.captures(line) {
            if let Some(rule_match) = captures.get(1) {
                let rule = rule_match.as_str().to_string();
                annotations.push(ClippyAnnotation {
                    file: file.to_string(),
                    rule,
                    line_content: line.trim().to_string(),
                });
            }
        }
    }
}

/// Count annotations by rule
fn count_annotations_by_rule(annotations: &[ClippyAnnotation]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();

    for annotation in annotations {
        *counts.entry(annotation.rule.clone()).or_insert(0) += 1;
    }

    counts
}

/// Generate a detailed report for PR comment
fn generate_report(analysis: &AnnotationAnalysis, rules: &[String], base_branch: &str) -> String {
    let mut report = String::from("## Clippy Allow Annotation Report\n\n");
    // Add simple branch information
    report.push_str(&format!("Comparing against base branch: **{}**\n\n", base_branch));
    // Summary table by rule
    report.push_str("### Summary by Rule\n\n");
    report.push_str("| Rule | Base Branch | PR Branch | Change |\n");
    report.push_str("|------|------------|-----------|--------|\n");

    let mut total_base = 0;
    let mut total_head = 0;

    for rule in rules {
        let base_count = *analysis.base_counts.get(rule).unwrap_or(&0);
        let head_count = *analysis.head_counts.get(rule).unwrap_or(&0);
        let change = head_count as isize - base_count as isize;

        total_base += base_count;
        total_head += head_count;

        let change_str = if change > 0 {
            format!("⚠️ +{}", change)
        } else if change < 0 {
            format!("✅ {}", change)
        } else {
            "No change".to_string()
        };

        report.push_str(&format!("| `{}` | {} | {} | {} |\n",
                                 rule, base_count, head_count, change_str));
    }

    // Add total row
    let total_change = total_head as isize - total_base as isize;
    let total_change_str = if total_change > 0 {
        format!("⚠️ +{}", total_change)
    } else if total_change < 0 {
        format!("✅ {}", total_change)
    } else {
        "No change".to_string()
    };

    report.push_str(&format!("| **Total** | **{}** | **{}** | **{}** |\n\n",
                             total_base, total_head, total_change_str));

    // File-level annotation counts
    if !analysis.changed_files.is_empty() {
        report.push_str("### Annotation Counts by File\n\n");
        report.push_str("| File | Base Branch | PR Branch | Change |\n");
        report.push_str("|------|------------|-----------|--------|\n");

        // Count annotations by file in base branch
        let mut base_file_counts = HashMap::new();
        for anno in &analysis.base_annotations {
            *base_file_counts.entry(anno.file.clone()).or_insert(0) += 1;
        }

        // Count annotations by file in head branch
        let mut head_file_counts = HashMap::new();
        for anno in &analysis.head_annotations {
            *head_file_counts.entry(anno.file.clone()).or_insert(0) += 1;
        }

        // Get a sorted list of all files
        let mut all_files: Vec<String> = analysis.changed_files.iter().cloned().collect();
        all_files.sort();

        // Generate table rows
        for file in all_files {
            let base_count = *base_file_counts.get(&file).unwrap_or(&0);
            let head_count = *head_file_counts.get(&file).unwrap_or(&0);
            let change = head_count as isize - base_count as isize;

            // Skip files with no changes in annotation count
            if change == 0 && base_count == 0 && head_count == 0 {
                continue;
            }

            let change_str = if change > 0 {
                format!("⚠️ +{}", change)
            } else if change < 0 {
                format!("✅ {}", change)
            } else {
                "No change".to_string()
            };

            report.push_str(&format!("| `{}` | {} | {} | {} |\n",
                                     file, base_count, head_count, change_str));
        }

        report.push_str("\n");
    }

    // Add explanation
    report.push_str("### About This Report\n\n");
    report.push_str("This report tracks Clippy allow annotations for specific rules, ");
    report.push_str("showing how they've changed in this PR. ");
    report.push_str("Decreasing the number of these annotations generally improves code quality.\n");

    report
}
