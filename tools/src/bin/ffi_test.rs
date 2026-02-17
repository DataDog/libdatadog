// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI Test Runner
//!
//! Builds and runs FFI examples to verify they compile and work correctly.
//! Usage: cargo ffi-test [--skip-build] [--filter <pattern>] [--keep-artifacts]

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::Colorize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

/// Convert a Path to &str, returning an error if it contains non-UTF-8
/// characters.
macro_rules! path_str {
    ($path:expr) => {
        $path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("path contains non-UTF-8 characters: {:?}", $path))
    };
}

#[derive(Parser, Debug)]
#[command(name = "ffi-test")]
#[command(about = "Build and run FFI examples to verify they compile and work correctly")]
struct Args {
    /// Skip building libraries and examples (use existing build)
    #[arg(long)]
    skip_build: bool,
    /// Only run examples whose names contain this case-sensitive substring
    #[arg(long)]
    filter: Option<String>,
    /// Keep temp directory with test artifacts for debugging
    #[arg(long)]
    keep_artifacts: bool,
    /// Timeout in seconds for each test (default: 300)
    #[arg(long, default_value = "300")]
    timeout: u64,
}

#[derive(Debug, PartialEq)]
enum TestStatus {
    Passed,
    Failed(String),
    Skipped(String),
    ExpectedFailure,
    UnexpectedPass,
    TimedOut,
}

struct TestResult {
    name: String,
    duration_ms: u128,
    status: TestStatus,
    output: String,
}

impl TestResult {
    fn print(&self) {
        let duration = format!("[{}ms]", self.duration_ms).dimmed();

        match &self.status {
            TestStatus::Passed => {
                println!("{} {} {}", "PASS".green(), self.name, duration);
            }
            TestStatus::Failed(msg) => {
                println!("{} {} ({}) {}", "FAIL".red(), self.name, msg, duration);
            }
            TestStatus::Skipped(reason) => {
                println!("{} {} ({}) {}", "SKIP".cyan(), self.name, reason, duration);
            }
            TestStatus::ExpectedFailure => {
                println!("{} {} {}", "XFAIL".yellow(), self.name, duration);
            }
            TestStatus::UnexpectedPass => {
                println!(
                    "{} {} {} {}",
                    "UPASS".yellow(),
                    self.name,
                    "(was expected to fail!)".yellow(),
                    duration
                );
            }
            TestStatus::TimedOut => {
                println!("{} {} {}", "TIMEOUT".red(), self.name, duration);
            }
        }
    }

    fn is_failure(&self) -> bool {
        matches!(
            self.status,
            TestStatus::Failed(_) | TestStatus::TimedOut | TestStatus::UnexpectedPass
        )
    }
}

// FFI features to build
const FFI_FEATURES: &[&str] = &[
    "profiling",
    "telemetry",
    "data-pipeline",
    "symbolizer",
    "crashtracker",
    "library-config",
    "log",
    "ddsketch",
    "ffe",
];

fn skip_examples() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            ("crashtracking", "intentionally crashes"),
            ("exporter", "requires CLI arguments"),
            ("exporter_manager", "Flaky because SIGPIPE thing"),
        ])
    })
}

fn expected_failures() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([(
            "trace_exporter",
            "expects trace data, fails on empty buffer",
        )])
    })
}

// Test data directories to symlink into work directory
const TEST_DATA_PATHS: &[&str] = &["datadog-ffe/tests/data"];

/// Run a command with output streamed to terminal
fn run_passthrough(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {}", what))?;
    if !status.success() {
        return Err(anyhow!("{} failed with {:?}", what, status.code()));
    }
    Ok(())
}

/// Run a command quietly and capture its output
fn run_capture(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd.output().with_context(|| format!("running {}", what))?;
    if !out.status.success() {
        return Err(anyhow!("{} failed with {:?}", what, out.status.code()));
    }
    Ok(out.stdout)
}

fn find_project_root() -> Result<PathBuf> {
    let stdout = run_capture(
        Command::new("cargo").args(["locate-project", "--workspace", "--message-format", "plain"]),
        "cargo locate-project",
    )?;
    let cargo_toml = std::str::from_utf8(&stdout)?.trim();

    Ok(PathBuf::from(cargo_toml)
        .parent()
        .ok_or_else(|| anyhow!("Cargo.toml has no parent directory"))?
        .to_path_buf())
}

fn build_ffi_libraries(project_root: &Path) -> Result<()> {
    println!("\n{}", "Building FFI libraries...".bold());

    let start = Instant::now();
    run_passthrough(
        Command::new("cargo").current_dir(project_root).args([
            "run",
            "--bin",
            "release",
            "--features",
            &FFI_FEATURES.join(","),
            "--release",
            "--",
            "--out",
            "release",
        ]),
        "FFI library build",
    )?;

    println!(
        "{}",
        format!("Built in {:.1}s", start.elapsed().as_secs_f32()).green()
    );

    Ok(())
}

fn build_examples(project_root: &Path) -> Result<()> {
    println!("\n{}", "Building FFI examples...".bold());

    let examples_dir = project_root.join("examples/ffi");
    let build_dir = examples_dir.join("build");
    let release_dir = project_root.join("release");

    let start = Instant::now();

    run_passthrough(
        Command::new("cmake").current_dir(project_root).args([
            "-S",
            path_str!(&examples_dir)?,
            "-B",
            path_str!(&build_dir)?,
            "-DCMAKE_BUILD_TYPE=Release",
            &format!("-DDatadog_ROOT={}", release_dir.display()),
        ]),
        "CMake configure",
    )?;

    run_passthrough(
        Command::new("cmake").args(["--build", path_str!(&build_dir)?]),
        "CMake build",
    )?;

    println!(
        "{}",
        format!("Built in {:.1}s", start.elapsed().as_secs_f32()).green()
    );

    Ok(())
}

#[cfg(windows)]
fn copy_dir_all(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}

fn setup_work_dir(project_root: &Path) -> Result<PathBuf> {
    let work_dir = env::temp_dir().join(format!(
        "ffi-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    ));
    fs::create_dir_all(&work_dir).context("creating work directory")?;

    // Symlink test data into work directory
    for rel_path in TEST_DATA_PATHS {
        let src = project_root.join(rel_path);
        if !src.exists() {
            continue;
        }
        let dest = work_dir.join(rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, &dest).context("creating symlink")?;

        #[cfg(windows)]
        if src.is_dir() {
            copy_dir_all(&src, &dest)?;
        } else {
            fs::copy(&src, &dest)?;
        }
    }

    Ok(work_dir)
}

/// Spawn a test process and return child with captured output handles
fn spawn_test(exe_path: &Path, work_dir: &Path) -> Result<std::process::Child> {
    Command::new(exe_path)
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", exe_path.display()))
}

/// Read all bytes from an optional reader, ignoring errors.
/// Errors are ignored because test result is determined by exit code, not I/O
/// success.
fn read_all<R: Read>(mut reader: Option<R>) -> Vec<u8> {
    reader
        .as_mut()
        .and_then(|r| {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).ok()?;
            Some(buf)
        })
        .unwrap_or_default()
}

/// Wait for child process with output capture in background threads
fn wait_with_output(
    mut child: std::process::Child,
    timeout: Duration,
) -> (Option<std::process::ExitStatus>, String) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || read_all(stdout));
    let stderr_thread = std::thread::spawn(move || read_all(stderr));

    let exit_status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => Some(status),
        Ok(None) => {
            // Timed out
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        Err(_) => None,
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    let output = format!(
        "STDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );

    (exit_status, output)
}

/// Format exit status for error messages, including signal info on Unix
fn format_exit_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let signal_name = match signal {
                1 => "SIGHUP",
                2 => "SIGINT",
                3 => "SIGQUIT",
                4 => "SIGILL",
                6 => "SIGABRT",
                8 => "SIGFPE",
                9 => "SIGKILL",
                11 => "SIGSEGV",
                13 => "SIGPIPE",
                14 => "SIGALRM",
                15 => "SIGTERM",
                _ => "unknown",
            };
            return format!("killed by signal {} ({})", signal, signal_name);
        }
    }
    format!("exit code {:?}", status.code())
}

/// Determine the test status from exit result and expected failure status
fn determine_status(
    exit_status: Option<std::process::ExitStatus>,
    is_expected_failure: bool,
) -> TestStatus {
    match exit_status {
        Some(status) => {
            let success = status.success();
            match (success, is_expected_failure) {
                (true, false) => TestStatus::Passed,
                (true, true) => TestStatus::UnexpectedPass,
                (false, true) => TestStatus::ExpectedFailure,
                (false, false) => TestStatus::Failed(format_exit_status(&status)),
            }
        }
        None => TestStatus::TimedOut,
    }
}

fn run_test(name: &str, exe_path: &Path, work_dir: &Path, timeout: Duration) -> TestResult {
    let is_expected_failure = expected_failures().contains_key(name);
    let start = Instant::now();

    let child = match spawn_test(exe_path, work_dir) {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                name: name.to_string(),
                duration_ms: start.elapsed().as_millis(),
                status: TestStatus::Failed(format!("spawn error: {}", e)),
                output: e.to_string(),
            };
        }
    };

    let (exit_status, output) = wait_with_output(child, timeout);
    let status = determine_status(exit_status, is_expected_failure);

    TestResult {
        name: name.to_string(),
        duration_ms: start.elapsed().as_millis(),
        status,
        output,
    }
}

fn find_executables(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    visit_dirs(dir, &mut |path| {
        if is_executable(path) {
            result.push(path.to_path_buf());
        }
    });
    result.sort_unstable();
    result
}

fn visit_dirs(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                cb(&path);
            } else if path.is_dir() {
                // Skip CMake internal directories
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name != "CMakeFiles" && name != "_deps" {
                        visit_dirs(&path, cb);
                    }
                }
            }
        }
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
    }
}

fn run_examples(
    project_root: &Path,
    filter: Option<&str>,
    keep_artifacts: bool,
    timeout: Duration,
) -> Result<Vec<TestResult>> {
    println!("\n{}", "Running FFI examples...".bold());

    let build_dir = project_root.join("examples/ffi/build");
    let work_dir = setup_work_dir(project_root).context("setting up work directory")?;
    println!("Work directory: {}\n", work_dir.display());

    let executables = find_executables(&build_dir);
    if executables.is_empty() {
        cleanup_work_dir(&work_dir, keep_artifacts);
        return Err(anyhow!("no executables found in {}", build_dir.display()));
    }

    let mut results = Vec::new();
    for exe in &executables {
        let name = match exe.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };

        // Apply filter
        if let Some(f) = filter {
            if !name.contains(f) {
                continue;
            }
        }

        // Check skip list
        if let Some(reason) = skip_examples().get(name) {
            let result = TestResult {
                name: name.to_string(),
                duration_ms: 0,
                status: TestStatus::Skipped(reason.to_string()),
                output: String::new(),
            };
            result.print();
            results.push(result);
            continue;
        }

        let result = run_test(name, exe, &work_dir, timeout);
        result.print();
        results.push(result);
    }

    if results.is_empty() {
        cleanup_work_dir(&work_dir, keep_artifacts);
        return Err(anyhow!("no tests matched filter"));
    }

    // Show artifacts created
    let artifacts = collect_artifacts(&work_dir);
    if !artifacts.is_empty() {
        println!("\nGenerated {} artifacts", artifacts.len());
    }

    cleanup_work_dir(&work_dir, keep_artifacts);
    Ok(results)
}

fn collect_artifacts(dir: &Path) -> Vec<PathBuf> {
    let mut artifacts = Vec::new();
    visit_dirs(dir, &mut |path| {
        // Only include regular files (skip symlinks to source data)
        if fs::symlink_metadata(path)
            .map(|m| !m.is_symlink())
            .unwrap_or(false)
        {
            artifacts.push(path.to_path_buf());
        }
    });
    artifacts
}

fn cleanup_work_dir(work_dir: &Path, keep: bool) {
    if keep {
        println!("\nArtifacts kept at: {}", work_dir.display());
    } else {
        let _ = fs::remove_dir_all(work_dir);
    }
}

fn print_summary(results: &[TestResult]) -> ExitCode {
    let passed = results
        .iter()
        .filter(|r| r.status == TestStatus::Passed)
        .count();
    let failed = results.iter().filter(|r| r.is_failure()).count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, TestStatus::Skipped(_)))
        .count();
    let xfail = results
        .iter()
        .filter(|r| r.status == TestStatus::ExpectedFailure)
        .count();

    println!("\n{}", "Summary".bold());
    println!("  Passed:   {}", passed);
    println!("  Failed:   {}", failed);
    println!("  Expected: {}", xfail);
    println!("  Skipped:  {}", skipped);
    println!("  Total:    {}", results.len());

    // Show failure details
    let failures: Vec<_> = results.iter().filter(|r| r.is_failure()).collect();
    if !failures.is_empty() {
        println!("\n{}", "Failure Details".bold());
        for result in failures {
            println!("\n--- {} ---", result.name.red());
            let output = &result.output;
            let truncate_at = output
                .char_indices()
                .nth(2000)
                .map(|(idx, _)| idx)
                .unwrap_or(output.len());
            if truncate_at < output.len() {
                println!("{}...\n[truncated]", &output[..truncate_at]);
            } else {
                println!("{}", output);
            }
        }
        println!("\n{}", format!("FAILED: {} tests", failed).red());
        return ExitCode::FAILURE;
    }

    println!("\n{}", "All tests passed!".green());
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = Args::parse();

    let project_root = match find_project_root() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Could not find project root: {:#}", e);
            return ExitCode::FAILURE;
        }
    };

    println!("Project root: {}", project_root.display());

    if !args.skip_build {
        if let Err(e) = build_ffi_libraries(&project_root) {
            eprintln!("Error: {:#}", e);
            return ExitCode::FAILURE;
        }
        if let Err(e) = build_examples(&project_root) {
            eprintln!("Error: {:#}", e);
            return ExitCode::FAILURE;
        }
    }

    let results = match run_examples(
        &project_root,
        args.filter.as_deref(),
        args.keep_artifacts,
        Duration::from_secs(args.timeout),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {:#}", e);
            return ExitCode::FAILURE;
        }
    };

    print_summary(&results)
}
