// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Diff parsing and source inspection for `ffi-panic-lint`.

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

const ALLOW_MARKER: &str = "allow(ffi-panic-boundary)";
const CONTAINMENT_MARKERS: [&str; 5] = [
    "catch_unwind",
    "wrap_with_ffi_result!",
    "wrap_with_void_ffi_result!",
    "wrap_with_ffi_result_no_catch!",
    "wrap_with_void_ffi_result_no_catch!",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Source path relative to the repository root.
    pub file: String,
    /// 1-based line where the function signature starts.
    pub line: usize,
    pub function: String,
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
            self.file, self.line, self.function, self.problem
        )?;
        write!(f, "    {}", self.hint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub added_lines: BTreeSet<usize>,
}

impl ChangedFile {
    pub fn lint(&self, root: &Path) -> Result<Vec<Violation>> {
        let path = root.join(&self.path);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        lint_source(&self.path, &source, &self.added_lines)
    }
}

/// Outcome of looking for an escape-hatch comment above a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allow {
    /// No comment appears in either of the two preceding lines.
    Absent,
    /// The marker is present, but its justification is empty.
    MarkerWithoutJustification,
    /// A free-form comment without the marker appears above the function.
    Comment,
    /// The marker carries a non-empty justification.
    Justified,
}

/// Parse a zero-context unified diff into current-file paths and added line numbers.
pub fn parse_diff(diff: &str) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();
    let mut current_file = None;
    let mut current_new_line = None;

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            current_new_line = None;
            if path == "/dev/null" {
                current_file = None;
                continue;
            }

            let path = path.strip_prefix("b/").unwrap_or(path).to_owned();
            files.push(ChangedFile {
                path,
                added_lines: BTreeSet::new(),
            });
            current_file = Some(files.len() - 1);
            continue;
        }

        if line.starts_with("@@ ") {
            current_new_line = Some(parse_hunk_new_start(line)?);
            continue;
        }

        let (Some(file_index), Some(new_line)) = (current_file, current_new_line) else {
            continue;
        };

        if line.starts_with('+') {
            files[file_index].added_lines.insert(new_line);
            current_new_line = Some(new_line + 1);
        } else if !line.starts_with('-') && !line.starts_with('\\') {
            current_new_line = Some(new_line + 1);
        }
    }

    Ok(files)
}

fn parse_hunk_new_start(header: &str) -> Result<usize> {
    let start = header
        .find(" +")
        .map(|index| index + 2)
        .with_context(|| format!("invalid diff hunk header: {header}"))?;
    let rest = &header[start..];
    let end = rest
        .find([',', ' '])
        .with_context(|| format!("invalid diff hunk header: {header}"))?;
    rest[..end]
        .parse()
        .with_context(|| format!("invalid new-file line in diff hunk header: {header}"))
}

fn lint_source(file: &str, source: &str, added_lines: &BTreeSet<usize>) -> Result<Vec<Violation>> {
    let signature =
        Regex::new(r#"\bpub\s+(?:unsafe\s+)?extern\s*"C"\s+fn\s+((?:r#)?[A-Za-z_][A-Za-z0-9_]*)"#)
            .context("failed to compile FFI signature matcher")?;
    let code = code_positions(source);
    let lines: Vec<&str> = source.lines().collect();
    let mut violations = Vec::new();

    for signature_match in signature.captures_iter(source) {
        let full_match = signature_match
            .get(0)
            .context("signature matcher omitted its full match")?;
        if !code.get(full_match.start()).copied().unwrap_or(false) {
            continue;
        }

        let start_line = line_number(source, full_match.start());
        let end_line = line_number(source, full_match.end().saturating_sub(1));
        if !added_lines.range(start_line..=end_line).next().is_some() {
            continue;
        }

        let function = signature_match
            .get(1)
            .context("signature matcher omitted the function name")?
            .as_str()
            .to_owned();
        let (body_start, body_end) =
            function_body(source, &code, full_match.end()).with_context(|| {
                format!("failed to locate body of {function} in {file}:{start_line}")
            })?;

        if contains_containment_marker(source, &code, body_start, body_end)
            || allow_above(&lines, start_line) == Allow::Justified
        {
            continue;
        }

        let allow = allow_above(&lines, start_line);
        let (problem, hint) = match allow {
            Allow::MarkerWithoutJustification => (
                "has an ffi-panic-boundary escape hatch without a justification".to_owned(),
                format!(
                    "`// {ALLOW_MARKER}: <justification>` above it needs a non-empty justification; otherwise use `wrap_with_ffi_result!`, `wrap_with_void_ffi_result!`, or `std::panic::catch_unwind` as described in AGENTS.md's \"Reliability & integrability\" section"
                ),
            ),
            Allow::Absent | Allow::Comment => (
                "is a new extern \"C\" function without panic containment".to_owned(),
                format!(
                    "use `wrap_with_ffi_result!`, `wrap_with_void_ffi_result!`, or `std::panic::catch_unwind` per AGENTS.md's \"Reliability & integrability\" section; if containment is redundant, add `// {ALLOW_MARKER}: <justification>` directly above the function"
                ),
            ),
            Allow::Justified => continue,
        };

        violations.push(Violation {
            file: file.to_owned(),
            line: start_line,
            function,
            problem,
            hint,
        });
    }

    Ok(violations)
}

fn line_number(source: &str, byte: usize) -> usize {
    source[..byte].bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn function_body(source: &str, code: &[bool], after_signature: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let body_start = (after_signature..bytes.len()).find(|&index| {
        if !code[index] {
            return false;
        }
        if bytes[index] == b';' {
            return true;
        }
        bytes[index] == b'{'
    })?;
    if bytes[body_start] == b';' {
        return None;
    }

    let mut depth = 0usize;
    for index in body_start..bytes.len() {
        if !code[index] {
            continue;
        }
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((body_start, index + 1));
                }
            }
            _ => {}
        }
    }

    None
}

fn contains_containment_marker(source: &str, code: &[bool], start: usize, end: usize) -> bool {
    CONTAINMENT_MARKERS.iter().any(|marker| {
        source[start..end]
            .match_indices(marker)
            .any(|(offset, _)| code.get(start + offset).copied().unwrap_or(false))
    })
}

fn allow_above(lines: &[&str], signature_line: usize) -> Allow {
    let end = signature_line.saturating_sub(1);
    let start = end.saturating_sub(2);
    let mut allow = Allow::Absent;

    for line in &lines[start..end] {
        let Some(comment) = line.trim().strip_prefix("//") else {
            continue;
        };
        let comment = comment.trim();
        match comment.strip_prefix(ALLOW_MARKER) {
            Some(rest) => {
                let justification = rest.trim_start().strip_prefix(':').unwrap_or("").trim();
                if justification.is_empty() {
                    allow = Allow::MarkerWithoutJustification;
                } else {
                    return Allow::Justified;
                }
            }
            None if !comment.is_empty() && allow == Allow::Absent => allow = Allow::Comment,
            None => {}
        }
    }

    allow
}

/// Mark bytes that are Rust code rather than comments or string/character literals. This keeps
/// braces and containment-marker names inside comments and literals from affecting the lint.
fn code_positions(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut code = vec![true; bytes.len()];
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| index + offset);
            code[index..end].fill(false);
            index = end;
        } else if bytes[index..].starts_with(b"/*") {
            let mut end = index + 2;
            let mut depth = 1usize;
            while end < bytes.len() && depth > 0 {
                if bytes[end..].starts_with(b"/*") {
                    depth += 1;
                    end += 2;
                } else if bytes[end..].starts_with(b"*/") {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            code[index..end].fill(false);
            index = end;
        } else if let Some(end) = raw_string_end(bytes, index) {
            code[index..end].fill(false);
            index = end;
        } else if bytes[index] == b'"' {
            let end = quoted_end(bytes, index, b'"').unwrap_or(bytes.len());
            code[index..end].fill(false);
            index = end;
        } else if bytes[index] == b'\'' {
            if let Some(end) = char_literal_end(source, index) {
                code[index..end].fill(false);
                index = end;
            } else {
                index += 1;
            }
        } else {
            index += 1;
        }
    }

    code
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        return None;
    }

    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;

    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hash_count = cursor - hashes_start;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hash_count)
                .is_some_and(|hashes| hashes.iter().all(|byte| *byte == b'#'))
        {
            return Some(cursor + 1 + hash_count);
        }
        cursor += 1;
    }

    Some(bytes.len())
}

fn quoted_end(bytes: &[u8], start: usize, delimiter: u8) -> Option<usize> {
    let mut cursor = start + 1;
    let mut escaped = false;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == delimiter {
            return Some(cursor + 1);
        }
        cursor += 1;
    }
    None
}

fn char_literal_end(source: &str, start: usize) -> Option<usize> {
    let rest = &source[start + 1..];
    if rest.starts_with('\\') {
        return quoted_end(source.as_bytes(), start, b'\'');
    }

    let character = rest.chars().next()?;
    let closing = start + 1 + character.len_utf8();
    (source.as_bytes().get(closing) == Some(&b'\'')).then_some(closing + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(lines: &[usize]) -> BTreeSet<usize> {
        lines.iter().copied().collect()
    }

    #[test]
    fn parses_added_lines_from_zero_context_diff() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n-old\n+new\n+added\n@@ -3,0 +5 @@\n+last\n";
        let files = parse_diff(diff).expect("diff should parse");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].added_lines, added(&[1, 2, 5]));
    }

    #[test]
    fn brace_matching_ignores_nested_blocks_comments_and_literals() {
        let source = r##"
pub extern "C" fn example() {
    let text = "}";
    let raw = r#"{"#;
    let character = '}';
    /* } */
    if true { nested(); }
}
"##;
        let code = code_positions(source);
        let signature_end = source.find("example").expect("function name") + "example".len();
        let (start, end) = function_body(source, &code, signature_end).expect("function body");

        assert_eq!(
            &source[start..end],
            source[source.find('{').unwrap()..].trim_end()
        );
    }

    #[test]
    fn new_uncontained_function_is_reported() {
        let source = "pub extern \"C\" fn exposed() { risky(); }\n";
        let violations = lint_source("src/lib.rs", source, &added(&[1])).unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].function, "exposed");
    }

    #[test]
    fn multiline_signature_with_wrapper_is_accepted() {
        let source = "pub unsafe\nextern \"C\" fn exposed() {\n    wrap_with_void_ffi_result!({ risky(); })\n}\n";
        let violations = lint_source("src/lib.rs", source, &added(&[2])).unwrap();

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn no_catch_wrapper_counts_as_an_explicit_acknowledgement() {
        let source = "pub extern \"C\" fn exposed() {\n    wrap_with_ffi_result_no_catch!({ safe_call() })\n}\n";
        let violations = lint_source("src/lib.rs", source, &added(&[1])).unwrap();

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn containment_name_in_a_comment_does_not_pass() {
        let source = "pub extern \"C\" fn exposed() {\n    // catch_unwind is intentionally absent\n    risky();\n}\n";
        let violations = lint_source("src/lib.rs", source, &added(&[1])).unwrap();

        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn justified_escape_hatch_is_accepted() {
        let source = "// allow(ffi-panic-boundary): reads a plain integer only\npub extern \"C\" fn exposed() { 42; }\n";
        let violations = lint_source("src/lib.rs", source, &added(&[2])).unwrap();

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn escape_hatch_without_justification_is_reported() {
        let source = "// allow(ffi-panic-boundary):\npub extern \"C\" fn exposed() { 42; }\n";
        let violations = lint_source("src/lib.rs", source, &added(&[2])).unwrap();

        assert_eq!(violations.len(), 1);
        assert!(violations[0].problem.contains("without a justification"));
    }

    #[test]
    fn unchanged_function_is_ignored() {
        let source = "pub extern \"C\" fn existing() { risky(); }\n\nfn changed() {}\n";
        let violations = lint_source("src/lib.rs", source, &added(&[3])).unwrap();

        assert!(violations.is_empty(), "{violations:?}");
    }
}
