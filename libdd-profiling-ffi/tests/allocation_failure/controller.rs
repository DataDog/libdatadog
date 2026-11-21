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
/// Directory of the per-worker LLVM profiles (`discovery/discovery.py coverage`).
const PROFILE_DIR_ENV: &str = "ALLOC_CHAOS_PROFILE_DIR";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Baseline,
    FailOnce,
    FailFrom,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
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
    /// The API returned, but its handles are not in their documented state afterwards.
    ContractViolation,
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
            Outcome::ContractViolation => "contract-violation",
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
        "ArenaRequest" => Domain::ArenaRequest,
        "ArenaBacking" => Domain::ArenaBacking,
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

    // The marker is written without allocating, so it still works when every allocation fails.
    // The previous hook prints the `RUST_BACKTRACE` trace.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICKED.store(true, Ordering::Relaxed);
        let _ = std::io::stderr().write_all(b"alloc-chaos: panic\n");
        previous_hook(info);
    }));

    allocator::reset();
    allocator::write_stack_on_first_failure();
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
    let (other_threads, other_overflow) = allocator::other_threads();
    let contract = scenario
        .check
        .map(|check| check(&mut fixture, result.is_err()));
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
        Err(ScenarioError::Rejected) => {
            eprintln!("the call rejected its input");
            std::process::exit(2);
        }
    };
    let _ = writeln!(out, "result {result}");
    match contract {
        None | Some(Ok(())) => {}
        Some(Err(violation)) => {
            let _ = writeln!(out, "contract {}", violation.replace('\n', " "));
        }
    }
    for (thread, count, bytes) in other_threads {
        let _ = writeln!(out, "other-thread {thread} {count} {bytes}");
    }
    if other_overflow > 0 {
        let _ = writeln!(out, "other-thread-overflow {other_overflow}");
    }
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
    /// Allocations of other threads during the scope: `(thread, count, bytes)`.
    pub other_threads: Vec<(u32, u64, u64)>,
    /// Other-thread allocations that could not be attributed to a thread.
    pub other_threads_unattributed: u64,
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
    let mut command = Command::new(exe);
    if let Some(dir) = std::env::var_os(PROFILE_DIR_ENV) {
        // Coverage builds only: one LLVM profile per worker. A worker that aborts writes none.
        let file = std::path::Path::new(&dir)
            .join(scenario)
            .join(format!("{}-{ordinal}-%p.profraw", mode.as_str()));
        // The profile runtime of the controller sets this marker, and a process that inherits it
        // skips its own profile setup.
        command
            .env("LLVM_PROFILE_FILE", file)
            .env_remove("__LLVM_PROFILE_RT_INIT_ONCE");
    }
    let mut child = command
        .args([WORKER_ARG, scenario, mode.as_str(), &ordinal.to_string()])
        .env("RUST_BACKTRACE", "1")
        // Panic backtraces only. A captured `std::backtrace::Backtrace` (anyhow errors) allocates
        // under the std backtrace lock, and a failure there deadlocks the abort path. Production
        // leaves capture off.
        .env("RUST_LIB_BACKTRACE", "0")
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
            } else if let Some(violation) = field("contract") {
                (
                    Outcome::ContractViolation,
                    format!("result {result}: {violation}"),
                )
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
    let other_threads = stdout
        .lines()
        .filter_map(|line| {
            let mut it = line.strip_prefix("other-thread ")?.split(' ');
            Some((
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
            ))
        })
        .collect();
    let other_threads_unattributed = field("other-thread-overflow")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok(WorkerRun {
        outcome,
        trace,
        detail,
        stderr,
        other_threads,
        other_threads_unattributed,
    })
}

#[derive(Debug)]
pub struct Finding {
    pub mode: Mode,
    pub ordinal: u64,
    pub event: Event,
    pub run: WorkerRun,
    /// First stack frame in libdatadog code, from a debugger stopped on the injection hook.
    pub call_site: Option<String>,
    /// Frame lines of that stack.
    pub allocation_stack: Option<String>,
}

#[derive(Debug)]
pub struct Report {
    pub scenario: &'static str,
    pub baseline: Vec<Event>,
    /// Allocations of other threads during the baseline scope: `(thread, count, bytes)`.
    /// They are reported, not enumerated.
    pub other_threads: Vec<(u32, u64, u64)>,
    pub other_threads_unattributed: u64,
    /// Set for an I/O-bound scenario whose two baselines differ.
    pub unstable_baseline: Option<String>,
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
        if let Some(reason) = &self.unstable_baseline {
            let _ = writeln!(
                s,
                "I/O-bound scenario: {reason}. Runs past the common prefix can be \
                 nondeterministic.\n"
            );
        }
        if !self.other_threads.is_empty() || self.other_threads_unattributed > 0 {
            let _ = writeln!(
                s,
                "Other threads during the baseline (not enumerated, never failed):\n"
            );
            for (thread, count, bytes) in &self.other_threads {
                let _ = writeln!(s, "- thread {thread}: {count} allocations, {bytes} bytes");
            }
            if self.other_threads_unattributed > 0 {
                let _ = writeln!(
                    s,
                    "- unattributed: {} allocations",
                    self.other_threads_unattributed
                );
            }
            let _ = writeln!(s);
        }
        let _ = writeln!(
            s,
            "| mode | ordinal | domain | operation | size | align | thread | outcome | detail |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|");
        for f in &self.runs {
            let _ = writeln!(
                s,
                "| {} | {} | {:?} | {:?} | {} | {} | {} | {} | {} |",
                f.mode.as_str(),
                f.ordinal,
                f.event.domain,
                f.event.operation,
                f.event.size,
                f.event.align,
                f.event.thread,
                f.run.outcome.as_str(),
                f.run.detail
            );
        }
        for f in &self.runs {
            if f.call_site.is_none() && f.allocation_stack.is_none() {
                continue;
            }
            let _ = writeln!(
                s,
                "\n### {} #{} {}\n\nCall site: {}\n",
                f.mode.as_str(),
                f.ordinal,
                f.run.outcome.as_str(),
                f.call_site.as_deref().unwrap_or("(none)")
            );
            if let Some(stack) = &f.allocation_stack {
                let _ = writeln!(s, "```\n{stack}\n```");
            }
            if let Some(panic) = f.run.stderr.split("stack backtrace:").nth(1) {
                let _ = writeln!(s, "\nPanic backtrace:\n```\nstack backtrace:{panic}\n```");
            }
        }
        s
    }
}

/// Runs the baseline twice (it must succeed and be stable), then one worker per ordinal and
/// mode. Returns `Err` only for harness errors.
pub fn enumerate(scenario: &'static str) -> Result<Report, String> {
    enumerate_inner(scenario, false, false)
}

/// Like [`enumerate`], then captures the allocation stack of every run that is not `oom-error`.
/// With `io_bound`, the trace depends on I/O timing: an unstable baseline is reported, not a
/// harness error.
pub fn enumerate_with_stacks(scenario: &'static str, io_bound: bool) -> Result<Report, String> {
    enumerate_inner(scenario, true, io_bound)
}

fn enumerate_inner(scenario: &'static str, stacks: bool, io_bound: bool) -> Result<Report, String> {
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
    let unstable_baseline = (!stable).then(|| {
        format!(
            "baseline trace is not stable ({} then {} events)",
            baseline.trace.len(),
            again.trace.len()
        )
    });
    if let Some(reason) = &unstable_baseline
        && !io_bound
    {
        return Err(format!("{scenario}: {reason}"));
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

    let jobs: Vec<(Event, Mode)> = baseline
        .trace
        .iter()
        .flat_map(|event| [(*event, Mode::FailOnce), (*event, Mode::FailFrom)])
        .collect();
    let results = run_parallel(&jobs, |&(event, mode)| {
        run_worker(scenario, mode, event.ordinal, &baseline.trace)
    });
    let mut runs = Vec::with_capacity(jobs.len());
    for ((event, mode), run) in jobs.into_iter().zip(results) {
        runs.push(Finding {
            mode,
            ordinal: event.ordinal,
            event,
            run: run?,
            call_site: None,
            allocation_stack: None,
        });
    }
    if stacks {
        capture_stacks(scenario, &baseline.trace, &mut runs);
    }
    let report = Report {
        scenario,
        baseline: baseline.trace,
        other_threads: baseline.other_threads,
        other_threads_unattributed: baseline.other_threads_unattributed,
        unstable_baseline,
        runs,
    };
    write_report(&report);
    Ok(report)
}

/// Per-scenario reports: `ALLOC_CHAOS_REPORT_DIR`, so two modes can keep separate reports, or
/// `$CARGO_TARGET_TMPDIR/allocation_failure`.
fn report_dir() -> std::path::PathBuf {
    std::env::var_os("ALLOC_CHAOS_REPORT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("allocation_failure")
        })
}

fn write_report(report: &Report) {
    let markdown = report.to_markdown();
    println!("{markdown}");
    let dir = report_dir();
    let path = dir.join(format!("{}.md", report.scenario));
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, markdown.as_bytes()))
    {
        eprintln!("cannot write {}: {e}", path.display());
    } else {
        println!("report: {}", path.display());
    }
}

/// Worker processes started at the same time: `ALLOC_CHAOS_JOBS`, or the available parallelism.
fn jobs() -> usize {
    std::env::var("ALLOC_CHAOS_JOBS")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .max(1)
}

/// Runs `f` on every item on [`jobs`] threads. Results keep the order of `items`.
fn run_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..jobs().min(items.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = f(item);
                    *results[index].lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .expect("every item ran")
        })
        .collect()
}

/// Symbolizes the stack a worker wrote on its first injected failure. The addresses are mapped
/// into this process with the address of `alloc_chaos_injection_hook`: same executable.
fn worker_stack(stderr: &str) -> Option<Vec<String>> {
    let parse = |line: &str, prefix: &str| {
        line.strip_prefix(prefix)
            .and_then(|hex| usize::from_str_radix(hex.trim(), 16).ok())
    };
    let base = stderr
        .lines()
        .find_map(|line| parse(line, allocator::STACK_BASE_PREFIX))?;
    let here = allocator::alloc_chaos_injection_hook as *const () as usize;
    let mut frames = Vec::new();
    for (i, ip) in stderr
        .lines()
        .filter_map(|line| parse(line, allocator::STACK_FRAME_PREFIX))
        .enumerate()
    {
        let addr = ip.wrapping_add(here).wrapping_sub(base);
        // A return address points after the call: look up the call itself.
        let lookup = if i == 0 { addr } else { addr.wrapping_sub(1) };
        let before = frames.len();
        backtrace::resolve(lookup as *mut std::ffi::c_void, |symbol| {
            let name = symbol
                .name()
                .map(|name| format!("{name:#}"))
                .unwrap_or_else(|| "??".to_string());
            let frame = match (symbol.filename(), symbol.lineno()) {
                (Some(file), Some(line)) => {
                    format!("#{} {name} at {}:{line}", frames.len(), file.display())
                }
                _ => format!("#{} {name}", frames.len()),
            };
            frames.push(frame);
        });
        if frames.len() == before {
            frames.push(format!("#{} 0x{addr:x}", frames.len()));
        }
    }
    (!frames.is_empty()).then_some(frames)
}

/// Maximum number of runs replayed under a debugger when their own stack has no owned frame.
const REPLAY_LIMIT: usize = 32;

/// Takes each run's stack from the worker's own output. A run without an owned frame there, or
/// every run with `ALLOC_CHAOS_DEBUGGER=gdb|lldb`, is replayed under a debugger stopped on
/// `alloc_chaos_injection_hook`.
fn capture_stacks(scenario: &str, baseline: &[Event], runs: &mut [Finding]) {
    let force_debugger = std::env::var_os("ALLOC_CHAOS_DEBUGGER").is_some();
    // Some unwinders allocate while walking the stack (notably on macOS). Those allocations also
    // fail in fail-from mode, so reuse fail-once's stack for the same first failed allocation.
    let fail_once_stacks: Vec<_> = runs
        .iter()
        .filter(|finding| finding.mode == Mode::FailOnce)
        .map(|finding| (finding.ordinal, worker_stack(&finding.run.stderr)))
        .collect();
    let mut replay = Vec::new();
    for (index, finding) in runs.iter_mut().enumerate() {
        if matches!(
            finding.run.outcome,
            Outcome::OomError | Outcome::Nondeterministic | Outcome::Timeout
        ) {
            continue;
        }
        if !force_debugger {
            let mut frames = worker_stack(&finding.run.stderr);
            if finding.mode == Mode::FailFrom
                && !frames
                    .as_ref()
                    .is_some_and(|frames| frames.iter().any(|line| is_owned_frame(line)))
            {
                frames = fail_once_stacks
                    .iter()
                    .find(|(ordinal, _)| *ordinal == finding.ordinal)
                    .and_then(|(_, frames)| frames.clone());
            }
            match frames {
                Some(frames) => {
                    finding.call_site = frames.iter().find(|line| is_owned_frame(line)).cloned();
                    finding.allocation_stack = Some(frames.join("\n"));
                }
                None => {
                    finding.allocation_stack = Some("the worker wrote no stack".to_string());
                }
            }
            if finding.call_site.is_some() {
                continue;
            }
        }
        replay.push(index);
    }
    if replay.is_empty() {
        return;
    }
    // A debugger run takes seconds. When symbolization fails for many runs, report the missing
    // call sites rather than replaying for hours.
    if !force_debugger && replay.len() > REPLAY_LIMIT {
        eprintln!(
            "{scenario}: {} runs have no owned frame, more than {REPLAY_LIMIT}: not replayed",
            replay.len()
        );
        return;
    }
    let Some(debugger) = debugger() else {
        eprintln!("{scenario}: no gdb or lldb, allocation stacks were not replayed");
        return;
    };
    let dir = report_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for index in replay {
        let finding = &mut runs[index];
        let input = dir.join(format!(
            "stdin-{}-{}-{}-{}.txt",
            scenario,
            finding.mode.as_str(),
            finding.ordinal,
            std::process::id()
        ));
        let mut text = String::new();
        for event in baseline {
            let _ = writeln!(text, "{}", format_event(event));
        }
        if std::fs::write(&input, text).is_err() {
            continue;
        }
        match debugger_backtrace(debugger, scenario, finding.mode, finding.ordinal, &input) {
            Ok(raw) => {
                let frames = frame_lines(&raw);
                finding.call_site = frames.iter().find(|line| is_owned_frame(line)).cloned();
                finding.allocation_stack = Some(frames.join("\n"));
            }
            Err(error) => finding.allocation_stack = Some(error),
        }
    }
}

/// `ALLOC_CHAOS_DEBUGGER=gdb|lldb` picks one; otherwise gdb, then lldb.
fn debugger() -> Option<&'static str> {
    match std::env::var("ALLOC_CHAOS_DEBUGGER").as_deref() {
        Ok("gdb") => return command_ok("gdb").then_some("gdb"),
        Ok("lldb") => return command_ok("lldb").then_some("lldb"),
        _ => {}
    }
    if command_ok("gdb") {
        Some("gdb")
    } else if command_ok("lldb") {
        Some("lldb")
    } else {
        None
    }
}

fn command_ok(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn debugger_backtrace(
    debugger: &str,
    scenario: &str,
    mode: Mode,
    ordinal: u64,
    input: &std::path::Path,
) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut cmd = Command::new(debugger);
    let ordinal = ordinal.to_string();
    match debugger {
        "gdb" => {
            // `run < file` replaces the inferior arguments, so the redirect goes in `set args`.
            // The executable has to come before `-ex`, or gdb runs those commands too early.
            cmd.arg("-nx")
                .arg("-batch")
                .arg(&exe)
                .arg("-ex")
                .arg("set pagination off")
                .arg("-ex")
                .arg("set confirm off")
                .arg("-ex")
                .arg("set debuginfod enabled off")
                .arg("-ex")
                .arg(format!(
                    "set args {WORKER_ARG} {scenario} {} {ordinal} < \"{}\"",
                    mode.as_str(),
                    input.display()
                ))
                .arg("-ex")
                .arg("break alloc_chaos_injection_hook")
                .arg("-ex")
                .arg("run")
                .arg("-ex")
                .arg("bt 80");
        }
        "lldb" => {
            // The default frame format prints the file name only; owned frames are recognized
            // by their full path.
            cmd.arg("-b")
                .arg("-o")
                .arg("settings set auto-confirm true")
                .arg("-o")
                .arg(
                    "settings set frame-format \"frame #${frame.index}: ${function.name}\
                     { at ${line.file.fullpath}:${line.number}}\\n\"",
                )
                .arg("-o")
                .arg("breakpoint set --name alloc_chaos_injection_hook")
                .arg("-o")
                .arg(format!(
                    "process launch -i {} -- {} {} {} {}",
                    input.display(),
                    WORKER_ARG,
                    scenario,
                    mode.as_str(),
                    ordinal
                ))
                .arg("-o")
                .arg("bt 80")
                .arg("-o")
                .arg("quit")
                .arg(&exe);
        }
        other => return Err(format!("unknown debugger {other}")),
    }
    cmd.env("RUST_BACKTRACE", "1")
        .env("RUST_LIB_BACKTRACE", "0");
    let (status, stdout, stderr) = output_with_timeout(&mut cmd, Duration::from_secs(120))?;
    if status.is_none() {
        return Err("debugger timed out".to_string());
    }
    let combined = format!("{stdout}\n{stderr}");
    if !combined.contains("alloc_chaos_injection_hook") {
        return Err(format!("debugger did not stop on the hook\n{combined}"));
    }
    Ok(combined)
}

fn output_with_timeout(
    cmd: &mut Command,
    limit: Duration,
) -> Result<(Option<ExitStatus>, String, String), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start the debugger: {e}"))?;
    let stdout = spawn_reader(child.stdout.take().ok_or("no debugger stdout")?);
    let stderr = spawn_reader(child.stderr.take().ok_or("no debugger stderr")?);
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait().map_err(|e| format!("wait: {e}"))? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    Ok((
        status,
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    ))
}

fn frame_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let marker = trimmed.trim_start_matches('*').trim();
            if marker.starts_with('#') || marker.starts_with("frame #") {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .take(80)
        .collect()
}

/// A frame in libdatadog code. Runtime frames are skipped even when a generic argument names one
/// of our types, and harness frames are skipped too.
fn is_owned_frame(line: &str) -> bool {
    if line.contains("allocation_failure::") || line.contains("/allocation_failure/") {
        return false;
    }
    if let Some((_, file)) = line.rsplit_once(" at ") {
        // The chaos wrapper is test infrastructure, like this harness. The call site is the
        // production line that asked for the memory.
        if file.contains("libdd-profiling/src/arena.rs") || file.contains("/allocation_failure/") {
            return false;
        }
        return file.contains("libdd-") || file.contains("libdd_");
    }
    line.split(" in ").nth(1).is_some_and(|rest| {
        rest.split('(')
            .next()
            .unwrap_or(rest)
            .trim_start()
            .starts_with("libdd_")
    })
}
