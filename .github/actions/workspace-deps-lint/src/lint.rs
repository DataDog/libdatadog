// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linter for how dependencies are declared across the libdatadog workspace. See the crate-level
//! documentation for the rules.

use anyhow::{Context, Result};
use std::fmt;
use toml_edit::{ImDocument, Item, Key, TableLike};

/// Marker that waives a rule for the dependency declared right below it:
/// `# allow(workspace-deps): <justification>`.
pub const ALLOW_MARKER: &str = "allow(workspace-deps)";

/// The dependency tables a manifest can declare, both at the top level and
/// under `[target.<cfg>]`.
const DEP_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// External dependencies of a member must be inherited from
    /// `[workspace.dependencies]` with `workspace = true`.
    Inherit,
    /// `[workspace.dependencies]` entries must not turn features on for every
    /// member: `default-features = false` and no `features`.
    Features,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Manifest path, as passed in (kept relative for readable reports).
    pub file: String,
    /// 1-based line of the offending dependency key.
    pub line: usize,
    pub dep: String,
    pub rule: Rule,
    /// What is wrong.
    pub problem: String,
    /// How to fix it.
    pub hint: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{}:{}: `{}` {}",
            self.file, self.line, self.dep, self.problem
        )?;
        write!(f, "    {}", self.hint)
    }
}

/// Outcome of looking for a justification above a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Allow {
    /// No comment at all above the dependency.
    Absent,
    /// A comment is there, but it is only an `allow(workspace-deps)` marker
    /// with an empty justification.
    MarkerWithoutJustification,
    /// A free-form comment (no marker) sits above the dependency.
    Comment,
    /// An `allow(workspace-deps): <justification>` marker with a justification.
    Justified,
}

/// Lint `[workspace.dependencies]` of a workspace root manifest (rule 2). Checks that no feature is
/// enabled unless there's a comment justification for it.
pub fn lint_workspace_dependencies(file: &str, src: &str) -> Result<Vec<Violation>> {
    let doc = ImDocument::parse(src).with_context(|| format!("failed to parse {file}"))?;
    let Some(deps) = doc
        .as_table()
        .get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Item::as_table_like)
    else {
        return Ok(Vec::new());
    };

    let violations = deps.iter().filter_map(|(name, _)| {
        let (key, item) = deps.get_key_value(name)?;

        // Path entries point at crates of this repository, not at external
        // dependencies, so the feature policy does not apply to them.
        if is_path_dependency(item) {
            return None;
        }

        let problem = feature_problem(item)?;
        let line = key_line(src, key);

        // Rule 4: a free-form comment above the entry is enough to justify
        // enabling a feature for every member.
        match allow_above(src, line) {
            Allow::Justified | Allow::Comment => None,
            Allow::MarkerWithoutJustification => Some(Violation {
                file: file.to_owned(),
                line,
                dep: name.to_owned(),
                rule: Rule::Features,
                problem,
                hint: format!("`{ALLOW_MARKER}` above it needs a justification: `# {ALLOW_MARKER}: <why every member gets this feature>`"),
            }),
            Allow::Absent => Some(Violation {
                file: file.to_owned(),
                line,
                dep: name.to_owned(),
                rule: Rule::Features,
                problem,
                hint: "fix: let each member pick its own features, or keep the feature and document why every member needs it in a comment directly above the entry".to_owned(),
            }),
        }
    }).collect();

    Ok(violations)
}

/// Lint the dependency tables of a workspace member manifest (rule 1). All dependencies must come
/// from the workspace, unless there's a explicit justification for it.
pub fn lint_member(file: &str, src: &str) -> Result<Vec<Violation>> {
    let doc = ImDocument::parse(src).with_context(|| format!("failed to parse {file}"))?;
    let root = doc.as_table();

    let mut tables: Vec<&dyn TableLike> = Vec::new();
    tables.extend(dependency_tables(root));
    // `[target.<cfg>.dependencies]` and friends.
    if let Some(targets) = root.get("target").and_then(Item::as_table_like) {
        tables.extend(
            targets
                .iter()
                .filter_map(|(_, target)| Some(dependency_tables(target.as_table_like()?)))
                .flatten(),
        );
    }

    let violations = tables
        .iter()
        .flat_map(|&table| table.iter().map(move |(name, _)| (table, name)))
        .filter_map(|(table, name)| {
            let (key, item) = table.get_key_value(name)?;

            if is_inherited(item) || is_path_dependency(item) {
                return None;
            }

            let line = key_line(src, key);
            // Rule 3: only the explicit marker waives this rule, so that
            // exceptions stay greppable and carry a justification.
            let hint = match allow_above(src, line) {
                Allow::Justified => return None,
                Allow::MarkerWithoutJustification => format!(
                    "`{ALLOW_MARKER}` above it needs a justification: `# {ALLOW_MARKER}: <why this crate cannot inherit>`"
                ),
                Allow::Absent | Allow::Comment => format!(
                    "fix: declare `{name}` in [workspace.dependencies] of the root Cargo.toml and use `{name} = {{ workspace = true, features = [..] }}` here, or document an exception with `# {ALLOW_MARKER}: <justification>` directly above it"
                ),
            };

            Some(Violation {
                file: file.to_owned(),
                line,
                dep: name.to_owned(),
                rule: Rule::Inherit,
                problem: "is not inherited from [workspace.dependencies] (`workspace = true`)"
                    .to_owned(),
                hint,
            })
        }).collect();

    Ok(violations)
}

fn dependency_tables(table: &dyn TableLike) -> impl Iterator<Item = &dyn TableLike> {
    DEP_TABLES
        .iter()
        .filter_map(move |name| table.get(name).and_then(Item::as_table_like))
}

fn is_inherited(item: &Item) -> bool {
    matches!(
        item.as_table_like()
            .and_then(|dep| dep.get("workspace"))
            .and_then(Item::as_bool),
        Some(true)
    )
}

fn is_path_dependency(item: &Item) -> bool {
    item.as_table_like()
        .is_some_and(|dep| dep.contains_key("path"))
}

/// Describe how a `[workspace.dependencies]` entry deviates from "version-only, `default-features =
/// false`", or `None` if it complies.
fn feature_problem(item: &Item) -> Option<String> {
    let Some(dep) = item.as_table_like() else {
        // `dep = "1.2"`: shorthand, so default features are on.
        return Some(
            "is declared as a bare version string, which leaves default features enabled for every member".to_owned(),
        );
    };

    let mut problems = Vec::new();

    let default_features = dep
        .get("default-features")
        .or_else(|| dep.get("default_features"))
        .and_then(Item::as_bool);

    if !matches!(default_features, Some(false)) {
        problems.push("is missing `default-features = false`".to_owned());
    }

    let features: Vec<&str> = dep
        .get("features")
        .and_then(Item::as_array)
        .map(|features| {
            features
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .collect()
        })
        .unwrap_or_default();

    if !features.is_empty() {
        problems.push(format!(
            "enables {} for every member",
            features
                .iter()
                .map(|feature| format!("`{feature}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    (!problems.is_empty()).then(|| problems.join(" and "))
}

/// 1-based line number of a key, falling back to line 1 when the document was built without spans.
fn key_line(src: &str, key: &Key) -> usize {
    key.span()
        .map_or(1, |span| src[..span.start].matches('\n').count() + 1)
}

/// Inspect the block of comment lines directly above `line`, trying to see if it's prefixed by a
/// leading [ALLOW_MARKER] at the beginning.
fn allow_above(src: &str, line: usize) -> Allow {
    let lines: Vec<&str> = src.lines().collect();

    let mut allow = Allow::Absent;
    let mut index = line.saturating_sub(1);

    while index > 0 {
        index -= 1;
        let Some(comment) = lines[index].trim().strip_prefix('#') else {
            break;
        };
        let comment = comment.trim();
        match comment.strip_prefix(ALLOW_MARKER) {
            Some(rest) => {
                let justification = rest.trim_start().strip_prefix(':').unwrap_or("").trim();
                if justification.is_empty() {
                    if allow == Allow::Absent {
                        allow = Allow::MarkerWithoutJustification;
                    }
                } else {
                    return Allow::Justified;
                }
            }
            None if !comment.is_empty() => allow = Allow::Comment,
            None => {}
        }
    }

    allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_violations(src: &str) -> Vec<Violation> {
        lint_workspace_dependencies("Cargo.toml", src).expect("manifest should parse")
    }

    fn member_violations(src: &str) -> Vec<Violation> {
        lint_member("crate/Cargo.toml", src).expect("manifest should parse")
    }

    fn deps(entries: &str) -> String {
        format!("[workspace.dependencies]\n{entries}")
    }

    #[test]
    fn workspace_entry_without_features_is_accepted() {
        let violations = workspace_violations(&deps(
            "anyhow = { version = \"1.0\", default-features = false }\n",
        ));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn workspace_entry_missing_default_features_is_reported() {
        let violations = workspace_violations(&deps("anyhow = { version = \"1.0\" }\n"));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].dep, "anyhow");
        assert_eq!(violations[0].rule, Rule::Features);
        assert_eq!(violations[0].line, 2);
        assert!(
            violations[0].problem.contains("default-features = false"),
            "{}",
            violations[0].problem
        );
    }

    #[test]
    fn workspace_entry_as_bare_version_is_reported() {
        let violations = workspace_violations(&deps("anyhow = \"1.0\"\n"));
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].problem.contains("bare version string"),
            "{}",
            violations[0].problem
        );
    }

    #[test]
    fn workspace_entry_enabling_features_is_reported() {
        let violations = workspace_violations(&deps(
            "serde = { version = \"1.0\", default-features = false, features = [\"derive\"] }\n",
        ));
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].problem.contains("`derive`"),
            "{}",
            violations[0].problem
        );
    }

    #[test]
    fn workspace_entry_with_empty_feature_list_is_accepted() {
        let violations = workspace_violations(&deps(
            "serde = { version = \"1.0\", default-features = false, features = [] }\n",
        ));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn workspace_entry_is_waived_by_a_plain_comment() {
        let violations = workspace_violations(&deps(
            "# Unusable without a compression backend.\nflate2 = { version = \"1.0\", default-features = false, features = [\"rust_backend\"] }\n",
        ));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn workspace_entry_is_waived_by_the_marker() {
        let violations = workspace_violations(&deps(
            "# allow(workspace-deps): every member needs `std`.\nlibc = { version = \"0.2\", default-features = true }\n",
        ));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn workspace_entry_comment_separated_by_a_blank_line_does_not_waive() {
        let violations = workspace_violations(&deps(
            "# Unrelated note.\n\nflate2 = { version = \"1.0\", features = [\"rust_backend\"] }\n",
        ));
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn workspace_entry_marker_without_justification_is_reported() {
        let violations = workspace_violations(&deps(
            "# allow(workspace-deps)\nlibc = { version = \"0.2\" }\n",
        ));
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].hint.contains("needs a justification"),
            "{}",
            violations[0].hint
        );
    }

    #[test]
    fn workspace_path_entry_is_ignored() {
        let violations = workspace_violations(&deps("internal = { path = \"../internal\" }\n"));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn inherited_member_dependencies_are_accepted() {
        let violations = member_violations(
            "[dependencies]\nanyhow.workspace = true\nserde = { workspace = true, features = [\"derive\"] }\ninternal = { path = \"../internal\" }\n",
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn member_dependency_with_its_own_version_is_reported() {
        let violations = member_violations("[dependencies]\nanyhow = \"1.0\"\n");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].dep, "anyhow");
        assert_eq!(violations[0].rule, Rule::Inherit);
        assert_eq!(violations[0].line, 2);
    }

    #[test]
    fn member_git_dependency_is_reported() {
        let violations =
            member_violations("[dependencies]\nfoo = { git = \"https://example.com/foo.git\" }\n");
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn member_dev_and_build_dependencies_are_linted() {
        let violations = member_violations(
            "[dev-dependencies]\nproptest = \"1\"\n\n[build-dependencies]\ncc = \"1\"\n",
        );
        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].dep, "proptest");
        assert_eq!(violations[1].dep, "cc");
    }

    #[test]
    fn member_target_dependencies_are_linted() {
        let violations = member_violations(
            "[target.'cfg(windows)'.dependencies]\nwinapi = { version = \"0.3.9\" }\n\n[target.'cfg(unix)'.dev-dependencies]\nnix = \"0.29\"\n",
        );
        assert_eq!(violations.len(), 2);
    }

    #[test]
    fn member_dependency_is_waived_by_the_marker() {
        let violations = member_violations(
            "[dependencies]\n# allow(workspace-deps): legacy API, incompatible with the 0.3 line.\nwinapi = { version = \"=0.2.8\" }\n",
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn member_dependency_is_not_waived_by_a_plain_comment() {
        let violations = member_violations(
            "[dependencies]\n# Needed for the legacy API.\nwinapi = { version = \"=0.2.8\" }\n",
        );
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].hint.contains(ALLOW_MARKER),
            "{}",
            violations[0].hint
        );
    }

    #[test]
    fn member_dependency_marker_without_justification_is_reported() {
        let violations = member_violations(
            "[dependencies]\n# allow(workspace-deps):\nwinapi = { version = \"=0.2.8\" }\n",
        );
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].hint.contains("needs a justification"),
            "{}",
            violations[0].hint
        );
    }

    #[test]
    fn marker_in_a_multi_line_comment_block_waives() {
        let violations = member_violations(
            "[dependencies]\n# allow(workspace-deps): pinned on purpose,\n# see the tracking issue.\nwinapi = { version = \"=0.2.8\" }\n",
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn manifest_without_dependencies_is_accepted() {
        assert!(member_violations("[package]\nname = \"foo\"\n").is_empty());
        assert!(workspace_violations("[workspace]\nmembers = []\n").is_empty());
    }

    #[test]
    fn invalid_manifest_is_an_error() {
        assert!(lint_member("crate/Cargo.toml", "[dependencies\n").is_err());
    }
}
