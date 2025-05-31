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
    let file_rc = Rc::new(file.to_string());

    for line in content.lines() {
        if let Some(captures) = regex.captures(line) {
            if let Some(rule_match) = captures.get(1) {
                let rule_str = rule_match.as_str().to_string();

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
                crate_cache.insert(file_path.to_string(), Rc::clone(&name));

                name
            }
        };

        *counts.entry(crate_name).or_insert(0) += 1;
    }

    counts
}

/// Create a regex for matching clippy allow annotations
pub(super) fn create_annotation_regex(rules: &[String]) -> Result<Regex> {
    let rule_pattern = rules.join("|");
    let regex = Regex::new(&format!(
        r"#\s*\[\s*allow\s*\(\s*clippy\s*::\s*({})\s*\)\s*\]",
        rule_pattern
    ))
    .context("Failed to compile annotation regex")?;

    Ok(regex)
}
