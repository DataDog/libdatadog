// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Controller/worker protocol.
//!
//! The controller runs the scenario in a baseline worker (no failure) to get its allocation
//! trace, checks it is stable, then runs one fresh worker process per ordinal and mode, and
//! classifies each run.
//!
//! A worker is this test binary started as `<exe> __worker <scenario> <mode> <ordinal>`. It reads
//! the baseline trace on stdin, runs the scenario inside a scope, then prints its trace and
//! result on stdout.

use crate::allocator::{self, Domain, Event, Operation, Policy, Scope};
use crate::scenarios::{self, ScenarioError};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const WORKER_ARG: &str = "__worker";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Baseline,
    FailOnce,
    FailFrom,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Baseline => "baseline",
            Mode::FailOnce => "fail-once",
            Mode::FailFrom => "fail-from",
        }
    }

    fn parse(s: &str) -> Option<Mode> {
        [Mode::Baseline, Mode::FailOnce, Mode::FailFrom]
            .into_iter()
            .find(|m| m.as_str() == s)
    }

    fn policy(self, ordinal: u64) -> Policy {
        match self {
            Mode::Baseline => Policy::Observe,
            Mode::FailOnce => Policy::FailOnceAt(ordinal),
            Mode::FailFrom => Policy::FailFrom(ordinal),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The API returned an allocation error.
    OomError,
    /// The API succeeded despite the failed allocation.
    SuccessFallback,
    Panic,
    Abort,
    Timeout,
    /// The event prefix differs from the baseline, or the ordinal was not reached.
    Nondeterministic,
    /// Any other abnormal exit (signal, unexpected exit code).
    Crash,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::OomError => "oom-error",
            Outcome::SuccessFallback => "success-fallback",
            Outcome::Panic => "panic",
            Outcome::Abort => "abort",
            Outcome::Timeout => "timeout",
            Outcome::Nondeterministic => "nondeterministic",
            Outcome::Crash => "crash",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker side
// ---------------------------------------------------------------------------------------------

static PANICKED: AtomicBool = AtomicBool::new(false);

fn format_event(e: &Event) -> String {
    format!(
        "event {} {:?} {:?} {} {} {}",
        e.ordinal, e.domain, e.operation, e.size, e.align, e.thread
    )
}

fn parse_event(line: &str) -> Option<Event> {
    let mut it = line.strip_prefix("event ")?.split(' ');
    let ordinal = it.next()?.parse().ok()?;
    let domain = match it.next()? {
        "Global" => Domain::Global,
        _ => return None,
    };
    let operation = match it.next()? {
        "Alloc" => Operation::Alloc,
        "AllocZeroed" => Operation::AllocZeroed,
        "Realloc" => Operation::Realloc,
        _ => return None,
    };
    Some(Event {
        ordinal,
        domain,
        operation,
        size: it.next()?.parse().ok()?,
        align: it.next()?.parse().ok()?,
        thread: it.next()?.parse().ok()?,
    })
}

/// Workers abort on purpose: do not write a core file for each of them.
fn disable_core_dumps() {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid `rlimit` for the duration of the call.
        unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) };
    }
}

/// Entry point of a worker process. Exits the process.
pub fn worker_main(args: &[String]) -> ! {
    let (Some(name), Some(mode), Some(ordinal)) = (
        args.first(),
        args.get(1).and_then(|m| Mode::parse(m)),
        args.get(2).and_then(|n| n.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: {WORKER_ARG} <scenario> <mode> <ordinal>");
        std::process::exit(2);
    };
    let Some(scenario) = scenarios::find(name) else {
        eprintln!("unknown scenario {name}");
        std::process::exit(2);
    };
    disable_core_dumps();

    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("cannot read the expected trace: {e}");
        std::process::exit(2);
    }
    let expected: Vec<Event> = input.lines().filter_map(parse_event).collect();

    // The hook writes a fixed marker without allocating, so it still works when every
    // allocation fails.
    std::panic::set_hook(Box::new(|_| {
        PANICKED.store(true, Ordering::Relaxed);
        let _ = std::io::stderr().write_all(b"alloc-chaos: panic\n");
    }));

    allocator::reset();
    if mode != Mode::Baseline
        && let Err(e) = allocator::set_expected(&expected)
    {
        eprintln!("{e}");
        std::process::exit(2);
    }

    let mut fixture = (scenario.setup)();
    let result = {
        let _scope = Scope::enter(mode.policy(ordinal));
        (scenario.run)(&mut fixture)
    };
    drop(fixture);

    let trace = match allocator::recorded() {
        Ok(trace) => trace,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut out = String::new();
    for e in &trace {
        let _ = writeln!(out, "{}", format_event(e));
    }
    let result = match result {
        Ok(()) => "ok",
        Err(ScenarioError::Oom) => "oom-error",
    };
    let _ = writeln!(out, "result {result}");
    let _ = writeln!(out, "injected {}", allocator::injected());
    if let Some(at) = allocator::diverged_at() {
        let _ = writeln!(out, "diverged {at}");
    }
    let _ = writeln!(out, "panicked {}", PANICKED.load(Ordering::Relaxed));
    let _ = std::io::stdout().write_all(out.as_bytes());
    std::process::exit(0);
}

// ---------------------------------------------------------------------------------------------
// Controller side
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct WorkerRun {
    pub outcome: Outcome,
    pub trace: Vec<Event>,
    /// Exit status, signal, or the reason of the classification.
    pub detail: String,
    pub stderr: String,
}

fn timeout() -> Duration {
    let secs = std::env::var("ALLOC_CHAOS_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs)
}

fn is_abort(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(libc::SIGABRT)
    }
    #[cfg(windows)]
    {
        // `abort()` from the CRT exits with 3; Rust's `process::abort` uses `__fastfail`
        // (STATUS_STACK_BUFFER_OVERRUN).
        matches!(status.code(), Some(3) | Some(-1073740791))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = status;
        false
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        s
    })
}

pub fn run_worker(
    scenario: &str,
    mode: Mode,
    ordinal: u64,
    baseline: &[Event],
) -> Result<WorkerRun, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut child = Command::new(exe)
        .args([WORKER_ARG, scenario, mode.as_str(), &ordinal.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start the worker: {e}"))?;

    let mut input = String::new();
    for e in baseline {
        let _ = writeln!(input, "{}", format_event(e));
    }
    let mut stdin = child.stdin.take().ok_or("no worker stdin")?;
    let stdout = spawn_reader(child.stdout.take().ok_or("no worker stdout")?);
    let stderr = spawn_reader(child.stderr.take().ok_or("no worker stderr")?);
    // The worker may die before reading its input: ignore a broken pipe.
    let _ = stdin.write_all(input.as_bytes());
    drop(stdin);

    let deadline = Instant::now() + timeout();
    let status = loop {
        match child.try_wait().map_err(|e| format!("wait: {e}"))? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    let trace: Vec<Event> = stdout.lines().filter_map(parse_event).collect();
    let field = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
            .map(str::to_owned)
    };

    let (outcome, detail) = match status {
        None => (Outcome::Timeout, format!("killed after {:?}", timeout())),
        Some(status) if status.success() => {
            let panicked = field("panicked").as_deref() == Some("true");
            let injected = field("injected")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let result = field("result").unwrap_or_default();
            if let Some(at) = field("diverged") {
                (
                    Outcome::Nondeterministic,
                    format!("event {at} differs from the baseline"),
                )
            } else if mode != Mode::Baseline && injected == 0 {
                (
                    Outcome::Nondeterministic,
                    format!("ordinal {ordinal} not reached"),
                )
            } else if panicked {
                (Outcome::Panic, format!("panic caught, result {result}"))
            } else if result == "oom-error" {
                (Outcome::OomError, String::new())
            } else if result == "ok" {
                (Outcome::SuccessFallback, String::new())
            } else {
                (Outcome::Crash, format!("no result line, {status}"))
            }
        }
        Some(status) if stderr.contains("alloc-chaos: panic") => {
            (Outcome::Panic, status.to_string())
        }
        Some(status) if is_abort(&status) => (Outcome::Abort, status.to_string()),
        Some(status) => (Outcome::Crash, status.to_string()),
    };
    Ok(WorkerRun {
        outcome,
        trace,
        detail,
        stderr,
    })
}

#[derive(Debug)]
pub struct Finding {
    pub mode: Mode,
    pub ordinal: u64,
    pub event: Event,
    pub run: WorkerRun,
}

#[derive(Debug)]
pub struct Report {
    pub scenario: &'static str,
    pub baseline: Vec<Event>,
    pub runs: Vec<Finding>,
}

impl Report {
    pub fn outcomes(&self, mode: Mode) -> Vec<Outcome> {
        self.runs
            .iter()
            .filter(|r| r.mode == mode)
            .map(|r| r.run.outcome)
            .collect()
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "## {}\n", self.scenario);
        let _ = writeln!(s, "Baseline: {} events.\n", self.baseline.len());
        let _ = writeln!(
            s,
            "| mode | ordinal | operation | size | align | thread | outcome | detail |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|");
        for f in &self.runs {
            let _ = writeln!(
                s,
                "| {} | {} | {:?} | {} | {} | {} | {} | {} |",
                f.mode.as_str(),
                f.ordinal,
                f.event.operation,
                f.event.size,
                f.event.align,
                f.event.thread,
                f.run.outcome.as_str(),
                f.run.detail
            );
        }
        s
    }
}

/// Runs the baseline twice (it must succeed and be stable), then one worker per ordinal and
/// mode. Returns `Err` only for harness errors.
pub fn enumerate(scenario: &'static str) -> Result<Report, String> {
    let baseline = run_worker(scenario, Mode::Baseline, 0, &[])?;
    if baseline.outcome != Outcome::SuccessFallback {
        return Err(format!(
            "{scenario}: baseline failed: {} {}\n{}",
            baseline.outcome.as_str(),
            baseline.detail,
            baseline.stderr
        ));
    }
    let again = run_worker(scenario, Mode::Baseline, 0, &[])?;
    let stable = again.trace.len() == baseline.trace.len()
        && again
            .trace
            .iter()
            .zip(&baseline.trace)
            .all(|(a, b)| a.same_request(b));
    if !stable {
        return Err(format!(
            "{scenario}: baseline trace is not stable ({} then {} events)",
            baseline.trace.len(),
            again.trace.len()
        ));
    }
    if let Some(e) = baseline
        .trace
        .iter()
        .find(|e| e.thread != baseline.trace[0].thread)
    {
        return Err(format!(
            "{scenario}: baseline has events on another thread: {e:?}"
        ));
    }

    let mut runs = Vec::new();
    for event in &baseline.trace {
        for mode in [Mode::FailOnce, Mode::FailFrom] {
            let run = run_worker(scenario, mode, event.ordinal, &baseline.trace)?;
            runs.push(Finding {
                mode,
                ordinal: event.ordinal,
                event: *event,
                run,
            });
        }
    }
    let report = Report {
        scenario,
        baseline: baseline.trace,
        runs,
    };
    write_report(&report);
    Ok(report)
}

fn write_report(report: &Report) {
    let markdown = report.to_markdown();
    println!("{markdown}");
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("allocation_failure");
    let path = dir.join(format!("{}.md", report.scenario));
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, markdown.as_bytes()))
    {
        eprintln!("cannot write {}: {e}", path.display());
    } else {
        println!("report: {}", path.display());
    }
}
