// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use anyhow::{Context, Result};
use datadog_crashtracker::CrashtrackerConfiguration;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::modes::unix::*;

/// Defines the additional behavior for a given crashtracking test
pub trait Behavior {
    fn setup(&self, output_dir: &Path, config: &mut CrashtrackerConfiguration) -> Result<()>;
    fn pre(&self, output_dir: &Path) -> Result<()>;
    fn post(&self, output_dir: &Path) -> Result<()>;
}

pub fn fileat_content_equals(dir: &Path, filename: &str, contents: &str) -> anyhow::Result<bool> {
    let filepath = dir.join(filename);
    file_content_equals(&filepath, contents)
}

pub fn file_content_equals(filepath: &Path, contents: &str) -> anyhow::Result<bool> {
    let file_contents = std::fs::read_to_string(filepath)
        .with_context(|| format!("Failed to read file: {}", filepath.display()))?;
    Ok(file_contents.trim() == contents)
}

pub fn file_append_msg(filepath: &Path, contents: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(filepath)
        .with_context(|| format!("Failed to open file: {}", filepath.display()))?;

    file.write_all(contents.as_bytes())
        .with_context(|| format!("Failed to write to file: {}", filepath.display()))?;

    Ok(())
}

pub fn atom_to_clone<T: Clone>(atom: &AtomicPtr<T>) -> Result<T> {
    let ptr = atom.load(Ordering::SeqCst);
    if ptr.is_null() {
        anyhow::bail!("Pointer was null");
    }

    // If not null, clone the referenced value
    unsafe {
        ptr.as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Failed to clone"))
    }
}

pub fn set_atomic<T>(atom: &AtomicPtr<T>, value: T) {
    let box_ptr = Box::into_raw(Box::new(value));
    let old = atom.swap(box_ptr, Ordering::SeqCst);
    if !old.is_null() {
        unsafe {
            // Drop the previous value safely
            let _ = Box::from_raw(old);
        }
    }
}

pub fn removeat_permissive(dir: &Path, filename: &str) {
    let filepath = dir.join(filename);
    remove_permissive(&filepath);
}

pub fn remove_permissive(filepath: &Path) {
    // Removes the file if it exists.  If it doesn't exist, it's not an error or anything.
    let _ = std::fs::remove_file(filepath);
}

pub fn get_behavior(mode_str: &str) -> Box<dyn Behavior> {
    match mode_str {
        "donothing" => Box::new(test_000_donothing::Test),
        "sigpipe" => Box::new(test_001_sigpipe::Test),
        "sigchld" => Box::new(test_002_sigchld::Test),
        "sigchld_exec" => Box::new(test_003_sigchld_with_exec::Test),
        "donothing_sigstack" => Box::new(test_004_donothing_sigstack::Test),
        "sigpipe_sigstack" => Box::new(test_005_sigpipe_sigstack::Test),
        "sigchld_sigstack" => Box::new(test_006_sigchld_sigstack::Test),
        "chained" => Box::new(test_007_chaining::Test),
        _ => panic!("Unknown mode: {}", mode_str),
    }
}
