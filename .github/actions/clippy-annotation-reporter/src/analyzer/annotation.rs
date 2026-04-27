// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Functions for finding and parsing clippy annotations

use crate::analyzer::crate_detection::get_crate_for_file;
use crate::analyzer::ClippyAnnotation;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::rc::Rc;

/// Find clippy annotations in file content
pub(super) fn find_annotations(
    annotations: &mut Vec<ClippyAnnotation>,
    file: &str,
    content: &str,
    regex: &Regex,
    rule_cache: &mut HashMap<String, Rc<String>>,
) {
    // Use Rc for file path
    let file_rc = Rc::new(file.to_owned());

    for line in content.lines() {
        if let Some(captures) = regex.captures(line) {
            if let Some(rule_match) = captures.get(1) {
                let rule_str = rule_match.as_str().to_owned();

                // Get or create Rc for this rule
                let rule_rc = match rule_cache.get(&rule_str) {
                    Some(cached) => Rc::clone(cached),
                    None => {
                        let rc = Rc::new(rule_str.clone());
                        rule_cache.insert(rule_str, Rc::clone(&rc));
                        rc
                    }
                };

                annotations.push(ClippyAnnotation {
                    file: Rc::clone(&file_rc),
                    rule: rule_rc,
                });
            }
        }
    }
}

/// Count annotations by rule
pub(super) fn count_annotations_by_rule(
    annotations: &[ClippyAnnotation],
) -> HashMap<Rc<String>, usize> {
    let mut counts = HashMap::with_capacity(annotations.len().min(20));

    for annotation in annotations {
        *counts.entry(Rc::clone(&annotation.rule)).or_insert(0) += 1;
    }

    counts
}

/// Count annotations by crate
pub(super) fn count_annotations_by_crate(
    annotations: &[ClippyAnnotation],
) -> HashMap<Rc<String>, usize> {
    let mut counts = HashMap::new();
    let mut crate_cache: HashMap<String, Rc<String>> = HashMap::new();

    for annotation in annotations {
        let file_path = annotation.file.as_str();

        // Use cached crate name if we've seen this file before
        let crate_name = match crate_cache.get(file_path) {
            Some(name) => name.clone(),
            None => {
                let name = Rc::new(get_crate_for_file(file_path).to_owned());
                crate_cache.insert(file_path.to_owned(), Rc::clone(&name));

                name
            }
        };

        *counts.entry(crate_name).or_insert(0) += 1;
    }

    counts
}

/// Create a regex for matching clippy allow annotations
pub(super) fn create_annotation_regex(rules: &[String]) -> Result<Regex> {
    if rules.is_empty() {
        return Err(anyhow::anyhow!("Cannot create regex with empty rules list"));
    }

    let rule_pattern = rules.join("|");
    let regex = Regex::new(&format!(
        r"#\s*\[\s*allow\s*\(\s*clippy\s*::\s*({})\s*\)\s*\]",
        rule_pattern
    ))
    .context("Failed to compile annotation regex")?;

    Ok(regex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn test_count_annotations_by_rule() {
        // Create test annotations
        let rule1 = Rc::new("clippy::unwrap_used".to_owned());
        let rule2 = Rc::new("clippy::match_bool".to_owned());
        let file = Rc::new("src/main.rs".to_owned());

        let annotations = vec![
            ClippyAnnotation {
                file: file.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file.clone(),
                rule: rule1.clone(),
            },
            ClippyAnnotation {
                file: file.clone(),
                rule: rule2.clone(),
            },
            ClippyAnnotation {
                file: file.clone(),
                rule: rule1.clone(),
            },
        ];

        let counts = count_annotations_by_rule(&annotations);

        assert_eq!(counts.len(), 2, "Should have counts for 2 rules");
        assert_eq!(counts[&rule1], 3, "Rule1 should have 3 annotations");
        assert_eq!(counts[&rule2], 1, "Rule2 should have 1 annotation");
    }

    #[test]
    fn test_count_annotations_by_rule_empty() {
        // Test with empty annotations
        let annotations: Vec<ClippyAnnotation> = vec![];
        let counts = count_annotations_by_rule(&annotations);

        assert_eq!(
            counts.len(),
            0,
            "Empty annotations should produce empty counts"
        );
    }

    #[test]
    fn test_count_annotations_by_crate() {
        use std::fs::{self, File};
        use std::io::Write;
        use tempfile::TempDir;

        // Create a temporary directory structure for testing
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let temp_path = temp_dir.path();

        // Create a directory structure with two different crates
        // crate1
        // ├── Cargo.toml (package.name = "crate1")
        // └── src
        //     ├── lib.rs
        //     └── module.rs
        // crate2
        // ├── Cargo.toml (package.name = "crate2")
        // └── src
        //     └── main.rs

        // Create directories
        let crate1_dir = temp_path.join("crate1");
        let crate1_src_dir = crate1_dir.join("src");
        let crate2_dir = temp_path.join("crate2");
        let crate2_src_dir = crate2_dir.join("src");

        fs::create_dir_all(&crate1_src_dir).expect("Failed to create crate1/src directory");
        fs::create_dir_all(&crate2_src_dir).expect("Failed to create crate2/src directory");

        // Create Cargo.toml files with specific package names
        let crate1_cargo = crate1_dir.join("Cargo.toml");
        let mut cargo1_file =
            File::create(&crate1_cargo).expect("Failed to create crate1 Cargo.toml");
        writeln!(
            cargo1_file,
            r#"[package]
name = "crate1"
version = "0.1.0"
edition = "2021"
"#
        )
        .expect("Failed to write to crate1 Cargo.toml");

        let crate2_cargo = crate2_dir.join("Cargo.toml");
        let mut cargo2_file =
            File::create(&crate2_cargo).expect("Failed to create crate2 Cargo.toml");
        writeln!(
            cargo2_file,
            r#"[package]
name = "crate2"
version = "0.1.0"
edition = "2021"
"#
        )
        .expect("Failed to write to crate2 Cargo.toml");

        // Create source files
        let crate1_lib = crate1_src_dir.join("lib.rs");
        let mut lib_file = File::create(&crate1_lib).expect("Failed to create lib.rs");
        writeln!(lib_file, "// Empty lib file").expect("Failed to write to lib.rs");

        let crate1_module = crate1_src_dir.join("module.rs");
        let mut module_file = File::create(&crate1_module).expect("Failed to create module.rs");
        writeln!(module_file, "// Empty module file").expect("Failed to write to module.rs");

        let crate2_main = crate2_src_dir.join("main.rs");
        let mut main_file = File::create(&crate2_main).expect("Failed to create main.rs");
        writeln!(main_file, "// Empty main file").expect("Failed to write to main.rs");

        // Create test annotations with the real file paths
        let rule = Rc::new("clippy::unwrap_used".to_owned());

        let crate1_lib_path = Rc::new(crate1_lib.to_string_lossy().to_string());
        let crate1_module_path = Rc::new(crate1_module.to_string_lossy().to_string());
        let crate2_main_path = Rc::new(crate2_main.to_string_lossy().to_string());

        let annotations = vec![
            ClippyAnnotation {
                file: crate1_lib_path.clone(),
                rule: rule.clone(),
            },
            ClippyAnnotation {
                file: crate1_module_path.clone(),
                rule: rule.clone(),
            },
            ClippyAnnotation {
                file: crate1_module_path.clone(), // Another annotation in the same file
                rule: rule.clone(),
            },
            ClippyAnnotation {
                file: crate2_main_path.clone(),
                rule: rule.clone(),
            },
        ];

        let counts = count_annotations_by_crate(&annotations);

        assert_eq!(counts.len(), 2, "Should have counts for 2 crates");

        let crate1_count = counts
            .iter()
            .find(|(k, _)| k.contains("crate1"))
            .map(|(_, v)| *v)
            .unwrap_or(0);

        let crate2_count = counts
            .iter()
            .find(|(k, _)| k.contains("crate2"))
            .map(|(_, v)| *v)
            .unwrap_or(0);

        assert_eq!(crate1_count, 3, "crate1 should have 3 annotations");
        assert_eq!(crate2_count, 1, "crate2 should have 1 annotation");
    }

    #[test]
    fn test_count_annotations_by_crate_empty() {
        // Test with empty annotations
        let annotations: Vec<ClippyAnnotation> = vec![];
        let counts = count_annotations_by_crate(&annotations);

        assert_eq!(
            counts.len(),
            0,
            "Empty annotations should produce empty counts"
        );
    }

    #[test]
    fn test_create_annotation_regex_single_rule() {
        let rules = vec!["unwrap_used".to_owned()]; // Rule without clippy:: prefix
        let regex = create_annotation_regex(&rules).expect("Failed to create regex");

        // Test matching
        assert!(regex.is_match("#[allow(clippy::unwrap_used)]"));
        assert!(regex.is_match("#[allow(clippy:: unwrap_used )]")); // With spaces
        assert!(regex.is_match("#  [ allow ( clippy :: unwrap_used ) ]")); // With more spaces

        // Test non-matching
        assert!(!regex.is_match("#[allow(clippy::unused_imports)]"));
        assert!(!regex.is_match("#[allow(unwrap_used)]")); // Missing clippy::
        assert!(!regex.is_match("clippy::unwrap_used")); // Missing #[allow()]
    }
    #[test]
    fn test_create_annotation_regex_multiple_rules() {
        let rules = vec!["unwrap_used".to_owned(), "match_bool".to_owned()];
        let regex = create_annotation_regex(&rules).expect("Failed to create regex");

        assert!(regex.is_match("#[allow(clippy::unwrap_used)]"));
        assert!(regex.is_match("#[allow(clippy::match_bool)]"));

        // Test mixed spacing and formatting
        assert!(regex.is_match("#[allow(clippy:: unwrap_used )]")); // With spaces
        assert!(regex.is_match("#  [ allow ( clippy :: match_bool ) ]")); // With more spaces

        // Test non-matching
        assert!(!regex.is_match("#[allow(clippy::unused_imports)]"));
        assert!(!regex.is_match("#[allow(unwrap_used)]")); // Missing clippy::
    }

    #[test]
    fn test_create_annotation_regex_empty_rules() {
        let rules: Vec<String> = vec![];
        let result = create_annotation_regex(&rules);

        assert!(
            result.is_err(),
            "Creating regex with empty rules should fail"
        );
    }
}
