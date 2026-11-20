// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Gitignore-subset matcher for GitHub CODEOWNERS patterns.
//!
//! CODEOWNERS follows gitignore pattern rules with a few omissions (no
//! negation, no character ranges). This module implements the subset used in
//! this repository:
//!
//! - a pattern containing a `/` anywhere except a trailing one is anchored to the repository root;
//!   a pattern without any `/` matches at any depth
//! - a trailing `/` marks a directory pattern: it matches everything inside the matched directory,
//!   not the directory path itself
//! - a rooted literal path naming a directory matches everything beneath it (gitignore
//!   inheritance); unlike gitignore, a component glob does not extend into subdirectories: GitHub
//!   documents that `docs/*` does not match `docs/build-app/troubleshooting.md`
//! - `*` and `?` are wildcards within a single path component
//!
//! Owner validity (handles exist and have write access) is GitHub's own
//! validation and is enforced in CI through the codeowners/errors API; it
//! cannot be checked locally.

use anyhow::{Result, ensure};

/// One CODEOWNERS rule: a pattern and its owners.
pub struct Rule {
    pub line: usize,
    pub pattern: String,
    pub owners: Vec<String>,
}

/// Parse a CODEOWNERS file. Blank lines and comments are skipped; a pattern
/// without owners is an error (GitHub cannot resolve it to a reviewer).
pub fn parse(text: &str) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut toks = line.split_whitespace();
        let pattern = toks.next().unwrap_or_default().to_string();
        let owners: Vec<String> = toks.map(str::to_owned).collect();
        ensure!(
            !owners.is_empty(),
            "line {}: {pattern:?} has no owner",
            i + 1
        );
        rules.push(Rule {
            line: i + 1,
            pattern,
            owners,
        });
    }
    Ok(rules)
}

/// Shell-style glob with `*` and `?`, matching within one path component.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut star_t) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            star_t = ti;
            pi += 1;
        } else if star != usize::MAX {
            // backtrack: let the last `*` swallow one more character
            pi = star + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether a CODEOWNERS `pattern` matches a repository-relative `path`.
pub fn matches(pattern: &str, path: &str) -> bool {
    let leading = pattern.starts_with('/');
    let core = pattern.trim_start_matches('/').trim_end_matches('/');
    let trailing = pattern.ends_with('/');
    // gitignore: a slash at the start or in the middle anchors the pattern to
    // the repository root; a slash only at the end does not
    let rooted = leading || core.contains('/');
    let comps: Vec<&str> = if core.is_empty() {
        Vec::new()
    } else {
        core.split('/').collect()
    };
    let pcs: Vec<&str> = path.split('/').collect();

    if trailing {
        // directory pattern: the path must live strictly inside a matched dir
        let n = comps.len();
        if rooted {
            return pcs.len() > n && (0..n).all(|i| glob_match(comps[i], pcs[i]));
        }
        return (0..pcs.len())
            .any(|i| i + n < pcs.len() && (0..n).all(|j| glob_match(comps[j], pcs[i + j])));
    }
    if rooted {
        // exact component-wise match; a literal prefix naming a directory also
        // matches everything beneath it (component globs do not extend)
        let m = comps.len();
        if pcs.len() == m && (0..m).all(|i| glob_match(comps[i], pcs[i])) {
            return true;
        }
        let literal = comps.iter().all(|c| !c.contains(['*', '?']));
        return literal && pcs.len() > m && (0..m).all(|i| comps[i] == pcs[i]);
    }
    // single component, matches at any depth (file or dir, contents inherit)
    pcs.iter().any(|c| glob_match(comps[0], c))
}

/// The rule owning `path`: the last matching rule wins, as on GitHub.
pub fn resolve<'r>(path: &str, rules: &'r [Rule]) -> Option<&'r Rule> {
    rules.iter().rev().find(|r| matches(&r.pattern, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_single_component() {
        assert!(glob_match("libdd-profiling*", "libdd-profiling"));
        assert!(glob_match("libdd-profiling*", "libdd-profiling-ffi"));
        assert!(!glob_match("libdd-profiling*", "libdd-alloc"));
        assert!(glob_match("Cargo.*", "Cargo.toml"));
        assert!(glob_match("Cargo.*", "Cargo.lock"));
        assert!(glob_match("f?le", "file"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("file", "files"));
    }

    #[test]
    fn anchored_directory_patterns() {
        assert!(matches("/libdd-profiling*/", "libdd-profiling/src/x.rs"));
        assert!(matches(
            "/libdd-profiling*/",
            "libdd-profiling-ffi/src/x.rs"
        ));
        assert!(!matches("/libdd-profiling*/", "libdd-alloc/src/x.rs"));
        assert!(!matches("/tests/", "tests")); // the directory path itself is not a file inside
        assert!(matches(
            "fuzz/",
            "libdd-trace-normalization/fuzz/Cargo.toml"
        ));
        // an internal slash anchors to the root like a leading one
        assert!(matches("tests/wasm/", "tests/wasm/foo.rs"));
        assert!(!matches("tests/wasm/", "archive/tests/wasm/foo.rs"));
    }

    #[test]
    fn unanchored_directory_patterns() {
        assert!(matches("tests/", "tests/x.rs"));
        assert!(matches("tests/", "libdd-profiling/tests/x.rs"));
        assert!(!matches("tests/", "testsuite/x.rs"));
        assert!(matches("benches/", "libdd-ddsketch/benches/b.rs"));
    }

    #[test]
    fn file_patterns() {
        assert!(matches("/README.md", "README.md"));
        assert!(!matches("/README.md", "docs/README.md"));
        assert!(matches("Cargo.toml", "Cargo.toml"));
        assert!(matches("Cargo.toml", "libdd-alloc/Cargo.toml"));
        assert!(matches("LICENSE*", "LICENSE"));
        assert!(matches("LICENSE*", "libdd-profiling/LICENSE.txt"));
        assert!(matches("NOTICE", "libdd-profiling/NOTICE"));
        assert!(matches("ruby/README.md", "ruby/README.md"));
        assert!(!matches("ruby/README.md", "README.md"));
    }

    #[test]
    fn rooted_literal_prefix_inherits_glob_does_not() {
        // gitignore: a pattern matching a directory owns its subtree
        assert!(matches("/docs", "docs/a/b.md"));
        assert!(matches("/examples", "examples/ffi/ffe.c"));
        assert!(!matches("/docs", "documentation/a.md"));
        // a component glob only matches at its own depth (GitHub documents
        // that docs/* does not match docs/build-app/troubleshooting.md)
        assert!(matches(".cargo/*", ".cargo/config.toml"));
        assert!(!matches(".cargo/*", ".cargo/x/y.rs"));
        assert!(!matches(".cargo/*", "other/config.toml"));
    }

    #[test]
    fn resolve_last_match_wins() {
        let rules = parse("*.md @a\n/docs/ @b\n").expect("parses");
        assert_eq!(resolve("docs/x.md", &rules).unwrap().owners, ["@b"]);
        assert_eq!(resolve("x.md", &rules).unwrap().owners, ["@a"]);
        assert!(resolve("x.rs", &rules).is_none());
    }

    #[test]
    fn parse_rejects_ownerless_pattern() {
        assert!(parse("# ok\n\n/foo\n").is_err());
        assert!(parse("/foo @a\n").is_ok());
    }
}
