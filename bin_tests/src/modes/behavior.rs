// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
use anyhow::{Context, Result};
use libdd_crashtracker::CrashtrackerConfiguration;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::modes::unix::*;
use nix::sys::socket;
use std::os::unix::io::AsRawFd;

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

pub fn file_write_msg(filepath: &Path, contents: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(filepath)
        .with_context(|| format!("Failed to open file: {}", filepath.display()))?;

    file.write_all(contents.as_bytes())
        .with_context(|| format!("Failed to write to file: {}", filepath.display()))?;

    Ok(())
}

pub fn atom_to_clone<T: Clone>(atom: &AtomicPtr<T>) -> Result<T> {
    let ptr = atom.load(Ordering::SeqCst);
    anyhow::ensure!(!ptr.is_null(), "Pointer was null");

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

// This helper function is used to trigger a SIGPIPE signal. This is useful to
// verify that the crashtracker correctly suppresses the SIGPIPE signal while it
// emitts information to the collector, and that the SIGPIPE signal can be emitted
// and used normally afterwards, as tested in the following tests:
// - test_001_sigpipe
// - test_005_sigpipe_sigstack
pub fn trigger_sigpipe() -> Result<()> {
    let (reader_fd, writer_fd) = socket::socketpair(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        None,
        socket::SockFlag::empty(),
    )?;
    drop(reader_fd);

    let writer_raw_fd = writer_fd.as_raw_fd();
    let write_result =
        unsafe { libc::write(writer_raw_fd, b"Hello".as_ptr() as *const libc::c_void, 5) };

    if write_result != -1 {
        anyhow::bail!("Expected write to fail with SIGPIPE, but it succeeded");
    }

    Ok(())
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
        "fork" => Box::new(test_008_fork::Test),
        "prechain_abort" => Box::new(test_009_prechain_with_abort::Test),
        _ => panic!("Unknown mode: {mode_str}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static SIGPIPE_CAUGHT: AtomicBool = AtomicBool::new(false);

    extern "C" fn sigpipe_handler(_: libc::c_int) {
        SIGPIPE_CAUGHT.store(true, Ordering::SeqCst);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_trigger_sigpipe() {
        use std::mem;
        use std::thread;

        let result = thread::spawn(|| {
            SIGPIPE_CAUGHT.store(false, Ordering::SeqCst);

            let mut sigset: libc::sigset_t = unsafe { mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut sigset);
            }

            let sigpipe_action = libc::sigaction {
                sa_sigaction: sigpipe_handler as usize,
                sa_mask: sigset,
                sa_flags: libc::SA_RESTART | libc::SA_SIGINFO,
                #[cfg(target_os = "linux")]
                sa_restorer: None,
            };

            let mut old_action: libc::sigaction = unsafe { mem::zeroed() };
            let install_result =
                unsafe { libc::sigaction(libc::SIGPIPE, &sigpipe_action, &mut old_action) };

            if install_result != 0 {
                return Err("Failed to set up SIGPIPE handler".to_string());
            }

            let trigger_result = trigger_sigpipe();

            thread::sleep(std::time::Duration::from_millis(10));

            let handler_called = SIGPIPE_CAUGHT.load(Ordering::SeqCst);

            unsafe {
                libc::sigaction(libc::SIGPIPE, &old_action, std::ptr::null_mut());
            }

            if trigger_result.is_err() {
                return Err(format!(
                    "trigger_sigpipe should succeed: {:?}",
                    trigger_result
                ));
            }

            if !handler_called {
                return Err("SIGPIPE handler should have been called".to_string());
            }

            Ok(())
        })
        .join();

        match result {
            Ok(Ok(())) => {} // Test passed
            Ok(Err(e)) => panic!("{}", e),
            Err(_) => panic!("Thread panicked during SIGPIPE test"),
        }
    }
}
