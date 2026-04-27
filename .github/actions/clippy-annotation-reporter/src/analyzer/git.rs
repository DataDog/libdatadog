// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use log::{debug, info};
use octocrab::Octocrab;
use std::process::{Command, Output};

// Define a trait for command execution
#[cfg_attr(test, mockall::automock)]
pub trait CommandExecutor {
    fn execute_command<'a>(&self, command: &str, args: &'a [&'a str]) -> std::io::Result<Output>;
}

pub struct RealCommandExecutor;

impl CommandExecutor for RealCommandExecutor {
    fn execute_command<'a>(&self, command: &str, args: &'a [&'a str]) -> std::io::Result<Output> {
        Command::new(command).args(args).output()
    }
}

// Default implementation for RealCommandExecutor
impl Default for RealCommandExecutor {
    fn default() -> Self {
        Self
    }
}

// Git operations struct that takes a CommandExecutor
pub struct GitOperations<T: CommandExecutor> {
    executor: T,
}

impl<T: CommandExecutor> GitOperations<T> {
    // Constructor with explicit executor
    pub fn new(executor: T) -> Self {
        Self { executor }
    }

    /// Get file content from a specific branch
    pub fn get_file_content(&self, file: &str, branch: &str) -> Result<String> {
        debug!("Getting content for {} from {}", file, branch);

        let output = self
            .executor
            .execute_command("git", &["show", &format!("{}:{}", branch, file)])
            .context(format!("Failed to execute git show command for {}", file))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git show command failed: {}", stderr);
        }

        let content =
            String::from_utf8(output.stdout).context("Failed to parse file content as UTF-8")?;

        Ok(content)
    }

    /// Get file content from a branch, handling common errors
    pub fn get_branch_content(&self, file: &str, branch: &str) -> String {
        match self.get_file_content(file, branch) {
            Ok(content) => content,
            Err(e) => {
                // Skip errors for files that might not exist in one branch
                if !e.to_string().contains("did not match any file") {
                    log::warn!("Failed to get {} content from {}: {}", file, branch, e);
                }
                String::new()
            }
        }
    }

    /// Get all Rust files in the repository
    pub fn get_all_rust_files(&self) -> Result<Vec<String>> {
        info!("Getting all Rust files in the repository...");

        let output = self
            .executor
            .execute_command("git", &["ls-files", "*.rs"])
            .context("Failed to execute git ls-files command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git ls-files command failed: {}", stderr);
        }

        let files =
            String::from_utf8(output.stdout).context("Failed to parse git ls-files output")?;

        let rust_files: Vec<String> = files.lines().map(|line| line.to_owned()).collect();

        info!("Found {} Rust files in total", rust_files.len());

        Ok(rust_files)
    }
}

// Default implementation for GitOperations with RealCommandExecutor
impl Default for GitOperations<RealCommandExecutor> {
    fn default() -> Self {
        Self::new(RealCommandExecutor::default())
    }
}

// Standalone function for getting changed files from GitHub PR
pub async fn get_changed_files(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<String>> {
    info!("Getting changed files from PR #{}...", pr_number);

    let first_files = octocrab
        .pulls(owner, repo)
        .list_files(pr_number)
        .await
        .context("Failed to list PR files")?;

    let all_files = octocrab
        .all_pages(first_files)
        .await
        .context("Failed to fetch all pages of PR files")?;

    // Filter for Rust files only
    let rust_files: Vec<String> = all_files
        .into_iter()
        .filter(|file| file.filename.ends_with(".rs"))
        .map(|file| file.filename)
        .collect();

    info!("Found {} changed Rust files", rust_files.len());

    Ok(rust_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Uri;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use mockall::predicate::*;
    use serde_json::json;
    use std::io::{Error as IoError, ErrorKind};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};
    use std::str::FromStr;

    // Helper function to create mock output
    fn create_mock_output(status: i32, stdout: &str, stderr: &str) -> std::io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    // Helper function to create test octocrab
    fn create_test_octocrab(server: &MockServer) -> Octocrab {
        let uri = Uri::from_str(&server.base_url()).unwrap();
        Octocrab::builder().base_uri(uri).unwrap().build().unwrap()
    }

    #[test]
    fn test_get_file_content_success() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/test.rs"
            })
            .times(1)
            .returning(|_, _| create_mock_output(0, "fn test() {}\n", ""));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_file_content("src/test.rs", "main");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "fn test() {}\n");
    }

    #[test]
    fn test_get_file_content_command_error() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/test.rs"
            })
            .times(1)
            .returning(|_, _| Err(IoError::new(ErrorKind::NotFound, "git command not found")));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_file_content("src/test.rs", "main");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to execute git show command"));
    }

    #[test]
    fn test_get_file_content_git_error() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/nonexistent.rs"
            })
            .times(1)
            .returning(|_, _| {
                create_mock_output(
                    1,
                    "",
                    "fatal: Path 'src/nonexistent.rs' does not exist in 'main'",
                )
            });

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_file_content("src/nonexistent.rs", "main");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Git show command failed"));
    }

    #[test]
    fn test_get_file_content_invalid_utf8() {
        let mut mock_executor = MockCommandExecutor::new();

        // Create invalid UTF-8 output
        let invalid_utf8 = vec![0, 159, 146, 150]; // Invalid UTF-8 sequence

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/binary.rs"
            })
            .times(1)
            .returning(move |_, _| {
                Ok(Output {
                    status: ExitStatus::from_raw(0),
                    stdout: invalid_utf8.clone(),
                    stderr: Vec::new(),
                })
            });

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_file_content("src/binary.rs", "main");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse file content as UTF-8"));
    }

    #[test]
    fn test_get_branch_content_success() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/test.rs"
            })
            .times(1)
            .returning(|_, _| create_mock_output(0, "fn test() {}\n", ""));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_branch_content("src/test.rs", "main");

        assert_eq!(result, "fn test() {}\n");
    }

    #[test]
    fn test_get_branch_content_file_not_found() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "main:src/nonexistent.rs"
            })
            .times(1)
            .returning(|_, _| {
                create_mock_output(
                    1,
                    "",
                    "fatal: Path 'src/nonexistent.rs' does not exist in 'main'",
                )
            });

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_branch_content("src/nonexistent.rs", "main");

        assert_eq!(result, ""); // Should return empty string for non-existent file
    }

    #[test]
    fn test_get_branch_content_other_error() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git"
                    && args.len() == 2
                    && args[0] == "show"
                    && args[1] == "invalid-branch:src/test.rs"
            })
            .times(1)
            .returning(|_, _| {
                create_mock_output(1, "", "fatal: invalid branch name: invalid-branch")
            });

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_branch_content("src/test.rs", "invalid-branch");

        assert_eq!(result, ""); // Should return empty string for any error
    }

    #[test]
    fn test_get_all_rust_files_success() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git" && args.len() == 2 && args[0] == "ls-files" && args[1] == "*.rs"
            })
            .times(1)
            .returning(|_, _| {
                create_mock_output(0, "src/main.rs\nsrc/lib.rs\nsrc/module.rs\n", "")
            });

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_all_rust_files();

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], "src/main.rs");
        assert_eq!(files[1], "src/lib.rs");
        assert_eq!(files[2], "src/module.rs");
    }

    #[test]
    fn test_get_all_rust_files_empty_repo() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git" && args.len() == 2 && args[0] == "ls-files" && args[1] == "*.rs"
            })
            .times(1)
            .returning(|_, _| create_mock_output(0, "", ""));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_all_rust_files();

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 0); // Should return empty list for repo with no Rust files
    }

    #[test]
    fn test_get_all_rust_files_command_error() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git" && args.len() == 2 && args[0] == "ls-files" && args[1] == "*.rs"
            })
            .times(1)
            .returning(|_, _| Err(IoError::new(ErrorKind::NotFound, "git command not found")));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_all_rust_files();

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to execute git ls-files command"));
    }

    #[test]
    fn test_get_all_rust_files_git_error() {
        let mut mock_executor = MockCommandExecutor::new();

        mock_executor
            .expect_execute_command()
            .withf(|cmd, args| {
                cmd == "git" && args.len() == 2 && args[0] == "ls-files" && args[1] == "*.rs"
            })
            .times(1)
            .returning(|_, _| create_mock_output(1, "", "fatal: not a git repository"));

        let git_ops = GitOperations::new(mock_executor);

        let result = git_ops.get_all_rust_files();

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Git ls-files command failed"));
    }

    // Tests for get_changed_files
    #[tokio::test]
    async fn test_get_changed_files_success() {
        let server = MockServer::start();

        let list_files_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/pulls/123/files");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([
                    {
                        "sha": "abc123",
                        "filename": "src/main.rs",
                        "status": "modified",
                        "additions": 10,
                        "deletions": 5,
                        "changes": 15,
                        "blob_url": "https://github.com/test-owner/test-repo/blob/abc123/src/main.rs",
                        "raw_url": "https://github.com/test-owner/test-repo/raw/abc123/src/main.rs",
                        "contents_url": "https://api.github.com/repos/test-owner/test-repo/contents/src/main.rs?ref=abc123"
                    },
                    {
                        "sha": "def456",
                        "filename": "src/lib.rs",
                        "status": "modified",
                        "additions": 7,
                        "deletions": 3,
                        "changes": 10,
                        "blob_url": "https://github.com/test-owner/test-repo/blob/def456/src/lib.rs",
                        "raw_url": "https://github.com/test-owner/test-repo/raw/def456/src/lib.rs",
                        "contents_url": "https://api.github.com/repos/test-owner/test-repo/contents/src/lib.rs?ref=def456"
                    },
                    {
                        "sha": "ghi789",
                        "filename": "README.md",
                        "status": "modified",
                        "additions": 2,
                        "deletions": 0,
                        "changes": 2,
                        "blob_url": "https://github.com/test-owner/test-repo/blob/ghi789/README.md",
                        "raw_url": "https://github.com/test-owner/test-repo/raw/ghi789/README.md",
                        "contents_url": "https://api.github.com/repos/test-owner/test-repo/contents/README.md?ref=ghi789"
                    }
                ]));
        });

        let octocrab = create_test_octocrab(&server);

        let result = get_changed_files(&octocrab, "test-owner", "test-repo", 123).await;

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 2); // Should only include the .rs files
        assert_eq!(files[0], "src/main.rs");
        assert_eq!(files[1], "src/lib.rs");

        list_files_mock.assert();
    }

    #[tokio::test]
    async fn test_get_changed_files_no_rust_files() {
        let server = MockServer::start();

        let list_files_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/pulls/123/files");

            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!([
                    {
                        "sha": "abc123",
                        "filename": "README.md",
                        "status": "modified",
                        "additions": 10,
                        "deletions": 5,
                        "changes": 15,
                        "blob_url": "https://github.com/test-owner/test-repo/blob/abc123/README.md",
                        "raw_url": "https://github.com/test-owner/test-repo/raw/abc123/README.md",
                        "contents_url": "https://api.github.com/repos/test-owner/test-repo/contents/README.md?ref=abc123"
                    },
                    {
                        "sha": "def456",
                        "filename": "LICENSE",
                        "status": "modified",
                        "additions": 7,
                        "deletions": 3,
                        "changes": 10,
                        "blob_url": "https://github.com/test-owner/test-repo/blob/def456/LICENSE",
                        "raw_url": "https://github.com/test-owner/test-repo/raw/def456/LICENSE",
                        "contents_url": "https://api.github.com/repos/test-owner/test-repo/contents/LICENSE?ref=def456"
                    }
                ]));
        });

        let octocrab = create_test_octocrab(&server);

        let result = get_changed_files(&octocrab, "test-owner", "test-repo", 123).await;

        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 0); // Should return empty list as no Rust files changed

        list_files_mock.assert();
    }

    #[tokio::test]
    async fn test_get_changed_files_error() {
        let server = MockServer::start();

        let list_files_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/test-owner/test-repo/pulls/123/files");

            then.status(404)
                .header("content-type", "application/json")
                .json_body(json!({
                    "message": "Not Found",
                    "documentation_url": "https://docs.github.com/rest/pulls/pulls#list-pull-requests-files"
                }));
        });

        let octocrab = create_test_octocrab(&server);

        let result = get_changed_files(&octocrab, "test-owner", "test-repo", 123).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to list PR files"));

        list_files_mock.assert();
    }
}
