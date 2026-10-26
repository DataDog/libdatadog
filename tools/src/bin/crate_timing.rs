// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Per-crate CI cost attribution: how long each crate's tests and benchmarks take
//! to execute. Used by .github/workflows/crate-timing.yml. Measures only, never
//! gates: failures are recorded in the report and the run continues.
//!
//! Everything is built upfront, untimed, so timed runs never compile. Two exceptions:
//! doc tests compile inside `cargo test --doc`, and the bin_tests row includes its
//! nextest `prebuild-bin-tests` setup script -- both match what test.yml pays too.
//! The timed invocations mirror test.yml's test shapes and run_benchmarks_ci.sh's
//! bench settings, so the numbers attribute the real CI jobs.

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Parser;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
use tools::{doc, junit};

// Same crates and flags as .github/workflows/test.yml.
const TRACING_CRATES: [&str; 3] = [
    "libdd-data-pipeline",
    "libdd-trace-utils",
    "libdd-tracer-flare",
];
const CRASHTRACKER_FEATURE: &str = "libdd-crashtracker/generate-unit-test-files";
// builder has no tests of interest in test.yml either; test_spawn_from_lib only runs
// there under RUSTFLAGS="-C prefer-dynamic", a shape this tool never builds.
const SKIPPED_CRATES: [&str; 2] = ["builder", "test_spawn_from_lib"];

// Rows in the "Slowest tests" section: the outlier view, kept short; the
// breakdown tree below it holds the rest.
const SLOWEST_SHOWN: usize = 10;

const SCOPE: &str = "_Wall-clock, measured by tools/src/bin/crate_timing.rs. Covers the main \
                     nextest pass (+ doc tests, + the serial tracing-integration pass) and the \
                     criterion benchmarks at the benchmarks job's sampling settings. Other CI \
                     shapes (all-features, no-default-features, fips, centos7, other platforms) \
                     are not attributed here._";

// Must match benchmark/bench_features.sh (used by the GitLab benchmarks job).
fn bench_features_for_crate(crate_name: &str) -> Vec<&'static str> {
    match crate_name {
        "libdd-crashtracker" => vec!["libdd-crashtracker/benchmarking"],
        "libdd-sampling" => vec!["libdd-sampling/v04_span", "libdd-sampling/bench-internals"],
        "libdd-trace-utils" => vec!["libdd-trace-utils/bench-internals"],
        _ => vec![],
    }
}

// Must stay identical to benchmark/run_benchmarks_ci.sh's criterion args, so the
// bench column is the real benchmarks job's per-crate cost.
const CRITERION_ARGS: [&str; 5] = [
    "--warm-up-time",
    "1",
    "--measurement-time",
    "5",
    "--sample-size=200",
];

#[derive(Parser, Debug)]
#[command(name = "crate_timing")]
#[command(about = "Measure and report per-crate test and benchmark execution time")]
struct Args {
    /// Restrict to these crates (default: all); benches are scoped the same way.
    #[arg(long, num_args = 1..)]
    packages: Vec<String>,

    /// Whether the report includes the per-test breakdown tree.
    #[arg(long, value_enum, default_value_t = Format::Tree)]
    format: Format,

    /// Levels shown in the breakdown tree: 1 = crates, 2 = +modules, 3 = +tests
    /// (ignored by --format table).
    #[arg(long, default_value_t = 3)]
    depth: usize,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Format {
    Tree,
    Table,
}

/// The timed execution shapes, one per report column.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Tests,
    Tracing,
    Doc,
    Bench,
}

impl Phase {
    const ALL: [Phase; 4] = [Phase::Tests, Phase::Tracing, Phase::Doc, Phase::Bench];

    fn name(self) -> &'static str {
        ["tests", "tracing", "doc", "bench"][self as usize]
    }
}

struct Measurement {
    crate_name: Rc<str>,
    phase: Phase,
    secs: u64,
    ok: bool,
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let status = if self.ok { "ok" } else { "fail" };
        write!(
            f,
            "{:<9} {:<38} {:>5}s {status}",
            self.phase.name(),
            self.crate_name,
            self.secs
        )
    }
}

#[derive(Default)]
struct CrateTimes {
    secs: [u64; Phase::ALL.len()],
    bench_fns: u64,
    failed: Vec<Phase>,
}

impl CrateTimes {
    fn get(&self, phase: Phase) -> u64 {
        self.secs[phase as usize]
    }

    fn set(&mut self, phase: Phase, secs: u64) {
        self.secs[phase as usize] = secs;
    }

    fn total(&self) -> u64 {
        self.secs.iter().sum()
    }
}

type Times = BTreeMap<Rc<str>, CrateTimes>;

fn record(times: &mut Times, m: &Measurement) {
    let entry = times
        .get_mut(m.crate_name.as_ref())
        .expect("crate measured outside the selection");
    entry.set(m.phase, m.secs);
    if !m.ok {
        entry.failed.push(m.phase);
    }
}

struct TestResult {
    crate_name: Rc<str>,
    name: String,
    secs: f64,
    ok: bool,
}

struct Selection {
    crates: Vec<Rc<str>>,
    bench_crates: Vec<Rc<str>>,
    /// Packages with a library target; `cargo test --doc` fails on the others
    /// ("no library targets found in package").
    doc_crates: BTreeSet<Rc<str>>,
}

impl Selection {
    fn from(metadata: &cargo_metadata::Metadata, selected: &[String]) -> Result<Self> {
        let is_selected = |name: &Rc<str>| selected.iter().any(|s| s == name.as_ref());
        let is_benched = |p: &cargo_metadata::Package| {
            p.targets
                .iter()
                .any(|t| t.kind.iter().any(|k| k.as_str() == "bench"))
        };
        let has_lib = |p: &cargo_metadata::Package| {
            p.targets.iter().any(|t| {
                t.kind
                    .iter()
                    .any(|k| matches!(k.as_str(), "lib" | "rlib" | "dylib" | "proc-macro"))
            })
        };

        let mut crates: Vec<Rc<str>> = metadata
            .packages
            .iter()
            .map(|p| Rc::from(p.name.as_str()))
            .filter(|name: &Rc<str>| !SKIPPED_CRATES.contains(&name.as_ref()))
            .collect();
        let mut bench_crates: Vec<Rc<str>> = metadata
            .packages
            .iter()
            .filter(|&p| is_benched(p))
            .map(|p| Rc::from(p.name.as_str()))
            .collect();
        let doc_crates: BTreeSet<Rc<str>> = metadata
            .packages
            .iter()
            .filter(|&p| has_lib(p))
            .map(|p| Rc::from(p.name.as_str()))
            .collect();

        if !selected.is_empty() {
            crates.retain(&is_selected);
            bench_crates.retain(&is_selected);
            if crates.is_empty() && bench_crates.is_empty() {
                bail!("no selected crate is a workspace member");
            }
        }
        crates.sort();
        bench_crates.sort();
        Ok(Self {
            crates,
            bench_crates,
            doc_crates,
        })
    }
}

fn cargo() -> Command {
    Command::new("cargo")
}

fn nextest(crate_name: &str) -> Command {
    let mut cmd = cargo();
    cmd.args(["nextest", "run", "-p", crate_name]);
    cmd
}

fn bench(crate_name: &str) -> Command {
    let mut cmd = cargo();
    cmd.args(["bench", "-p", crate_name]);
    let features = bench_features_for_crate(crate_name);
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    cmd
}

// cargo rejects --features for a crate outside the selection.
fn with_own_features(cmd: &mut Command, crate_name: &str) {
    if crate_name == "libdd-crashtracker" {
        cmd.args(["--features", CRASHTRACKER_FEATURE]);
    }
}

struct Outcome {
    secs: u64,
    ok: bool,
}

fn run(cmd: &mut Command, log: &Path) -> Result<Outcome> {
    let start = Instant::now();
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {cmd:?}"))?;
    write_log(log, &output);
    Ok(Outcome {
        secs: start.elapsed().as_secs(),
        ok: output.status.success(),
    })
}

fn build(cmd: &mut Command, log: &Path, crate_name: &str) -> Result<()> {
    if !run(cmd, log)?.ok {
        bail!("build for {crate_name} failed, see {}", log.display());
    }
    Ok(())
}

fn measure(
    times: &mut Times,
    phase: Phase,
    crate_name: &Rc<str>,
    cmd: &mut Command,
    logs_dir: &Path,
) -> Result<()> {
    let log = logs_dir.join(format!("{}-{}.log", crate_name, phase.name()));
    let outcome = run(cmd, &log)?;
    let m = Measurement {
        crate_name: Rc::clone(crate_name),
        phase,
        secs: outcome.secs,
        ok: outcome.ok,
    };
    record(times, &m);
    println!("{m}");
    Ok(())
}

fn write_log(path: &Path, output: &std::process::Output) {
    let mut content = Vec::with_capacity(output.stdout.len() + output.stderr.len());
    content.extend_from_slice(&output.stdout);
    content.extend_from_slice(&output.stderr);
    let _ = fs::write(path, content);
}

/// Copies nextest's junit report aside and parses it, attributing every test to
/// `crate_name`.
fn junit_results(junit_dir: &Path, file_name: String, crate_name: &Rc<str>) -> Vec<TestResult> {
    let Ok(xml) = fs::read_to_string("target/nextest/ci/junit.xml") else {
        return Vec::new();
    };
    let _ = fs::write(junit_dir.join(file_name), &xml);
    junit::parse(&xml)
        .into_iter()
        .map(|tc| TestResult {
            crate_name: Rc::clone(crate_name),
            name: tc.name,
            secs: tc.secs,
            ok: tc.ok,
        })
        .collect()
}

/// Criterion nests every crate's functions flat under target/criterion (grouped and
/// parameterized benches get extra subdirs); every measured variant is a
/// `*/new/estimates.json` file.
fn bench_functions_since(root: &Path, since: SystemTime) -> u64 {
    let mut ids = BTreeSet::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if is_fresh_estimate(&path, since) {
                if let Some(id) = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(|rel| rel.ancestors().nth(2))
                {
                    ids.insert(id.to_string_lossy().into_owned());
                }
            }
        }
    }
    ids.len() as u64
}

fn is_fresh_estimate(path: &Path, since: SystemTime) -> bool {
    path.file_name().is_some_and(|n| n == "estimates.json")
        && path
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == "new"))
        && path
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since)
}

fn human(secs: f64) -> String {
    if secs < 60.0 {
        if secs == secs.trunc() {
            format!("{}s", secs as u64)
        } else {
            format!("{secs:.1}s")
        }
    } else {
        let total = secs.round() as u64;
        format!("{}m{:02}s", total / 60, total % 60)
    }
}

fn report_docs(times: &Times, tests: &[TestResult], args: &Args) -> Vec<doc::Doc> {
    let mut docs = vec![
        doc::Doc::Heading {
            level: 1,
            text: "Per-crate test & benchmark execution time".into(),
        },
        doc::Doc::Para(format!("{SCOPE} {}", settings(args))),
        doc::Doc::Table(crate_table(times)),
    ];
    if !tests.is_empty() {
        docs.push(doc::Doc::Heading {
            level: 2,
            text: "Slowest tests".into(),
        });
        docs.push(doc::Doc::Table(slowest_tests(tests, SLOWEST_SHOWN)));
        if matches!(args.format, Format::Tree) {
            docs.push(doc::Doc::Heading {
                level: 2,
                text: "Test breakdown".into(),
            });
            docs.push(doc::Doc::Tree(
                test_tree(tests)
                    .into_iter()
                    .map(|root| root.into_doc(args.depth))
                    .collect(),
            ));
        }
    }
    docs
}

/// The invocation settings the report was produced with, so an artifact says what
/// made it.
fn settings(args: &Args) -> String {
    let rendering = match args.format {
        Format::Tree => format!("slowest tests plus a tree to depth {}", args.depth),
        Format::Table => "slowest tests only".to_string(),
    };
    let crates = if args.packages.is_empty() {
        "all workspace crates".to_string()
    } else {
        let n = args.packages.len();
        format!("{n} selected crate{}", if n == 1 { "" } else { "s" })
    };
    format!("Measured at {} {rendering}, over {crates}.", commit())
}

fn commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "an unknown commit".to_string())
}

fn crate_table(times: &Times) -> doc::Table {
    let mut columns = vec![doc::Column::new("crate", doc::Align::Left)];
    columns.extend(
        Phase::ALL
            .iter()
            .map(|phase| doc::Column::new(phase.name(), doc::Align::Right)),
    );
    columns.extend(
        ["bench fns", "s/fn", "total", "share"]
            .iter()
            .map(|&title| doc::Column::new(title, doc::Align::Right)),
    );
    columns.push(doc::Column::new("failed", doc::Align::Left));

    let mut table = doc::Table::new(columns);
    let mut by_total_desc: Vec<(&Rc<str>, &CrateTimes)> = times.iter().collect();
    by_total_desc.sort_by_key(|&(_, t)| std::cmp::Reverse(t.total()));

    let grand_total = times.values().map(CrateTimes::total).sum::<u64>();
    for (name, t) in by_total_desc {
        let mut cells = vec![name.to_string()];
        cells.extend(Phase::ALL.iter().map(|p| human(t.get(*p) as f64)));
        cells.push(t.bench_fns.to_string());
        cells.push(bench_secs_per_fn(t));
        cells.push(human(t.total() as f64));
        cells.push(share(t.total(), grand_total));
        cells.push(failed_phases(t));
        table.row(cells);
    }
    table.row(totals_row(times));
    table
}

fn totals_row(times: &Times) -> Vec<String> {
    let sum = |phase: Phase| times.values().map(|t| t.get(phase)).sum::<u64>();
    let bench_fns = times.values().map(|t| t.bench_fns).sum::<u64>();
    let grand_total = times.values().map(CrateTimes::total).sum::<u64>();
    let mut cells: Vec<String> = vec!["**total**".into()];
    cells.extend(Phase::ALL.iter().map(|p| human(sum(*p) as f64)));
    cells.push(bench_fns.to_string());
    cells.push(if bench_fns == 0 {
        String::new()
    } else {
        human(sum(Phase::Bench) as f64 / bench_fns as f64)
    });
    cells.push(human(grand_total as f64));
    cells.push(share(grand_total, grand_total));
    cells.push(String::new());
    cells
}

fn failed_phases(t: &CrateTimes) -> String {
    t.failed
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// Bench seconds per criterion function. The sampling settings put a floor
/// under this (~6s per fn at warm-up 1s + measurement 5s), so it separates
/// "many functions" from "slow functions".
fn bench_secs_per_fn(t: &CrateTimes) -> String {
    if t.bench_fns == 0 {
        String::new()
    } else {
        human(t.get(Phase::Bench) as f64 / t.bench_fns as f64)
    }
}

/// The crate's share of all measured time, in percent.
fn share(secs: u64, grand_total: u64) -> String {
    if grand_total == 0 {
        String::new()
    } else {
        format!("{:.1}%", secs as f64 / grand_total as f64 * 100.0)
    }
}

/// A crate, module or test in the per-test breakdown tree. Internal nodes carry
/// the sum of their children's times and any descendant's failure.
struct TestNode {
    label: String,
    secs: f64,
    failed: bool,
    children: Vec<TestNode>,
}

impl TestNode {
    fn leaf(test: &TestResult) -> Self {
        Self {
            label: leaf_name(&test.name).to_string(),
            secs: test.secs,
            failed: !test.ok,
            children: Vec::new(),
        }
    }

    fn into_doc(self, depth: usize) -> doc::Node {
        let cut = depth <= 1 && !self.children.is_empty();
        let hidden = if cut { test_count(&self) } else { 0 };
        let children = if cut {
            Vec::new()
        } else {
            self.children
                .into_iter()
                .map(|child| child.into_doc(depth - 1))
                .collect()
        };
        let mut notes = Vec::new();
        if self.failed {
            notes.push("fail".to_string());
        }
        if cut {
            notes.push(format!("… {hidden} tests"));
        }
        doc::Node {
            label: self.label,
            value: human(self.secs),
            note: (!notes.is_empty()).then(|| notes.join(", ")),
            children,
        }
    }
}

fn test_count(node: &TestNode) -> usize {
    if node.children.is_empty() {
        1
    } else {
        node.children.iter().map(test_count).sum()
    }
}

/// A crate or module node: children slowest-first, time summed from them,
/// failures propagated up.
fn branch(label: String, mut children: Vec<TestNode>) -> TestNode {
    children.sort_by(|a, b| b.secs.total_cmp(&a.secs));
    TestNode {
        label,
        secs: children.iter().map(|c| c.secs).sum(),
        failed: children.iter().any(|c| c.failed),
        children,
    }
}

/// Groups flat test timings into a crate → module → test tree, slowest first at
/// every level. Tests outside any module hang directly off their crate.
fn test_tree(tests: &[TestResult]) -> Vec<TestNode> {
    // (crate, module) → tests; the map keys keep crates and modules sorted.
    let grouped: BTreeMap<(&str, &str), Vec<&TestResult>> =
        tests.iter().fold(BTreeMap::new(), |mut grouped, test| {
            grouped
                .entry((test.crate_name.as_ref(), test_module(&test.name)))
                .or_default()
                .push(test);
            grouped
        });

    let by_crate: BTreeMap<&str, Vec<TestNode>> = grouped.into_iter().fold(
        BTreeMap::new(),
        |mut by_crate, ((crate_name, module), tests)| {
            let children = by_crate.entry(crate_name).or_default();
            match module {
                "" => children.extend(tests.iter().copied().map(TestNode::leaf)),
                _ => children.push(branch(
                    module.to_string(),
                    tests.iter().copied().map(TestNode::leaf).collect(),
                )),
            }
            by_crate
        },
    );

    let mut roots: Vec<TestNode> = by_crate
        .into_iter()
        .map(|(crate_name, children)| branch(crate_name.to_string(), children))
        .collect();
    roots.sort_by(|a, b| b.secs.total_cmp(&a.secs));
    roots
}

/// The module path of a nextest test name: "mod::sub::test" → "mod::sub", and
/// "" for tests outside any module.
fn test_module(name: &str) -> &str {
    name.rsplit_once("::").map_or("", |(module, _)| module)
}

/// The bare test name: "mod::sub::test" → "test".
fn leaf_name(name: &str) -> &str {
    name.rsplit_once("::").map_or(name, |(_, test)| test)
}

/// The `take` slowest tests, flat: the outlier view over the full tree.
fn slowest_tests(tests: &[TestResult], take: usize) -> doc::Table {
    let mut table = doc::Table::new(vec![
        doc::Column::new("time", doc::Align::Right),
        doc::Column::new("test", doc::Align::Left),
        doc::Column::new("crate", doc::Align::Left),
        doc::Column::new("status", doc::Align::Left),
    ]);
    let mut by_duration_desc: Vec<&TestResult> = tests.iter().collect();
    by_duration_desc.sort_by(|a, b| b.secs.total_cmp(&a.secs));
    for t in by_duration_desc.into_iter().take(take) {
        table.row(vec![
            human(t.secs),
            t.name.clone(),
            t.crate_name.to_string(),
            if t.ok { String::new() } else { "fail".into() },
        ]);
    }
    table
}

fn main() -> Result<()> {
    let args = Args::parse();
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to run cargo metadata")?;
    let root = metadata.workspace_root.clone().into_std_path_buf();
    std::env::set_current_dir(&root)
        .with_context(|| format!("failed to cd to {}", root.display()))?;

    let selection = Selection::from(&metadata, &args.packages)?;
    let results_dir = PathBuf::from("target/crate-timing");
    let logs_dir = results_dir.join("logs");
    let junit_dir = results_dir.join("junit");
    fs::create_dir_all(&logs_dir).context("failed to create the logs directory")?;
    fs::create_dir_all(&junit_dir).context("failed to create the junit directory")?;

    let mut times: Times = selection
        .crates
        .iter()
        .chain(&selection.bench_crates)
        .map(|c| (Rc::clone(c), CrateTimes::default()))
        .collect();
    let mut tests = Vec::new();

    println!("== Building test binaries (untimed)");
    for c in &selection.crates {
        let mut cmd = nextest(c);
        cmd.args(["--profile", "ci", "--no-run"]);
        with_own_features(&mut cmd, c);
        build(&mut cmd, &logs_dir.join(format!("{}-build.log", c)), c)?;
    }

    println!("== Timing tests");
    for c in &selection.crates {
        let mut cmd = nextest(c);
        cmd.args([
            "--profile",
            "ci",
            "--no-tests=pass",
            "-E",
            "!test(tracing_integration_tests::)",
        ]);
        with_own_features(&mut cmd, c);
        measure(&mut times, Phase::Tests, c, &mut cmd, &logs_dir)?;
        tests.extend(junit_results(&junit_dir, format!("{}.xml", c), c));

        if TRACING_CRATES.contains(&c.as_ref()) {
            let mut cmd = nextest(c);
            cmd.args([
                "--profile",
                "ci",
                "--test-threads",
                "1",
                "--no-tests=pass",
                "-E",
                "test(tracing_integration_tests::)",
            ]);
            measure(&mut times, Phase::Tracing, c, &mut cmd, &logs_dir)?;
            tests.extend(junit_results(&junit_dir, format!("{}-tracing.xml", c), c));
        }

        if selection.doc_crates.contains(c) {
            let mut cmd = cargo();
            cmd.args(["test", "-p", c, "--doc", "--quiet"]);
            measure(&mut times, Phase::Doc, c, &mut cmd, &logs_dir)?;
        }
    }

    println!("== Timing benchmarks");
    for c in &selection.bench_crates {
        let mut build_cmd = bench(c);
        build_cmd.arg("--no-run");
        build(
            &mut build_cmd,
            &logs_dir.join(format!("{}-bench-build.log", c)),
            c,
        )?;

        // Criterion rewrites `*/new/estimates.json` per measured variant; 1s of slack
        // absorbs coarse filesystem timestamps.
        let since = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .context("system clock is before the epoch")?;
        let mut cmd = bench(c);
        cmd.arg("--").args(CRITERION_ARGS);
        measure(&mut times, Phase::Bench, c, &mut cmd, &logs_dir)?;
        times
            .get_mut(c.as_ref())
            .expect("crate outside the selection")
            .bench_fns = bench_functions_since(Path::new("target/criterion"), since);
    }

    let markdown = doc::render_markdown(&report_docs(&times, &tests, &args));
    fs::write(results_dir.join("results.md"), &markdown).context("failed to write results.md")?;
    print!("{markdown}");
    Ok(())
}
