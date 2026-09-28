// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for the `ffi-panic-lint` CLI.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_ffi-panic-lint");
static TEMP_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "ffi-panic-lint-{label}-{}-{id}",
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

struct Fixture {
    root: TempDir,
    base: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = TempDir::new(label);
        git(root.path(), &["init", "--quiet", "--initial-branch=main"]);
        write_file(
            &root.path().join("src/lib.rs"),
            "pub extern \"C\" fn existing_uncontained() { risky(); }\n",
        );
        let base = commit_fixture(root.path(), None, "base");
        Self { root, base }
    }

    fn commit(&self, message: &str) {
        commit_fixture(self.root.path(), Some(&self.base), message);
    }

    fn run(&self) -> Output {
        Command::new(BINARY)
            .current_dir(self.root.path())
            .args(["--base-ref", &self.base])
            .output()
            .expect("failed to run ffi-panic-lint")
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create directory");
    }
    std::fs::write(path, contents).expect("failed to write file");
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be UTF-8")
        .trim()
        .to_owned()
}

/// Create a deterministic commit object without depending on the machine's Git identity.
fn commit_fixture(root: &Path, parent: Option<&str>, message: &str) -> String {
    git(root, &["add", "--all"]);
    let tree = git(root, &["write-tree"]);
    let parent = parent.map_or_else(String::new, |sha| format!("parent {sha}\n"));
    let commit = format!(
        "tree {tree}\n{parent}author Test <test@example.com> 0 +0000\ncommitter Test <test@example.com> 0 +0000\n\n{message}\n"
    );

    let mut child = Command::new("git")
        .current_dir(root)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to start git hash-object");
    child
        .stdin
        .take()
        .expect("git stdin")
        .write_all(commit.as_bytes())
        .expect("failed to write commit object");
    let output = child.wait_with_output().expect("failed to write commit");
    assert!(output.status.success(), "git hash-object failed");
    let sha = String::from_utf8(output.stdout)
        .expect("commit hash should be UTF-8")
        .trim()
        .to_owned();
    git(root, &["update-ref", "refs/heads/main", &sha]);
    sha
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn new_uncontained_function_fails_but_existing_one_is_ignored() {
    let fixture = Fixture::new("uncontained");
    write_file(
        &fixture.root.path().join("src/lib.rs"),
        "pub extern \"C\" fn existing_uncontained() { risky(); }\n\npub unsafe extern \"C\" fn newly_uncontained() { risky(); }\n",
    );
    fixture.commit("add uncontained function");

    let output = fixture.run();
    assert!(!output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("1 violation(s)"), "{stdout}");
    assert!(stdout.contains("newly_uncontained"), "{stdout}");
    assert!(!stdout.contains("`existing_uncontained`"), "{stdout}");
}

#[test]
fn wrapper_and_justified_escape_hatch_pass() {
    let fixture = Fixture::new("contained");
    write_file(
        &fixture.root.path().join("src/lib.rs"),
        "pub extern \"C\" fn existing_uncontained() { risky(); }\n\npub extern \"C\" fn wrapped() {\n    wrap_with_void_ffi_result!({ risky(); })\n}\n\n// allow(ffi-panic-boundary): returns a constant and cannot panic\npub extern \"C\" fn justified() -> u32 { 42 }\n",
    );
    fixture.commit("add acknowledged functions");

    let output = fixture.run();
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(stdout_of(&output).contains("no violations found"));
}

#[test]
fn escape_hatch_without_justification_fails() {
    let fixture = Fixture::new("empty-allow");
    write_file(
        &fixture.root.path().join("src/lib.rs"),
        "pub extern \"C\" fn existing_uncontained() { risky(); }\n\n// allow(ffi-panic-boundary):\npub extern \"C\" fn unjustified() -> u32 { 42 }\n",
    );
    fixture.commit("add unjustified function");

    let output = fixture.run();
    assert!(!output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("unjustified"), "{stdout}");
    assert!(stdout.contains("without a justification"), "{stdout}");
}
