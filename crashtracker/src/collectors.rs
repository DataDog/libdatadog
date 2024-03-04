// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use anyhow::Context;

use super::constants::*;
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
pub unsafe fn emit_backtrace_by_frames(
    w: &mut impl Write,
    resolve_frames: bool,
) -> anyhow::Result<()> {
    // https://docs.rs/backtrace/latest/backtrace/index.html
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;
    backtrace::trace_unsynchronized(|frame| {
        // Write the values we can get without resolving, since these seem to
        // be crash safe in my experiments.
        write! {w, "{{"}.unwrap();
        write!(w, "\"ip\": \"{:?}\", ", frame.ip()).unwrap();
        if let Some(module_base_address) = frame.module_base_address() {
            write!(w, "\"module_base_address\": \"{module_base_address:?}\", ",).unwrap();
        }
        write!(w, "\"sp\": \"{:?}\", ", frame.sp()).unwrap();
        write!(w, "\"symbol_address\": \"{:?}\"", frame.symbol_address()).unwrap();

        if resolve_frames {
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
    writeln! {w, "{DD_CRASHTRACK_END_STACKTRACE}"}.unwrap();
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
pub fn emit_text_file(w: &mut impl Write, path: &str) -> anyhow::Result<()> {
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

#[cfg(target_os = "linux")]
/// `/proc/self/maps` is very useful for debugging, and difficult to get from
/// the child process (permissions issues on Linux).  Emit it directly onto the
/// pipe to get around this.
pub fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    emit_text_file(w, "/proc/self/maps")?;
    Ok(())
}
