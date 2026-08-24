// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shows why the `sigchld` bin tests must keep their SIGCHLD handler async-signal-safe.
//!
//! `test_002_sigchld`, `test_003_sigchld_with_exec` and `test_006_sigchld_sigstack` used to install
//! a handler that allocated: `atom_to_clone` clones a `PathBuf`, and `file_write_msg` opens and
//! writes through Rust's `File`. Both reach malloc.
//!
//! A child can exit before its parent has finished returning from `fork()`, so SIGCHLD is routinely
//! delivered while libmalloc holds the lock its atfork hooks take. A handler that allocates there
//! re-enters that lock: macOS kills the process outright ("BUG IN CLIENT OF LIBPLATFORM: Trying to
//! recursively lock an os_unfair_lock"), and elsewhere it can deadlock.
//!
//! This runs that old handler against the `handler_write_msg` the tests now use, each in its own
//! child process, and reports how each fares:
//!
//! ```not_rust
//! cargo run -p bin_tests --example sigchld_async_signal_safety [iterations]
//! ```
//!
//! The window only opens under CPU contention, so this saturates the machine while each variant
//! runs. Expect it to peg every core for the duration.

#[cfg(not(unix))]
fn main() {
    eprintln!("This reproducer is unix-only");
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix::main()
}

#[cfg(unix)]
mod unix {
    use anyhow::{Context, Result};
    use bin_tests::modes::behavior::{
        atom_to_clone, file_write_msg, handler_write_msg, set_atomic, set_handler_path,
    };
    use std::ffi::CString;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus};
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    const DEFAULT_ITERATIONS: usize = 5_000;
    const CHILD_TIMEOUT: Duration = Duration::from_secs(120);
    const PROGRESS_FILE: &str = "progress";
    const CHECK_FILE: &str = "check";

    /// The handler the sigchld bin tests used to install, built from the same helpers it used.
    static ALLOCATING_OUTPUT: AtomicPtr<PathBuf> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn allocating_handler(_: libc::c_int) {
        let ofile = match atom_to_clone(&ALLOCATING_OUTPUT) {
            Ok(ofile) => ofile,
            _ => return,
        };
        file_write_msg(&ofile, "O").ok();
    }

    /// The handler those tests install today.
    static SAFE_OUTPUT: AtomicPtr<CString> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn safe_handler(_: libc::c_int) {
        handler_write_msg(&SAFE_OUTPUT, b"O");
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Variant {
        Allocating,
        Safe,
    }

    impl Variant {
        fn as_str(self) -> &'static str {
            match self {
                Variant::Allocating => "allocating",
                Variant::Safe => "safe",
            }
        }

        fn parse(s: &str) -> Result<Self> {
            match s {
                "allocating" => Ok(Variant::Allocating),
                "safe" => Ok(Variant::Safe),
                other => anyhow::bail!("Unknown variant: {other}"),
            }
        }
    }

    pub fn main() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();

        // Re-invocation of ourselves: run one variant's loop in this process, since reproducing the
        // bug means being killed by it.
        if args.get(1).map(String::as_str) == Some("--run-variant") {
            let variant = Variant::parse(args.get(2).context("Missing variant")?)?;
            let iterations: usize = args.get(3).context("Missing iterations")?.parse()?;
            let dir = Path::new(args.get(4).context("Missing output directory")?);
            return run_variant(variant, iterations, dir);
        }

        let iterations: usize = match args.get(1) {
            Some(arg) => arg.parse().context("Iterations must be a number")?,
            None => DEFAULT_ITERATIONS,
        };

        println!("Forking {iterations} times per variant, one child process each.\n");

        let mut reproduced = false;
        for variant in [Variant::Allocating, Variant::Safe] {
            let (outcome, reached) = spawn_variant(variant, iterations)?;
            let label = variant.as_str();
            match outcome {
                Outcome::Completed => {
                    println!("  {label:<11} completed all {iterations} iterations");
                }
                Outcome::Killed(signal) => {
                    reproduced |= variant == Variant::Allocating;
                    println!("  {label:<11} killed by signal {signal} after ~{reached} iterations");
                }
                Outcome::Failed(status) => {
                    println!("  {label:<11} exited with {status} after ~{reached} iterations");
                }
                Outcome::Hung => {
                    reproduced |= variant == Variant::Allocating;
                    println!(
                        "  {label:<11} hung after ~{reached} iterations (deadlocked in malloc)"
                    );
                }
            }
        }

        println!();
        if reproduced {
            println!("Reproduced: the tests' allocating handler is fatal, the safe one is not.");
            if cfg!(target_os = "macos") {
                println!(
                    "A matching report naming os_unfair_lock should be in \
                     ~/Library/Logs/DiagnosticReports."
                );
            }
        } else {
            println!(
                "Not reproduced. The window needs CPU contention: retry with more iterations, \
                 or while the machine is busy."
            );
        }

        Ok(())
    }

    enum Outcome {
        Completed,
        Killed(i32),
        Failed(ExitStatus),
        Hung,
    }

    /// Runs one variant in a child process, returning its fate and roughly how far it got.
    fn spawn_variant(variant: Variant, iterations: usize) -> Result<(Outcome, usize)> {
        let tmpdir = tempfile::TempDir::new().context("Failed to create temporary directory")?;
        let exe = std::env::current_exe().context("Failed to find our own binary")?;

        let load = CpuLoad::start();

        let mut child = Command::new(exe)
            .arg("--run-variant")
            .arg(variant.as_str())
            .arg(iterations.to_string())
            .arg(tmpdir.path())
            .spawn()
            .context("Failed to spawn variant process")?;

        let deadline = Instant::now() + CHILD_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        load.stop();

        let reached = std::fs::read_to_string(tmpdir.path().join(PROGRESS_FILE))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        let outcome = match status {
            None => Outcome::Hung,
            Some(status) if status.success() => Outcome::Completed,
            Some(status) => match status.signal() {
                Some(signal) => Outcome::Killed(signal),
                None => Outcome::Failed(status),
            },
        };

        Ok((outcome, reached))
    }

    /// Forks repeatedly so that each child's SIGCHLD races the parent's next `fork()`.
    fn run_variant(variant: Variant, iterations: usize, dir: &Path) -> Result<()> {
        let check_path = dir.join(CHECK_FILE);
        match variant {
            Variant::Allocating => {
                set_atomic(&ALLOCATING_OUTPUT, check_path);
                install_handler(allocating_handler)?;
            }
            Variant::Safe => {
                set_handler_path(&SAFE_OUTPUT, &check_path)?;
                install_handler(safe_handler)?;
            }
        }

        let progress_path = dir.join(PROGRESS_FILE);
        for i in 0..iterations {
            std::fs::write(&progress_path, i.to_string())?;

            match unsafe { libc::fork() } {
                -1 => anyhow::bail!("Failed to fork"),
                0 => unsafe { libc::_exit(0) },
                _ => loop {
                    let mut status: libc::c_int = 0;
                    if -1 == unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) } {
                        break;
                    }
                },
            }
        }

        std::fs::write(&progress_path, iterations.to_string())?;
        Ok(())
    }

    /// Saturates the CPUs for as long as it is alive.
    ///
    /// Without contention the parent almost always finishes `fork()` before its child exits, and
    /// the handler never lands on the malloc lock.
    struct CpuLoad {
        stop: Arc<AtomicBool>,
        spinners: Vec<JoinHandle<()>>,
    }

    impl CpuLoad {
        fn start() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);

            let spinners = (0..threads)
                .map(|_| {
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || {
                        let mut x: u64 = 0;
                        while !stop.load(Ordering::Relaxed) {
                            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                            std::hint::black_box(x);
                        }
                    })
                })
                .collect();

            Self { stop, spinners }
        }

        fn stop(self) {
            self.stop.store(true, Ordering::Relaxed);
            for spinner in self.spinners {
                let _ = spinner.join();
            }
        }
    }

    fn install_handler(handler: extern "C" fn(libc::c_int)) -> Result<()> {
        let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut sigset) };

        let action = libc::sigaction {
            sa_sigaction: handler as *const () as usize,
            sa_mask: sigset,
            sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
            #[cfg(target_os = "linux")]
            sa_restorer: None,
        };

        anyhow::ensure!(
            0 == unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
            "Failed to set up SIGCHLD handler"
        );
        Ok(())
    }
}
