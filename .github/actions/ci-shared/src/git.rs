// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

/// Local ref the PR's test merge commit is fetched into.
const PR_MERGE_REF: &str = "refs/ci-shared/pr-merge";

/// How long to wait before each retry when there is no up-to-date test merge commit yet: GitHub
/// recomputes `refs/pull/<N>/merge` asynchronously after a push to either side.
pub const PR_MERGE_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(20)];

/// The two revisions whose diff is the set of changes under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRange {
    pub base: String,
    pub head: String,
}

impl DiffRange {
    /// Changes on `HEAD` since it branched off `origin/<base_ref>`, i.e.
    /// `origin/<base_ref>...HEAD`.
    pub fn merge_base(base_ref: &str) -> Result<Self> {
        Self::merge_base_in(Path::new("."), base_ref)
    }

    fn merge_base_in(repo: &Path, base_ref: &str) -> Result<Self> {
        let head = git(repo, &["rev-parse", "HEAD^{commit}"])?;
        let base = git(repo, &["merge-base", &format!("origin/{base_ref}"), &head])
            .with_context(|| format!("No merge-base between HEAD and origin/{base_ref}"))?;
        Ok(Self { base, head })
    }

    /// Changes `HEAD` brings when merged into `origin/<base_ref>`, according to GitHub's test
    /// merge commit (`refs/pull/<N>/merge`): the diff between its first parent (the target
    /// branch it was merged into) and itself.
    ///
    /// Returns `None` when there is no such commit for `HEAD`: it is not the head of an open PR,
    /// the PR has conflicts, targets another branch, or GitHub did not recompute the merge in
    /// time. `HEAD` itself must be the PR head, not the merge (unlike in GitHub Actions'
    /// `pull_request` checkouts, where `merge_base` already yields the same range).
    pub fn pr_merge(base_ref: &str) -> Result<Option<Self>> {
        Self::pr_merge_in(Path::new("."), base_ref, &PR_MERGE_RETRY_DELAYS)
    }

    fn pr_merge_in(repo: &Path, base_ref: &str, retry_delays: &[Duration]) -> Result<Option<Self>> {
        let head = git(repo, &["rev-parse", "HEAD^{commit}"])?;
        let target = format!("origin/{base_ref}");
        // Matching the head SHA against refs/pull/*/head finds the PR without a GitHub API call
        // (rate limits, tokens). GitHub keeps those refs after a PR is closed, so several PRs can
        // match (e.g. a PR reopened as a new one from the same branch). Only open PRs have a merge
        // ref, and `try_pr_merge` keeps one that merges HEAD into the target.
        let pr_refs = git(repo, &["ls-remote", "origin", "refs/pull/*/head"])?;
        let prs: Vec<&str> = pr_refs
            .lines()
            .filter_map(|line| {
                let (sha, name) = line.split_once('\t')?;
                (sha == head).then_some(name.strip_suffix("/head")?)
            })
            .collect();
        if prs.is_empty() {
            log::info!("{head} is not the head of any PR");
            return Ok(None);
        }

        // Try every candidate before waiting, so that closed PRs cost nothing.
        let delays = std::iter::once(None).chain(retry_delays.iter().map(Some));
        for delay in delays {
            if let Some(delay) = delay {
                log::info!(
                    "No up-to-date test merge commit for {head} yet; retrying in {}s",
                    delay.as_secs()
                );
                sleep(*delay);
            }
            for pr in &prs {
                if let Some(range) = Self::try_pr_merge(repo, pr, &head, &target)? {
                    return Ok(Some(range));
                }
            }
        }
        Ok(None)
    }

    /// Fetches the test merge commit of `pr` (`refs/pull/<N>`), and returns it if it merges
    /// `head` into `target`.
    fn try_pr_merge(repo: &Path, pr: &str, head: &str, target: &str) -> Result<Option<Self>> {
        let fetch_spec = format!("+{pr}/merge:{PR_MERGE_REF}");
        if let Err(e) = git(repo, &["fetch", "--quiet", "origin", &fetch_spec]) {
            log::info!("No test merge commit for {pr} (closed, or conflicts?): {e:#}");
            return Ok(None);
        }
        let merge = git(repo, &["rev-parse", PR_MERGE_REF])?;
        let merged_head = git(repo, &["rev-parse", &format!("{merge}^2")])?;
        if merged_head != head {
            log::info!("{pr}/merge merges {merged_head}, not {head}: not recomputed yet");
            return Ok(None);
        }
        let base = git(repo, &["rev-parse", &format!("{merge}^1")])?;
        if !is_ancestor(repo, &base, target)? {
            log::info!("{pr} does not merge into {target}");
            return Ok(None);
        }
        log::info!("Using {merge}, test merge of {head} into {target} ({pr})");
        Ok(Some(Self { base, head: merge }))
    }
}

/// Runs git in `repo` and returns its trimmed stdout, failing on a non-zero exit.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .context("Failed to run git merge-base --is-ancestor")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git merge-base --is-ancestor {ancestor} {descendant} failed: {status}"),
    }
}

pub fn fetch_base(base_ref: &str) -> Result<()> {
    log::info!("Fetching base branch: {base_ref}");

    let status = Command::new("git")
        .args([
            "fetch",
            "origin",
            &format!("{base_ref}:refs/remotes/origin/{base_ref}"),
        ])
        .status()
        .context("Failed to run git fetch")?;

    if !status.success() {
        log::warn!("git fetch for {base_ref} returned non-zero exit code; continuing");
    }

    Ok(())
}

fn diff_names(repo: &Path, range: &DiffRange) -> Result<String> {
    git(repo, &["diff", "--name-only", &range.base, &range.head])
}

pub fn changed_files(range: &DiffRange) -> Result<Vec<String>> {
    let stdout = diff_names(Path::new("."), range)?;
    let files: Vec<String> = stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.contains(".github/"))
        .collect();

    Ok(files)
}

pub fn has_workflow_changes(range: &DiffRange) -> Result<bool> {
    let stdout = diff_names(Path::new("."), range)?;
    Ok(stdout.lines().any(|l| l.starts_with(".github/workflows/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A bare "origin" plus a clone of it, sharing a history that looks like
    /// `main: root -- m1`, `pr: root -- p1` (branched off before m1).
    struct Fixture {
        _dir: TempDir,
        origin: std::path::PathBuf,
        clone: std::path::PathBuf,
        root: String,
        main: String,
        pr_head: String,
    }

    fn run(repo: &Path, args: &[&str]) -> String {
        git(repo, args).unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn commit(repo: &Path, file: &str) -> String {
        fs::write(repo.join(file), file).unwrap();
        run(repo, &["add", file]);
        run(repo, &["commit", "--quiet", "-m", file]);
        run(repo, &["rev-parse", "HEAD"])
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        let clone = dir.path().join("clone");
        fs::create_dir_all(&work).unwrap();
        run(dir.path(), &["init", "--quiet", "--bare", "origin.git"]);
        run(&work, &["init", "--quiet", "-b", "main"]);
        run(&work, &["config", "user.email", "ci@example.com"]);
        run(&work, &["config", "user.name", "CI"]);
        run(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );

        let root = commit(&work, "root");
        run(&work, &["checkout", "--quiet", "-b", "pr"]);
        let pr_head = commit(&work, "pr-file");
        run(&work, &["checkout", "--quiet", "main"]);
        let main = commit(&work, "main-file");
        run(&work, &["push", "--quiet", "origin", "main", "pr"]);
        run(
            &work,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{pr_head}:refs/pull/1/head"),
            ],
        );

        run(
            dir.path(),
            &["clone", "--quiet", origin.to_str().unwrap(), "clone"],
        );
        run(&clone, &["checkout", "--quiet", &pr_head]);
        Fixture {
            _dir: dir,
            origin,
            clone,
            root,
            main,
            pr_head,
        }
    }

    /// Publishes `refs/pull/<n>/merge` on origin as a merge of `head` into `base`.
    fn publish_merge(f: &Fixture, n: u32, base: &str, head: &str) -> String {
        let tree = run(&f.origin, &["rev-parse", &format!("{head}^{{tree}}")]);
        let merge = run(
            &f.origin,
            &[
                "-c",
                "user.email=ci@example.com",
                "-c",
                "user.name=CI",
                "commit-tree",
                &tree,
                "-p",
                base,
                "-p",
                head,
                "-m",
                "merge",
            ],
        );
        run(
            &f.origin,
            &["update-ref", &format!("refs/pull/{n}/merge"), &merge],
        );
        merge
    }

    fn pr_merge(f: &Fixture) -> Option<DiffRange> {
        DiffRange::pr_merge_in(&f.clone, "main", &[Duration::ZERO]).unwrap()
    }

    #[test]
    fn merge_base_range() {
        let f = fixture();
        let range = DiffRange::merge_base_in(&f.clone, "main").unwrap();
        assert_eq!(
            range,
            DiffRange {
                base: f.root.clone(),
                head: f.pr_head.clone()
            }
        );
        assert_eq!(diff_names(&f.clone, &range).unwrap(), "pr-file");
    }

    #[test]
    fn pr_merge_range() {
        let f = fixture();
        let merge = publish_merge(&f, 1, &f.main, &f.pr_head);
        let range = pr_merge(&f).unwrap();
        assert_eq!(
            range,
            DiffRange {
                base: f.main.clone(),
                head: merge
            }
        );
    }

    #[test]
    fn pr_merge_missing() {
        let f = fixture();
        assert_eq!(pr_merge(&f), None);
    }

    #[test]
    fn pr_merge_stale() {
        let f = fixture();
        // A merge of an older PR head, as left by GitHub before it recomputes the ref.
        publish_merge(&f, 1, &f.main, &f.root);
        assert_eq!(pr_merge(&f), None);
    }

    #[test]
    fn pr_merge_other_target() {
        let f = fixture();
        // Merged into a commit that is not on origin/main.
        let elsewhere = publish_merge(&f, 2, &f.main, &f.root);
        publish_merge(&f, 1, &elsewhere, &f.pr_head);
        assert_eq!(pr_merge(&f), None);
    }

    #[test]
    fn pr_merge_skips_closed_pr_without_waiting() {
        let f = fixture();
        // #1 (from the fixture) shares the head but has no merge ref, like a closed PR; #2 is open.
        run(&f.origin, &["update-ref", "refs/pull/2/head", &f.pr_head]);
        let merge = publish_merge(&f, 2, &f.main, &f.pr_head);
        let start = std::time::Instant::now();
        let range = DiffRange::pr_merge_in(&f.clone, "main", &[Duration::from_secs(30)]).unwrap();
        assert_eq!(range.map(|r| r.head), Some(merge));
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "waited for the closed PR"
        );
    }

    #[test]
    fn pr_merge_picks_pr_into_target() {
        let f = fixture();
        // Two PRs share the head; only #3 merges into main.
        let elsewhere = publish_merge(&f, 9, &f.main, &f.root);
        publish_merge(&f, 1, &elsewhere, &f.pr_head);
        run(&f.origin, &["update-ref", "refs/pull/3/head", &f.pr_head]);
        let merge = publish_merge(&f, 3, &f.main, &f.pr_head);
        assert_eq!(pr_merge(&f).map(|r| r.head), Some(merge));
    }
}
