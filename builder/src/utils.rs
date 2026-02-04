// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Result};
use regex::Regex;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Child;

pub fn file_replace(file_in: &str, file_out: &str, target: &str, replace: &str) -> Result<()> {
    let content = fs::read_to_string(file_in)?;
    let new = content.replace(target, replace);

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(file_out)?;
    file.write_all(new.as_bytes())
        .map_err(|err| anyhow!("failed to write file: {}", err))
}

pub fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

pub(crate) fn adjust_extern_symbols(
    file_in: impl AsRef<Path>,
    file_out: impl AsRef<Path>,
) -> Result<()> {
    let content = fs::read_to_string(file_in)?;
    let re = Regex::new(r#"(?m)^(\s*)extern\s+(.+;)$"#).unwrap();

    // Replace function using captures
    let new_content = re.replace_all(&content, |caps: &regex::Captures| {
        let full_match = caps.get(0).unwrap().as_str();
        let indent = &caps[1];
        let declaration = &caps[2];

        // Skip if it's extern "C", already has LIBDD_DLLIMPORT, or contains '(' (function)
        if full_match.contains("extern \"C\"")
            || full_match.contains("LIBDD_DLLIMPORT")
            || full_match.contains('(')
        {
            return full_match.to_string();
        }

        // Keep indent + "extern " + "LIBDD_DLL_IMPORT " + declaration
        format!("{}extern LIBDD_DLLIMPORT {}", indent, declaration)
    });

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(file_out)?;
    file.write_all(new_content.as_bytes())
        .map_err(|err| anyhow!("failed to write file: {}", err))
}

/// Waits for a child process to complete and panics if it fails.
pub fn wait_for_success(mut child: Child, name: &str) {
    let status = child
        .wait()
        .unwrap_or_else(|_| panic!("{name} failed to wait"));
    assert!(
        status.success(),
        "{name} failed with exit code: {:?}",
        status.code()
    );
}
