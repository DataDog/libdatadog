// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use anyhow::Context;

use super::constants::*;
use std::{
    fs::File,
    io::{Read, Write},
};

// Getting a backtrace on rust is not guaranteed to be signal safe
// https://github.com/rust-lang/backtrace-rs/issues/414
// My experiemnts show that just calculating the `ip` of the frames seems
// to bo ok for Python, but resolving the frames crashes.
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
            // TODO: Figure out why this can give multiple answers.
            // This looks like it might be related to use of closures/inline?
            // For now, just print all resolved names/locations.
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
                        comma_needed = true;
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

// Getting a backtrace on rust is not guaranteed to be signal safe
// https://github.com/rust-lang/backtrace-rs/issues/414
// let current_backtrace = backtrace::Backtrace::new();
// In fact, if we look into the code here, we see mallocs.
// https://doc.rust-lang.org/src/std/backtrace.rs.html#332
pub fn _emit_backtrace_std(w: &mut impl Write) {
    let current_backtrace = std::backtrace::Backtrace::force_capture();
    writeln!(w, "{:?}", current_backtrace).unwrap();
}

// TODO comment why I do this by block not line
pub fn emit_file(w: &mut impl Write, path: &str) -> anyhow::Result<()> {
    let mut file = File::open(path).with_context(|| path.to_string())?;
    const BUFFER_LEN: usize = 512;
    let mut buffer = [0u8; BUFFER_LEN];

    writeln!(w, "{DD_CRASHTRACK_BEGIN_FILE} {path}")?;

    loop {
        let read_count = file.read(&mut buffer)?;
        w.write_all(&buffer)?;

        if read_count != BUFFER_LEN {
            break;
        }
    }
    writeln!(w, "\n{DD_CRASHTRACK_END_FILE} \"{path}\"")?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    emit_file(w, "/proc/self/maps")?;
    Ok(())
}
