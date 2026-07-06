// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    ptr::{self, NonNull},
    sync::atomic::{fence, Ordering},
};

#[cfg(debug_assertions)]
use std::sync::atomic::AtomicUsize;

use libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext;
use prost::Message;

use super::{MappingHeader, PROCESS_CTX_VERSION, SIGNATURE, UNPUBLISHED_OR_UPDATING};

#[cfg(debug_assertions)]
static LIVE_SELF_READERS: AtomicUsize = AtomicUsize::new(0);

/// Reader for the current process's OTel process context mapping.
///
/// Locates the OTEL_CTX mapping at construction. Call [`read`](Self::read) repeatedly to fetch
/// updated context data without re-parsing `/proc/self/maps`, as long as the process has not
/// forked. After a `fork()`, reads fail and a new reader must be constructed.
pub struct ProcessContextSelfReader {
    pid: libc::pid_t,
    header_ptr: NonNull<MappingHeader>,
}

// SAFETY: ProcessContextSelfReader doesn't rely on thread local state and
// only references static memory -- owns nothing.
unsafe impl Send for ProcessContextSelfReader {}
// SAFETY: ProcessContextSelfReader doesn't modify anything
unsafe impl Sync for ProcessContextSelfReader {}

impl ProcessContextSelfReader {
    /// Locates the OTEL_CTX mapping in `/proc/self/maps`.
    pub fn new() -> io::Result<Self> {
        let mapping_addr = Self::find_otel_mapping()?;
        // SAFETY: getpid() is always safe to call.
        let pid = unsafe { libc::getpid() };
        let reader = Self {
            pid,
            header_ptr: Self::header_ptr_from_addr(mapping_addr)?,
        };
        #[cfg(debug_assertions)]
        LIVE_SELF_READERS.fetch_add(1, Ordering::Relaxed);
        Ok(reader)
    }

    /// Reads and decodes the current process's OTel process context.
    pub fn read(&self) -> io::Result<ProcessContext> {
        // SAFETY: getpid() is always safe to call.
        let current_pid = unsafe { libc::getpid() };
        if current_pid != self.pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context reader is stale after fork; construct a new reader",
            ));
        }

        // SAFETY: `header_ptr` is non-null and points to our own process memory at an address
        // we found in /proc/self/maps for `self.pid`. The mapping must be readable if it is
        // listed as the OTel context.
        let header = unsafe { self.header_ptr.as_ref() };

        let published_at = header.monotonic_published_at_ns.load(Ordering::Acquire);
        if published_at == UNPUBLISHED_OR_UPDATING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context is currently being updated",
            ));
        }

        // `signature` and `version` are initialized before the release store that publishes
        // a non-zero timestamp. If the acquire load above observed that timestamp, those
        // writes are visible; if it observed `UNPUBLISHED_OR_UPDATING`, we returned before
        // reading them. Updates never mutate these fields, so their accesses are race-free.
        // The seqlock-controlled fields must be loaded atomically because they can change
        // during an update.
        let signature = header.signature;
        let version = header.version;

        if signature != *SIGNATURE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid signature in process context mapping",
            ));
        }
        if version != PROCESS_CTX_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported process context version {version}"),
            ));
        }

        let payload_size = header.payload_size.load(Ordering::Relaxed);
        let payload_ptr = header.payload_ptr.load(Ordering::Relaxed).cast_const();

        if payload_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context payload pointer is null",
            ));
        }

        let payload_bytes =
            Self::read_process_memory(self.pid, payload_ptr, payload_size as usize)?;

        // pairs with the first release fence on update() to ensure that, if we read data
        // updated after the initial published time, we at least see the published
        // time being set to 0 in the next load of the published time (or we could
        // see a later time rather than 0)
        fence(Ordering::Acquire);

        let published_at_after = header.monotonic_published_at_ns.load(Ordering::Relaxed);
        if published_at != published_at_after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context changed while being read",
            ));
        }

        let context = ProcessContext::decode(payload_bytes.as_slice())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        Ok(context)
    }

    fn header_ptr_from_addr(mapping_addr: usize) -> io::Result<NonNull<MappingHeader>> {
        NonNull::new(ptr::with_exposed_provenance::<MappingHeader>(mapping_addr).cast_mut())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "null process context header")
            })
    }

    /// Find the OTEL_CTX mapping in /proc/self/maps.
    pub(super) fn find_otel_mapping() -> io::Result<usize> {
        let file = File::open("/proc/self/maps")?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;

            if Self::is_named_otel_mapping(&line) {
                if let Some(addr) = Self::parse_mapping_start(&line) {
                    return Ok(addr);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "couldn't find the mapping of the process context",
        ))
    }

    /// Parses the start address from a /proc/self/maps line.
    fn parse_mapping_start(line: &str) -> Option<usize> {
        usize::from_str_radix(line.split('-').next()?, 16).ok()
    }

    /// Checks if a mapping line refers to the OTEL_CTX mapping.
    fn is_named_otel_mapping(line: &str) -> bool {
        let trimmed = line.trim_end();

        // The mapping name is the `pathname` column documented for `/proc/<pid>/maps`:
        // https://github.com/torvalds/linux/blob/9147566d801602c9e7fc7f85e989735735bf38ba/Documentation/filesystems/proc.rst?plain=1#L384-L386
        // For the OTEL_CTX names we care about, it is the 6th whitespace-delimited field;
        // `split_whitespace()` ignores the column padding.
        let Some(name) = trimmed.split_whitespace().nth(5) else {
            return false;
        };

        // The OTel process context spec says to search for entries whose names start with
        // these prefixes. In `/proc/<pid>/maps`, however, the optional ` (deleted)` suffix is
        // emitted as a separate token, so the mapping-name token itself should match exactly.
        matches!(
            name,
            "/memfd:OTEL_CTX" | "[anon_shmem:OTEL_CTX]" | "[anon:OTEL_CTX]"
        )
    }

    /// Reads `len` bytes from `addr` in the address space of `pid` via `process_vm_readv(2)`.
    ///
    /// Returns [`ErrorKind::WouldBlock`] for retryable races where the remote memory is no
    /// longer mapped or only partially readable. The kernel reports the former as
    /// [`libc::EFAULT`] from `pin_user_pages_remote()` and the latter as a short read (see
    /// `process_vm_rw_core()` in `mm/process_vm_access.c`).
    fn read_process_memory(pid: libc::pid_t, addr: *const u8, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let mut buf = vec![0u8; len];
        let local_iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: len,
        };
        let remote_iov = libc::iovec {
            iov_base: addr.cast_mut().cast(),
            iov_len: len,
        };

        // SAFETY: `buf` and `addr` each span `len` bytes for the duration of the syscall.
        let nbytes = unsafe { libc::process_vm_readv(pid, &local_iov, 1, &remote_iov, 1, 0) };

        if nbytes < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EFAULT) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "process context payload was unmapped during read",
                ));
            }
            return Err(io::Error::new(
                err.kind(),
                format!("failed to read process context payload: {err}"),
            ));
        }

        if nbytes as usize != len {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "incomplete read of process context payload",
            ));
        }

        Ok(buf)
    }
}

#[cfg(debug_assertions)]
impl Drop for ProcessContextSelfReader {
    fn drop(&mut self) {
        LIVE_SELF_READERS.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(debug_assertions)]
pub(super) fn live_reader_count() -> usize {
    LIVE_SELF_READERS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::ProcessContextSelfReader;

    #[test]
    fn is_named_otel_mapping_matches_exact_mapping_name() {
        assert!(ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX"
        ));
        assert!(ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX (deleted)"
        ));
        assert!(ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 [anon_shmem:OTEL_CTX]"
        ));
        assert!(ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 [anon:OTEL_CTX]"
        ));

        assert!(!ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX_BACKUP"
        ));
        assert!(!ProcessContextSelfReader::is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 [anon:OTEL_CTX_old]"
        ));
    }
}
