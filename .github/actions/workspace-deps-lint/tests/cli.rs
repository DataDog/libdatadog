// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for the `workspace-deps-lint` CLI.
//!
//! Each test builds a small workspace fixture in a temporary directory, runs the compiled binary
//! against it, and asserts on the exit code and output. The fixtures keep the workspace root clean
//! and put a rule-1 violation in the `bad` member, so exit codes unambiguously reflect whether the
//! `--package`/`-p` selection included it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Path of the compiled binary, provided by cargo to integration tests.
const BINARY: &str = env!("CARGO_BIN_EXE_workspace-deps-lint");

/// Makes temporary directory names unique across parallel tests.
static TEMP_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "workspace-deps-lint-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("failed to create temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create directory");
    }
    std::fs::write(path, contents).expect("failed to write file");
}

/// A three-member workspace: `good` and `other` are clean, `bad` declares a non-inherited
/// dependency. The workspace root itself is clean.
fn fixture(label: &str) -> TempDir {
    let root = TempDir::new(label);
    write_file(
        &root.path().join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"good\", \"bad\", \"other\"]\n\n[workspace.dependencies]\nanyhow = { version = \"1.0\", default-features = false }\n",
    );
    write_file(
        &root.path().join("good/Cargo.toml"),
        "[package]\nname = \"good\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nanyhow.workspace = true\n",
    );
    write_file(
        &root.path().join("bad/Cargo.toml"),
        "[package]\nname = \"bad\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    write_file(
        &root.path().join("other/Cargo.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    for member in ["good", "bad", "other"] {
        write_file(&root.path().join(member).join("src/lib.rs"), "");
    }
    root
}

/// Run the compiled binary against the fixture workspace root.
fn run(root: &TempDir, args: &[&str]) -> Output {
    Command::new(BINARY)
        .arg("--manifest-path")
        .arg(root.path().join("Cargo.toml"))
        .args(args)
        .output()
        .expect("failed to run workspace-deps-lint")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn no_package_selection_checks_every_manifest() {
    let root = fixture("no-selection");
    let output = run(&root, &[]);

    // `bad` violates rule 1, so the run fails and reports it.
    assert!(!output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("1 violation(s)"), "{stdout}");
    assert!(stdout.contains("bad/Cargo.toml"), "{stdout}");
    assert!(stdout.contains("`serde`"), "{stdout}");
}

#[test]
fn single_package_selection_checks_only_it() {
    let root = fixture("single");
    // Exercise the long spelling of the flag.
    let output = run(&root, &["--package", "good"]);

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("1 manifest(s) checked"), "{stdout}");
    assert!(stdout.contains("no violation found"), "{stdout}");
}

#[test]
fn multiple_package_selection_checks_only_the_selected_ones() {
    let root = fixture("multiple");
    let output = run(&root, &["-p", "good", "-p", "other"]);

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("2 manifest(s) checked"), "{stdout}");
    assert!(stdout.contains("no violation found"), "{stdout}");
}

#[test]
fn selected_violating_package_is_reported() {
    let root = fixture("violator");
    let output = run(&root, &["-p", "bad"]);

    assert!(!output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("1 violation(s)"), "{stdout}");
    assert!(stdout.contains("bad/Cargo.toml"), "{stdout}");
    assert!(stdout.contains("`serde`"), "{stdout}");
}

#[test]
fn unknown_package_is_an_error() {
    let root = fixture("unknown");
    let output = run(&root, &["-p", "good", "-p", "ghost"]);

    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("ghost"), "{stderr}");
    assert!(
        stderr.contains("couldn't be found in the root workspace manifest"),
        "{stderr}"
    );
    // The error aborts before any report is printed.
    assert!(output.stdout.is_empty());
}

#[test]
fn root_manifest_is_linted_regardless_of_the_selection() {
    let root = fixture("dirty-root");
    // Make [workspace.dependencies] of the root violate rule 2, while keeping the entries the
    // members inherit from.
    write_file(
        &root.path().join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"good\", \"bad\", \"other\"]\n\n[workspace.dependencies]\nanyhow = { version = \"1.0\", default-features = false }\nserde = \"1\"\n",
    );
    let output = run(&root, &["-p", "good"]);

    // The root violation is still reported, while the unselected `bad` member is not.
    assert!(!output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("1 violation(s)"), "{stdout}");
    assert!(stdout.contains("bare version string"), "{stdout}");
    assert!(!stdout.contains("bad/Cargo.toml"), "{stdout}");
}
