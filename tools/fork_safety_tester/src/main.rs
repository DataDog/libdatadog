use std::{
    env, io,
    process::{Command, ExitStatus, Stdio, self},
    sync::{
        atomic::{AtomicIsize, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
mod args;
mod perform;
mod runner;

use clap::Parser;

use crate::perform::{perform_allocation, perform_dlopen, spawn_continous_action};


extern "C" fn before_fork() {
    eprintln!("forking!");
}

extern "C" fn after_fork_child() {
    eprintln!("forking!");
}

extern "C" fn after_fork_parent() {
    eprintln!("forking!");
}

fn main() {
    let cli = args::Cli::parse_from(std::env::args());

    let res = unsafe {
        nix::libc::pthread_atfork(Some(before_fork), Some(after_fork_child), Some(after_fork_parent))
    };

    if res != 0 {
        panic!("can't install pthread_atfork");
    }
    
    match cli.command {
        args::Commands::ExecuteCmd(e) => {
            for id in 0..e.params.dl_open_threads {
                spawn_continous_action("dlopen", id, 1000, perform_dlopen)
            }
            for id in 0..e.params.malloc_threads {
                spawn_continous_action("allocations", id, 10000, perform_allocation);
            }
            if !e.warmup_time_ms.is_zero() {
                thread::sleep(e.warmup_time_ms);
            }
            let mut joins = vec![];

            let work_remaining = Arc::new(AtomicIsize::new(
                e.num_executions.clamp(0, std::isize::MAX as usize) as isize,
            ));
            let num_failures = Arc::new(AtomicUsize::new(0));
            let num_success = Arc::new(AtomicUsize::new(0));
            let num_errors = Arc::new(AtomicUsize::new(0));

            for _th_id in 0..e.parallel_executions {
                let work_remaining = work_remaining.clone();
                let num_failures = num_failures.clone();
                let num_success = num_success.clone();
                let num_errors = num_errors.clone();

                let command = e.command.clone();
                let args = e.command_args.clone();
                let join = thread::spawn(move || {
                    while work_remaining.fetch_sub(1, Ordering::AcqRel) > 0 {
                        eprint!(".");
                        let res = runner::spawn_subprocess(&command, &args)
                            .unwrap()
                            .join()
                            .unwrap();
                        match res {
                            Ok(e) if e.success() => {
                                num_success.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(_) => {
                                num_failures.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(_) => {
                                num_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
                joins.push(join);
            }

            for th in joins {
                th.join().ok();
            }

            let num_failures = num_failures.load(Ordering::Acquire);
            let num_success = num_success.load(Ordering::Acquire);
            let num_errors = num_errors.load(Ordering::Acquire);

            eprintln!(
                "\nExecutions: {}, success: {}, failures: {}, errors: {}",
                e.num_executions, num_success, num_failures, num_errors
            );
        }
    }
}
