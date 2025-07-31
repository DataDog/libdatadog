// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_os = "macos")]
pub fn alt_fork() -> i32 {
    // There is a lower-level `__fork()` function in macOS, and we can call it from Rust, but the
    // runtime is much stricter about which operations (e.g., no malloc) are allowed in the child.
    // This somewhat defeats the purpose, so macOS for now will just have to live with atfork
    // handlers.
    unsafe { libc::fork() }
}

#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{self, Read};

#[cfg(target_os = "linux")]
fn is_being_traced() -> io::Result<bool> {
    // Check to see whether we are being traced.  This will fail on systems where procfs is
    // unavailable, but presumably in those systems `ptrace()` is also unavailable.
    // The caller is free to treat a failure as a false.
    // This function may run in signal handler, so we should ensure that we do not allocate
    // memory on the heap (ex: avoiding using BufReader for example).
    let file = File::open("/proc/self/status")?;
    is_being_traced_internal(file)
}

#[cfg(target_os = "linux")]
const BUFFER_SIZE: usize = 1024;

#[cfg(target_os = "linux")]
fn is_being_traced_internal(mut file: File) -> io::Result<bool> {
    let tracer_pid_marker = b"TracerPid:";
    let mut buffer = [0u8; BUFFER_SIZE];
    let mut data_len = 0;
    let mut eof = false;
    let mut offset = 0;

    while !eof {
        if offset > 0 && offset < data_len {
            let leftover_len = data_len - offset;
            buffer.copy_within(offset..data_len, 0);
            data_len = leftover_len;
            offset = 0;
        } else if offset == data_len {
            // All data processed, reset data_len and offset
            data_len = 0;
            offset = 0;
        }

        let n = file.read(&mut buffer[data_len..])?;
        if n == 0 {
            eof = true;
        }
        data_len += n;

        while offset < data_len {
            if let Some(newline_pos) = buffer[offset..data_len].iter().position(|&b| b == b'\n') {
                let line_end = offset + newline_pos;
                let line = &buffer[offset..line_end];

                if line.starts_with(tracer_pid_marker) && line.len() > tracer_pid_marker.len() {
                    if let Ok(line_str) = std::str::from_utf8(line) {
                        let tracer_pid = line_str.split_whitespace().nth(1).unwrap_or("0");
                        return Ok(tracer_pid != "0");
                    }
                }

                offset = line_end + 1;
            } else {
                if offset == 0 && data_len == BUFFER_SIZE {
                    // We did not find any newline in the buffer, so force
                    // reading the full buffer anew
                    offset = data_len;
                }
                // No newline found: partial line, stop processing and read more
                break;
            }
        }
    }

    // search in the remaining data
    if data_len > offset {
        let line = &buffer[offset..data_len];
        if line.starts_with(tracer_pid_marker) {
            if let Ok(line_str) = std::str::from_utf8(line) {
                let tracer_pid = line_str.split_whitespace().nth(1).unwrap_or("0");
                return Ok(tracer_pid != "0");
            }
        }
    }

    Ok(false)
}

#[cfg(target_os = "linux")]
pub fn alt_fork() -> libc::pid_t {
    use libc::{
        c_ulong, c_void, pid_t, syscall, SYS_clone, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID,
        CLONE_PTRACE, SIGCHLD,
    };

    let mut _ptid: pid_t = 0;
    let mut _ctid: pid_t = 0;

    // Check whether we're traced before we fork.
    let being_traced = is_being_traced().unwrap_or(false);
    let extra_flags = if being_traced { CLONE_PTRACE } else { 0 };

    // Use the direct syscall interface into `clone()`.  This should replicate the parameters used
    // for glibc `fork()`, except of course without calling the atfork handlers.
    // One question is whether we're using the right set of flags.  For instance, does suppressing
    // `SIGCHLD` here make it easier for us to handle some conditions in the parent process?
    let res = unsafe {
        syscall(
            SYS_clone,
            (CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID | SIGCHLD | extra_flags) as c_ulong,
            std::ptr::null_mut::<c_void>(),
            &mut _ptid as *mut pid_t,
            &mut _ctid as *mut pid_t,
            0 as c_ulong,
        )
    };

    // The max value of a PID is configurable, but within an i32, so the failover
    if (res as i64) > (pid_t::MAX as i64) {
        pid_t::MAX
    } else if (res as i64) < (pid_t::MIN as i64) {
        pid_t::MIN
    } else {
        res as pid_t
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod tests {
    use crate::unix_utils::fork::is_being_traced_internal;
    use crate::unix_utils::fork::BUFFER_SIZE;
    use std::fs::File;
    use std::io::Seek;
    use std::io::Write;

    #[test]
    fn test_is_being_traced_in_middle() {
        let lines: &[&[u8]] = &[b"First:item\n", b"TracerPid: 2\n", b"Another: 21"];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_at_the_end() {
        let lines: &[&[u8]] = &[b"First:item\n", b"Another: 21\n", b"TracerPid: 2\n"];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_at_the_end_with_ending_newline() {
        let lines: &[&[u8]] = &[b"First:item\n", b"Another: 21\n", b"TracerPid: 2"];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_at_the_beginning() {
        let lines: &[&[u8]] = &[b"TracerPid: 2\n", b"nFirst:item\n", b"Another: 21"];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_with_first_string_larger_than_buffer() {
        // Create a string larger than BUFFER_SIZE
        let large_string = "A".repeat(BUFFER_SIZE + 20);
        let lines: &[&[u8]] = &[
            large_string.as_bytes(),
            b"\n",
            b"Another:12\n",
            b"TracerPid: 42\n",
        ];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_with_marker_in_between_buffer_read() {
        let large_string = "A".repeat(BUFFER_SIZE - 4);
        let lines: &[&[u8]] = &[
            large_string.as_bytes(),
            b"\n",
            b"TracerPid: 42\n",
            b"AnotherItem: 42\n",
        ];
        let f = create_temp_file(lines);
        assert!(is_being_traced_internal(f).unwrap_or(false))
    }

    #[test]
    fn test_is_being_traced_with_value_zero() {
        let lines: &[&[u8]] = &[b"First:item\n", b"TracerPid: 0\n", b"AnotherItem: 21\n"];
        let f = create_temp_file(lines);
        assert!(!is_being_traced_internal(f).unwrap_or(true))
    }

    fn create_temp_file(lines: &[&[u8]]) -> File {
        let mut f = tempfile::tempfile().unwrap();
        for line in lines {
            f.write_all(line).unwrap();
        }

        f.flush().unwrap();
        f.rewind().unwrap();

        f
    }
}
