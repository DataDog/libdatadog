// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
