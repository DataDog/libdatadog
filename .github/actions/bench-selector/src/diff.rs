// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Parsing of `git diff -U0` output into per-file changed-line ranges on the
//! candidate (HEAD) side.
//!
//! Ranges are 1-based inclusive `[start, end]`. Pure deletions produce a
//! zero-length candidate range in git's hunk header (`+l,0`); they are mapped
//! to both adjacent candidate lines (`l` and `l + 1`, bounded below by 1).
//! Changed paths are retained even for deletions and metadata-only renames;
//! an empty range list means no candidate-side line information is available.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Inclusive, 1-based `[start, end]` line range.
pub type LineRange = (u32, u32);

pub type ChangedLines = BTreeMap<String, Vec<LineRange>>;

/// Run `git diff -U0 <base>...HEAD` and parse the candidate-side line ranges.
pub fn changed_ranges(base_ref: &str, workspace_root: &Path) -> Result<ChangedLines> {
    let spec = format!("origin/{base_ref}...HEAD");
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args([
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &spec,
        ])
        .output()
        .context("failed to run git diff")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff -U0 {spec} failed: {stderr}");
    }
    let stdout = std::str::from_utf8(&output.stdout).context("git diff is not UTF-8")?;
    let mut changed = parse_unified_diff(stdout)?;

    // NUL-delimited paths cover mode-only changes and both rename endpoints.
    // Without rename detection, the old/new names are deletion/addition paths.
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["diff", "--name-only", "-z", "--no-renames", &spec])
        .output()
        .context("failed to list changed paths")?;
    if !output.status.success() {
        bail!(
            "git diff --name-only failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).context("changed path is not UTF-8")?;
        changed.entry(path.to_owned()).or_default();
    }
    Ok(changed)
}

/// Parse the candidate-side line ranges out of `git diff -U0` output.
pub fn parse_unified_diff(input: &str) -> Result<ChangedLines> {
    let mut changed: ChangedLines = BTreeMap::new();
    let mut current_file: Option<String> = None;
    let mut in_hunk = false;

    for line in input.lines() {
        if line.starts_with("diff --git") {
            current_file = None;
            in_hunk = false;
            continue;
        }
        if !in_hunk
            && let Some(path) = line
                .strip_prefix("rename from ")
                .or_else(|| line.strip_prefix("rename to "))
        {
            changed.entry(decode_git_path(path)?).or_default();
            continue;
        }
        if !in_hunk && let Some(path) = line.strip_prefix("--- ") {
            if path != "/dev/null" {
                // Git appends a tab separator to unquoted paths with spaces.
                let path = decode_git_path(path.strip_suffix('\t').unwrap_or(path))?;
                let path = path
                    .strip_prefix("a/")
                    .context("unexpected old-side path prefix")?;
                changed.entry(path.to_owned()).or_default();
            }
            continue;
        }
        if !in_hunk && let Some(path) = line.strip_prefix("+++ ") {
            current_file = if path == "/dev/null" {
                None
            } else {
                let path = decode_git_path(path.strip_suffix('\t').unwrap_or(path))?;
                let path = path
                    .strip_prefix("b/")
                    .context("unexpected candidate-side path prefix")?;
                changed.entry(path.to_owned()).or_default();
                Some(path.to_owned())
            };
            continue;
        }
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        in_hunk = true;
        // Deleted files have no candidate side; their old paths remain in the map.
        let Some(file) = current_file.as_ref() else {
            continue;
        };
        let (spec, _) = rest.split_once(" @@").context("invalid git hunk header")?;
        let plus_part = spec
            .split_whitespace()
            .find(|p| p.starts_with('+'))
            .context("missing candidate-side hunk range")?;
        let (start, count) = parse_hunk_side(&plus_part[1..])?;
        changed
            .entry(file.clone())
            .or_default()
            .push(candidate_range(start, count)?);
    }

    Ok(changed)
}

/// Parse one side of a hunk header like `12` or `12,7` into `(start, count)`.
/// A missing count means 1, following the unified-diff convention.
fn parse_hunk_side(spec: &str) -> Result<(u32, u32)> {
    let (start, count) = spec.split_once(',').unwrap_or((spec, "1"));
    Ok((
        start.parse().context("invalid hunk start")?,
        count.parse().context("invalid hunk count")?,
    ))
}

/// `+l,0` anchors before the deletion, so include its successor as well.
fn candidate_range(start: u32, count: u32) -> Result<LineRange> {
    if count == 0 {
        return Ok((start.max(1), start.saturating_add(1).max(1)));
    }
    anyhow::ensure!(start > 0, "nonempty hunk starts at line zero");
    let end = start
        .checked_add(count - 1)
        .context("hunk range overflow")?;
    Ok((start, end))
}

/// Decode Git's C-style quoted pathnames, including octal UTF-8 bytes.
fn decode_git_path(path: &str) -> Result<String> {
    if !path.starts_with('"') {
        return Ok(path.to_owned());
    }
    let quoted = path
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .context("unterminated quoted git path")?;
    let mut bytes = quoted.bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let escaped = bytes.next().context("truncated git path escape")?;
        let byte = match escaped {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            b'"' => b'"',
            b'\\' => b'\\',
            b'0'..=b'3' => {
                let second = bytes.next().context("truncated octal path escape")?;
                let third = bytes.next().context("truncated octal path escape")?;
                anyhow::ensure!(
                    matches!(second, b'0'..=b'7') && matches!(third, b'0'..=b'7'),
                    "invalid octal path escape"
                );
                (escaped - b'0') * 64 + (second - b'0') * 8 + (third - b'0')
            }
            _ => bail!("unsupported git path escape"),
        };
        decoded.push(byte);
    }
    String::from_utf8(decoded).context("git path is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_added_and_modified_lines() {
        let diff = "\
diff --git a/libdd-x/src/lib.rs b/libdd-x/src/lib.rs
--- a/libdd-x/src/lib.rs
+++ b/libdd-x/src/lib.rs
@@ -10,2 +10,3 @@
 old
+new
 old
@@ -40 +41 @@
-x
+y
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(
            changed.get("libdd-x/src/lib.rs").unwrap(),
            &[(10, 12), (41, 41)]
        );
    }

    #[test]
    fn pure_deletions_map_to_both_adjacent_candidate_lines() {
        let diff = "\
diff --git a/libdd-x/src/a.rs b/libdd-x/src/a.rs
--- a/libdd-x/src/a.rs
+++ b/libdd-x/src/a.rs
@@ -7,3 +6,0 @@
 a
-b
-c
 a
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(changed.get("libdd-x/src/a.rs").unwrap(), &[(6, 7)]);
    }

    #[test]
    fn new_files_contribute_their_lines() {
        let diff = "\
diff --git a/libdd-x/src/new.rs b/libdd-x/src/new.rs
new file mode 100644
--- /dev/null
+++ b/libdd-x/src/new.rs
@@ -0,0 +1,2 @@
+fn a() {}
+fn b() {}
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(changed.get("libdd-x/src/new.rs").unwrap(), &[(1, 2)]);
    }

    #[test]
    fn deleted_files_retain_the_old_path_without_candidate_ranges() {
        let diff = "\
diff --git a/libdd-x/src/gone.rs b/libdd-x/src/gone.rs
deleted file mode 100644
--- a/libdd-x/src/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(changed.get("libdd-x/src/gone.rs"), Some(&vec![]));
    }

    #[test]
    fn multiple_files_stay_separate() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-x
+y
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -5,2 +5,2 @@
-x
+y
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(changed.get("a.rs").unwrap(), &[(1, 1)]);
        assert_eq!(changed.get("b.rs").unwrap(), &[(5, 6)]);
    }

    #[test]
    fn single_line_hunk_without_count() {
        let diff = "\
--- a/a.rs
+++ b/a.rs
@@ -3 +3,2 @@
 old
+new
";
        let changed = parse_unified_diff(diff).expect("valid diff");
        assert_eq!(changed.get("a.rs").unwrap(), &[(3, 4)]);
    }

    #[test]
    fn beginning_of_file_deletion_keeps_line_one() {
        let changed =
            parse_unified_diff("--- a/a.rs\n+++ b/a.rs\n@@ -1 +0,0 @@\n-#[inline(never)]\n")
                .unwrap();
        assert_eq!(changed["a.rs"], [(1, 1)]);
    }

    #[test]
    fn git_quoted_utf8_and_control_characters_are_decoded() {
        let changed = parse_unified_diff(
            "--- \"a/caf\\303\\251.rs\"\n+++ \"b/caf\\303\\251.rs\"\n@@ -1 +1 @@\n-x\n+y\n",
        )
        .unwrap();
        assert_eq!(changed["café.rs"], [(1, 1)]);
        assert_eq!(
            decode_git_path(r#""a\tb\nc\\d\"e""#).unwrap(),
            "a\tb\nc\\d\"e"
        );
        assert!(decode_git_path(r#""a\q""#).is_err());
        assert!(decode_git_path(r#""a\377""#).is_err());
    }

    #[test]
    fn unquoted_space_paths_drop_only_gits_header_separator() {
        let changed = parse_unified_diff(
            "--- a/space name.rs\t\n+++ b/space name.rs\t\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        assert_eq!(changed["space name.rs"], [(1, 1)]);
    }

    #[test]
    fn hunk_body_is_not_mistaken_for_path_headers() {
        let changed = parse_unified_diff("--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n--- old contents\n+++ new contents\n@@ -3 +3 @@\n-x\n+y\n").unwrap();
        assert_eq!(changed["a.rs"], [(1, 1), (3, 3)]);
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn metadata_only_rename_retains_both_paths() {
        let changed = parse_unified_diff("diff --git a/x/benches/old.rs b/y/new.rs\nsimilarity index 100%\nrename from x/benches/old.rs\nrename to y/new.rs\n").unwrap();
        assert_eq!(changed.len(), 2);
        assert!(changed["x/benches/old.rs"].is_empty());
        assert!(changed["y/new.rs"].is_empty());
    }

    #[test]
    fn invalid_hunk_evidence_returns_an_error() {
        assert!(parse_unified_diff("+++ b/a.rs\n@@ -1 +bad @@\n").is_err());
        assert!(candidate_range(0, 1).is_err());
        assert!(candidate_range(u32::MAX, 2).is_err());
    }
}
