// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collector::counters::emit_counters;
use crate::collector::siginfo_strings;
use crate::collector::spans::emit_spans;
use crate::collector::spans::emit_traces;
use crate::shared::constants::*;
use crate::CrashtrackerConfiguration;
use crate::StacktraceCollection;
use anyhow::Context;
use std::{
    fs::File,
    io::{Read, Write},
};

/// Emit a stacktrace onto the given handle as formatted json.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
/// SIGNAL SAFETY:
///     Getting a backtrace on rust is not guaranteed to be signal safe.
///     https://github.com/rust-lang/backtrace-rs/issues/414
///     Calculating the `ip` of the frames seems safe, but resolving the frames
///     sometimes crashes.
unsafe fn emit_backtrace_by_frames(
    w: &mut impl Write,
    resolve_frames: StacktraceCollection,
) -> anyhow::Result<()> {
    // https://docs.rs/backtrace/latest/backtrace/index.html
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;
    backtrace::trace_unsynchronized(|frame| {
        // Write the values we can get without resolving, since these seem to
        // be crash safe in my experiments.
        write!(w, "{{").unwrap();
        write!(w, "\"ip\": \"{:?}\", ", frame.ip()).unwrap();
        if let Some(module_base_address) = frame.module_base_address() {
            write!(w, "\"module_base_address\": \"{module_base_address:?}\", ",).unwrap();
        }
        write!(w, "\"sp\": \"{:?}\", ", frame.sp()).unwrap();
        write!(w, "\"symbol_address\": \"{:?}\"", frame.symbol_address()).unwrap();
        if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
            write!(w, ", \"names\": [").unwrap();

            let mut first = true;
            // This can give multiple answers in the case of inlined functions
            // https://docs.rs/backtrace/latest/backtrace/fn.resolve.html
            // Store them all into an array of names
            unsafe {
                backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                    if !first {
                        write!(w, ", ").unwrap();
                    }
                    write!(w, "{{").unwrap();
                    let mut comma_needed = false;
                    if let Some(name) = symbol.name() {
                        write!(w, "\"name\": \"{}\"", name).unwrap();
                        comma_needed = true;
                    }
                    if let Some(filename) = symbol.filename() {
                        if comma_needed {
                            write!(w, ", ").unwrap();
                        }
                        write!(w, "\"filename\": {:?}", filename).unwrap();
                        comma_needed = true;
                    }
                    if let Some(colno) = symbol.colno() {
                        if comma_needed {
                            write!(w, ", ").unwrap();
                        }
                        write!(w, "\"colno\": {}", colno).unwrap();
                        comma_needed = true;
                    }

                    if let Some(lineno) = symbol.lineno() {
                        if comma_needed {
                            write!(w, ", ").unwrap();
                        }
                        write!(w, "\"lineno\": {}", lineno).unwrap();
                    }

                    write!(w, "}}").unwrap();

                    first = false;
                });
            }
            write!(w, "]").unwrap();
        }
        writeln!(w, "}}").unwrap();
        true // keep going to the next frame
    });
    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}").unwrap();
    Ok(())
}

pub(crate) fn emit_crashreport(
    pipe: &mut impl Write,
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_string: &str,
    siginfo: libc::siginfo_t,
) -> anyhow::Result<()> {
    emit_metadata(pipe, metadata_string)?;
    emit_config(pipe, config_str)?;
    emit_siginfo(pipe, siginfo)?;
    emit_procinfo(pipe)?;
    pipe.flush()?;
    emit_counters(pipe)?;
    pipe.flush()?;
    emit_spans(pipe)?;
    pipe.flush()?;
    emit_traces(pipe)?;
    pipe.flush()?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // Do this last, so even if it crashes, we still get the other info.
    if config.resolve_frames != StacktraceCollection::Disabled {
        unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames)? };
    }
    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;
    pipe.flush()?;

    Ok(())
}

fn emit_config(w: &mut impl Write, config_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_CONFIG}")?;
    writeln!(w, "{}", config_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_CONFIG}")?;
    Ok(())
}

fn emit_metadata(w: &mut impl Write, metadata_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_METADATA}")?;
    writeln!(w, "{}", metadata_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_METADATA}")?;
    Ok(())
}

fn emit_procinfo(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_PROCINFO}")?;
    let pid = nix::unistd::getpid();
    writeln!(w, "{{\"pid\": {pid} }}")?;
    writeln!(w, "{DD_CRASHTRACK_END_PROCINFO}")?;
    Ok(())
}

#[cfg(target_os = "linux")]
/// `/proc/self/maps` is very useful for debugging, and difficult to get from
/// the child process (permissions issues on Linux).  Emit it directly onto the
/// pipe to get around this.
fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    emit_text_file(w, "/proc/self/maps")?;
    Ok(())
}

/// Serialize a siginfo_t to the given handle.
///
/// This function eagerly emits various parts of the siginfo_t, even though some components are
/// only circumstantially useful.  The kernel generally provides sane defaults to unused
/// members, and for downstream consumers it is probably easier to present a consistent keyspace
/// than to conditionally emit fields.
///
/// In particular, note that `si_addr` is a repeat of `faulting_address`, the latter of which is
/// emitted only for `SIGSEGV` signals.  This data layout should be considered unstable until
/// canonized in a data format RFC.
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
/// SIGNAL SAFETY:
///     This function is careful to only write to the handle, without doing any
///     unnecessary mutexes or memory allocation.
pub(crate) fn emit_siginfo(w: &mut impl Write, siginfo: libc::siginfo_t) -> anyhow::Result<()> {
    let signame = siginfo_strings::get_signal_name(&siginfo);
    let codename = siginfo_strings::get_code_name(&siginfo);
    let si_addr = unsafe { siginfo.si_addr() } as usize;
    let si_signo = siginfo.si_signo;
    let si_pid = unsafe { siginfo.si_pid() };
    let si_code = siginfo.si_code;

    writeln!(w, "{DD_CRASHTRACK_BEGIN_SIGINFO}")?;
    write!(w, "{{")?;
    write!(w, "\"signum\": {si_signo}, ")?;
    write!(w, "\"signame\": \"{signame}\", ")?;
    write!(w, "\"pid\": {si_pid}, ")?;
    write!(w, "\"code\": {si_code}, ")?;
    write!(w, "\"codename\": \"{codename}\", ")?;
    if siginfo.si_signo == libc::SIGSEGV {
        write!(w, "\"faulting_address\": {si_addr}, ")?;
    }
    write!(w, "\"si_addr\": {si_addr}")?; // last field, no trailing comma
    writeln!(w, "}}")?;
    writeln!(w, "{DD_CRASHTRACK_END_SIGINFO}")?;
    Ok(())
}

/// Emit a file onto the given handle.
/// The file will be emitted in the format
///
/// DD_CRASHTRACK_BEGIN_FILE
/// <FILE BYTES>
/// DD_CRASHTRACK_END_FILE
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
///     The receiver expects the file to contain valid UTF-8 compatible text.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
/// SIGNAL SAFETY:
///     This function is careful to only write to the handle, without doing any
///     unnecessary mutexes or memory allocation.
#[allow(dead_code)]
fn emit_text_file(w: &mut impl Write, path: &str) -> anyhow::Result<()> {
    // open is signal safe
    // https://man7.org/linux/man-pages/man7/signal-safety.7.html
    let mut file = File::open(path).with_context(|| path.to_string())?;

    // Reading the file into a fixed buffer is signal safe.
    // Doing anything more complicated may involve allocation which is not.
    // So, just read it in, and then immediately push it out to the pipe.
    const BUFFER_LEN: usize = 512;
    let mut buffer = [0u8; BUFFER_LEN];

    writeln!(w, "{DD_CRASHTRACK_BEGIN_FILE} {path}")?;

    loop {
        let read_count = file.read(&mut buffer)?;
        w.write_all(&buffer[..read_count])?;
        if read_count == 0 {
            break;
        }
    }
    writeln!(w, "\n{DD_CRASHTRACK_END_FILE} \"{path}\"")?;
    w.flush()?;
    Ok(())
}

#[cfg(test)]
struct JsonCaptureWriter {
    buffer: Vec<u8>,
}

#[cfg(test)]
impl JsonCaptureWriter {
    fn new() -> Self {
        JsonCaptureWriter { buffer: Vec::new() }
    }

    pub fn get_json(&self) -> String {
        use regex::Regex;
        let output = String::from_utf8_lossy(&self.buffer);
        let re = Regex::new(r"DD_CRASHTRACK_\w+").unwrap();
        re.replace_all(&output, "").trim().to_string()
    }
}

#[cfg(test)]
impl Write for JsonCaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // NOP, but needed for interface
        Ok(())
    }
}

#[test]
fn test_sigaction_siginfo() -> anyhow::Result<()> {
    // This is a simple test to make sure that a valid SigInfo can be created from a
    // libc::siginfo_t on the current platform.
    use crate::SigInfo;
    let mut siginfo: libc::siginfo_t = unsafe { std::mem::zeroed() };
    siginfo.si_signo = libc::SIGSEGV;
    siginfo.si_code = 1;

    let mut writer = JsonCaptureWriter::new();
    emit_siginfo(&mut writer, siginfo)?;
    let siginfo_json_str = writer.get_json();

    // Is it valid JSON?
    let _: serde_json::Value = serde_json::from_str(&siginfo_json_str)?;

    // Is it a valid SigInfo?
    let siginfo: SigInfo = serde_json::from_str(&siginfo_json_str)?;

    // Sanity check for the fields
    assert_eq!(siginfo.signum, libc::SIGSEGV as u64);
    assert_eq!(siginfo.signame, "SIGSEGV");
    assert_eq!(siginfo.pid, 0);
    assert_eq!(siginfo.code, 1);
    assert_eq!(siginfo.codename, "SEGV_MAPERR");
    Ok(())
}
