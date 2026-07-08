// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Same-process reader for the OTEL process context mapping.
//!
//! By design, this reader emulates an out-of-process reader: it does not take the publisher's
//! mutex or hold any guard that prevents the mapping from being unpublished, the header from being
//! rewritten, or the payload allocation from being replaced while it copies memory. The
//! seqlock-style timestamp checks and fences only let it detect concurrent publication or update;
//! unpublish can make the memory disappear entirely.
//!
//! To handle this safely, instead of dereferencing process memory directly, we ask the kernel to
//! copy those bytes into a pipe and then read the pipe back. This gives us the same failure mode an
//! out-of-process reader has: if memory is unmapped or invalid, the copy fails instead of
//! segfaulting the process.

use core::{
    cell::Cell,
    mem::offset_of,
    ptr::{self, NonNull},
    sync::atomic::{fence, Ordering},
};
use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value, AnyValue, KeyValue, ProcessContext,
};
use prost::Message;

use super::{MappingHeader, PROCESS_CTX_VERSION, SIGNATURE, UNPUBLISHED_OR_UPDATING};

/// Reader for the current process's OTel process context mapping.
///
/// Locates the OTEL_CTX mapping at construction. Call [`read`](Self::read) repeatedly to fetch
/// updated context data without re-parsing `/proc/self/maps`, as long as the process has not
/// forked. After a `fork()`, reads fail and a new reader must be constructed.
pub struct ProcessContextSelfReader {
    pid: libc::pid_t,
    header_ptr: NonNull<u8>,
    pipe: Cell<Option<CopyPipe>>,
}

// SAFETY: ProcessContextSelfReader can be moved between threads because it owns its pipe file
// descriptors and only stores a process-global mapping address. `Cell` keeps it !Sync, so the
// cached pipe cannot be used concurrently through shared references.
unsafe impl Send for ProcessContextSelfReader {}
// we do not implement Sync because of the Cell

impl ProcessContextSelfReader {
    /// Locates the OTEL_CTX mapping in `/proc/self/maps`.
    pub fn new() -> io::Result<Self> {
        let mapping_addr = Self::find_otel_mapping()?;
        // SAFETY: getpid() is always safe to call.
        let pid = unsafe { libc::getpid() };
        let reader = Self {
            pid,
            header_ptr: Self::header_ptr_from_addr(mapping_addr)?,
            pipe: Cell::new(None),
        };
        Ok(reader)
    }

    /// Reads and decodes the current process's OTel process context.
    ///
    /// Returns [`io::ErrorKind::WouldBlock`] if a writer is currently publishing or updating the
    /// context, or if the context changed while it was being read. Callers may retry later.
    pub fn read(&self) -> io::Result<ProcessContext> {
        // SAFETY: getpid() is always safe to call.
        let current_pid = unsafe { libc::getpid() };
        if current_pid != self.pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context reader is stale after fork; construct a new reader",
            ));
        }

        let published_at = self.read_published_at()?;
        if published_at == UNPUBLISHED_OR_UPDATING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context is currently being updated",
            ));
        }

        // This is the first read-side seqlock fence. Header and payload copies happen after it;
        // the second fence below precedes the final timestamp copy.
        fence(Ordering::SeqCst);

        let header = self.read_header()?;
        if header.monotonic_published_at_ns != published_at {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context changed while being read",
            ));
        }

        if header.signature != *SIGNATURE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid signature in process context mapping",
            ));
        }
        if header.version != PROCESS_CTX_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported process context version {}", header.version),
            ));
        }

        let payload_size = header.payload_size;
        let payload_ptr = header.payload_ptr;

        if payload_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context payload pointer is null",
            ));
        }

        let payload_bytes = self.read_process_memory(payload_ptr, payload_size as usize)?;

        // This is the second read-side seqlock fence. It pairs with the writer's SeqCst fences so
        // that, if we copied data updated after the initial published time, the final timestamp
        // copy sees `UNPUBLISHED_OR_UPDATING` or a later published timestamp.
        fence(Ordering::SeqCst);

        let published_at_after = self.read_published_at()?;
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

    /// Returns the thread-local attribute key map from a decoded process context.
    pub fn threadlocal_attribute_key_map(context: &ProcessContext) -> Option<Vec<String>> {
        let key = "threadlocal.attribute_key_map";

        Self::find_attr(&context.extra_attributes, key).and_then(Self::string_array)
    }

    fn string_array(value: &AnyValue) -> Option<Vec<String>> {
        let any_value::Value::ArrayValue(array) = value.value.as_ref()? else {
            return None;
        };

        array
            .values
            .iter()
            .map(|value| match value.value.as_ref()? {
                any_value::Value::StringValue(value) => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    // The process context only carries a small resource/extra attribute set, so a linear scan
    // keeps this helper allocation-free and simpler than building a temporary index.
    fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
    }

    fn header_ptr_from_addr(mapping_addr: usize) -> io::Result<NonNull<u8>> {
        NonNull::new(ptr::with_exposed_provenance::<u8>(mapping_addr).cast_mut()).ok_or_else(|| {
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

    fn read_published_at(&self) -> io::Result<u64> {
        let timestamp_ptr = self
            .header_ptr
            .as_ptr()
            .wrapping_add(offset_of!(MappingHeader, monotonic_published_at_ns))
            .cast_const();
        let bytes = self.read_process_memory(timestamp_ptr, size_of::<u64>())?;
        Ok(u64::from_ne_bytes(
            Self::field_bytes::<{ size_of::<u64>() }>(&bytes, 0, "monotonic_published_at_ns")?,
        ))
    }

    fn read_header(&self) -> io::Result<MappingHeader> {
        let bytes = self.read_process_memory(
            self.header_ptr.as_ptr().cast_const(),
            size_of::<MappingHeader>(),
        )?;
        Self::mapping_header_from_bytes(&bytes)
    }

    fn mapping_header_from_bytes(bytes: &[u8]) -> io::Result<MappingHeader> {
        if bytes.len() != size_of::<MappingHeader>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "process context header copy had size {}, expected {}",
                    bytes.len(),
                    size_of::<MappingHeader>()
                ),
            ));
        }

        let signature =
            Self::field_bytes::<8>(bytes, offset_of!(MappingHeader, signature), "signature")?;
        let version = u32::from_ne_bytes(Self::field_bytes::<{ size_of::<u32>() }>(
            bytes,
            offset_of!(MappingHeader, version),
            "version",
        )?);
        let payload_size = u32::from_ne_bytes(Self::field_bytes::<{ size_of::<u32>() }>(
            bytes,
            offset_of!(MappingHeader, payload_size),
            "payload_size",
        )?);
        let monotonic_published_at_ns =
            u64::from_ne_bytes(Self::field_bytes::<{ size_of::<u64>() }>(
                bytes,
                offset_of!(MappingHeader, monotonic_published_at_ns),
                "monotonic_published_at_ns",
            )?);
        let payload_addr = usize::from_ne_bytes(Self::field_bytes::<{ size_of::<usize>() }>(
            bytes,
            offset_of!(MappingHeader, payload_ptr),
            "payload_ptr",
        )?);

        Ok(MappingHeader {
            signature,
            version,
            payload_size,
            monotonic_published_at_ns,
            payload_ptr: ptr::with_exposed_provenance(payload_addr),
        })
    }

    fn field_bytes<const N: usize>(
        bytes: &[u8],
        offset: usize,
        field: &'static str,
    ) -> io::Result<[u8; N]> {
        let end = offset.checked_add(N).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("process context header field {field} offset overflowed"),
            )
        })?;
        let slice = bytes.get(offset..end).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("process context header field {field} is out of bounds"),
            )
        })?;
        let mut out = [0; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    /// Copies `len` bytes from `addr` through a pipe.
    ///
    /// `write(2)` copies from the source address in kernel space, so unmapped source memory is
    /// reported as an error or a short write instead of raising `SIGSEGV`.
    fn read_process_memory(&self, addr: *const u8, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let pipe = match self.pipe.take() {
            Some(pipe) => pipe,
            None => CopyPipe::new()?,
        };

        match pipe.copy_via_pipe(addr, len) {
            Ok(buf) => {
                self.pipe.set(Some(pipe));
                Ok(buf)
            }
            Err(PipeCopyError { err, pipe_dirty }) => {
                if !pipe_dirty {
                    // The pipe does not hold bytes from an aborted transfer
                    // Save it as a sort of cache
                    // Note that we're not Sync
                    self.pipe.set(Some(pipe));
                }
                Err(err)
            }
        }
    }
}

/// A cached pipe used to probe-copy process memory via `write(2)`.
///
/// Invariant: the pipe is empty between calls to `CopyPipe::copy_via_pipe`.
struct CopyPipe {
    read_fd: OwnedFd,
    write_fd: OwnedFd,
    capacity: usize,
}

impl CopyPipe {
    fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: `fds` points to space for the two file descriptors returned by pipe2.
        // `O_NONBLOCK` makes invariant bugs fail with EAGAIN instead of blocking forever.
        let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if ret != 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(
                err.kind(),
                format!("failed to create process context copy pipe: {err}"),
            ));
        }

        // SAFETY: pipe2 initialized both file descriptors on success and ownership is
        // transferred to OwnedFd exactly once.
        let (read_fd, write_fd) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

        // SAFETY: `write_fd` is a valid pipe file descriptor.
        let capacity = unsafe { libc::fcntl(write_fd.as_raw_fd(), libc::F_GETPIPE_SZ) };
        if capacity < 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(
                err.kind(),
                format!("failed to query process context copy pipe capacity: {err}"),
            ));
        }
        if capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context copy pipe has zero capacity",
            ));
        }

        Ok(Self {
            read_fd,
            write_fd,
            capacity: capacity as usize,
        })
    }

    fn copy_via_pipe(&self, addr: *const u8, len: usize) -> Result<Vec<u8>, PipeCopyError> {
        // The buffer is filled sequentially from the pipe; `buf[..offset]` is always initialized,
        // so there is no need to zero it up front.
        let mut buf: Vec<u8> = Vec::with_capacity(len);
        let mut offset = 0;

        while offset < len {
            let chunk_len = (len - offset).min(self.capacity);
            let chunk_addr = addr.wrapping_add(offset);

            // SAFETY: `write` copies from `chunk_addr` in kernel space without dereferencing it in
            // Rust. Invalid user memory is reported by the kernel as EFAULT (nothing copied) or a
            // short write (fault partway through).
            let nbytes = loop {
                let result =
                    unsafe { libc::write(self.write_fd.as_raw_fd(), chunk_addr.cast(), chunk_len) };
                if result > 0 {
                    break result as usize;
                }
                if result == 0 {
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::WriteZero,
                            "zero-length write while copying process context memory into pipe",
                        ),
                        pipe_dirty: false,
                    });
                }

                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(errno) if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK => {
                        return Err(PipeCopyError {
                            err: io::Error::other(
                                "process context copy pipe blocked despite capacity-limited write",
                            ),
                            pipe_dirty: true,
                        });
                    }
                    // EFAULT is only returned when the fault occurs before any byte is copied, so
                    // the pipe is still empty.
                    Some(libc::EFAULT) => {
                        return Err(PipeCopyError {
                            err: io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "process context memory was unmapped during read",
                            ),
                            pipe_dirty: false,
                        });
                    }
                    _ => {
                        return Err(PipeCopyError {
                            err: io::Error::new(
                                err.kind(),
                                format!("failed to copy process context memory into pipe: {err}"),
                            ),
                            pipe_dirty: false,
                        });
                    }
                }
            };

            // A short write (`nbytes < chunk_len`) means the kernel copy stopped partway — either a
            // fault or a signal after partial transfer; the two are indistinguishable here. Drain
            // the bytes that made it and retry the remainder: `offset` advances by `nbytes`, so the
            // next write starts exactly at the stop point. If the source really is unmapped, that
            // write fails with EFAULT before copying anything, and we report it then. Since
            // `nbytes >= 1`, progress is guaranteed and the loop terminates.
            //
            // Draining exactly `nbytes` every iteration also keeps the pipe empty before each
            // write, so `chunk_len <= capacity` should leave every write ready immediately.
            let mut drained = 0;
            while drained < nbytes {
                // SAFETY: `buf` owns `len` bytes of writable capacity and
                // `offset + nbytes <= len`, so the destination range is valid.
                let result = unsafe {
                    libc::read(
                        self.read_fd.as_raw_fd(),
                        buf.as_mut_ptr().add(offset + drained).cast(),
                        nbytes - drained,
                    )
                };
                if result > 0 {
                    drained += result as usize;
                    continue;
                }

                if result == 0 {
                    // Unreachable in practice: this process holds the write end open, so the pipe
                    // cannot report EOF
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "pipe reported EOF while draining process context payload",
                        ),
                        pipe_dirty: true,
                    });
                }

                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(errno) if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK => {
                        return Err(PipeCopyError {
                            err: io::Error::other(
                                "process context copy pipe had no bytes to drain after write",
                            ),
                            // The pipe state violated our expected write/drain sequence.
                            pipe_dirty: true,
                        });
                    }
                    _ => {
                        return Err(PipeCopyError {
                            err: io::Error::new(
                                err.kind(),
                                format!("failed to drain process context memory from pipe: {err}"),
                            ),
                            // Undrained bytes may remain in the pipe.
                            pipe_dirty: true,
                        });
                    }
                }
            }

            offset += nbytes;
        }

        // SAFETY: the loop exits with `offset == len`, and every byte of `buf[..offset]` was
        // initialized by the pipe reads above.
        unsafe { buf.set_len(len) };
        Ok(buf)
    }
}

/// Error from [`copy_via_pipe`], carrying whether the pipe may still hold undrained bytes.
#[derive(Debug)]
struct PipeCopyError {
    err: io::Error,
    /// If true, the pipe may contain leftover bytes and must not be reused.
    pipe_dirty: bool,
}

#[cfg(test)]
mod tests {
    use core::ptr;
    use std::io;

    use super::{CopyPipe, ProcessContextSelfReader};

    fn with_published_mapping(f: impl FnOnce()) {
        super::super::publish_raw_payload(b"setup".to_vec()).expect("publish should succeed");
        f();
        super::super::unpublish().expect("unpublish should succeed");
    }

    fn page_size() -> usize {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0, "page size query should succeed");
        page_size as usize
    }

    fn map_anonymous(len: usize) -> *mut u8 {
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(
            ptr,
            libc::MAP_FAILED,
            "anonymous page mapping should succeed"
        );
        ptr.cast()
    }

    fn map_anonymous_page() -> *mut u8 {
        map_anonymous(page_size())
    }

    fn unmap(ptr: *mut u8, len: usize) {
        let ret = unsafe { libc::munmap(ptr.cast(), len) };
        assert_eq!(ret, 0, "page unmap should succeed");
    }

    fn unmap_page(ptr: *mut u8) {
        unmap(ptr, page_size());
    }

    struct MappedPage(*mut u8);

    impl MappedPage {
        fn as_ptr(&self) -> *const u8 {
            self.0.cast()
        }
    }

    impl Drop for MappedPage {
        fn drop(&mut self) {
            unmap_page(self.0);
        }
    }

    fn assert_unmapped_read_error(err: io::Error) {
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(
            err.to_string().contains("unmapped"),
            "unexpected error message: {err}"
        );
    }

    fn assert_is_otel_mapping(line: &str) {
        assert!(ProcessContextSelfReader::is_named_otel_mapping(line));
    }

    fn assert_is_not_otel_mapping(line: &str) {
        assert!(!ProcessContextSelfReader::is_named_otel_mapping(line));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial]
    fn read_returns_would_block_while_context_is_being_updated() {
        with_published_mapping(|| {
            let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
            // SAFETY: the mapping was published by this test before being read, and the test is
            // serial so no writer is concurrently mutating the plain header field.
            let published_at_ns = unsafe {
                let header = reader.header_ptr.as_ptr().cast::<super::MappingHeader>();
                let published_at_ns = (*header).monotonic_published_at_ns;
                (*header).monotonic_published_at_ns = super::UNPUBLISHED_OR_UPDATING;
                published_at_ns
            };

            let err = reader
                .read()
                .expect_err("read should report writer in progress");

            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
            assert!(
                err.to_string().contains("currently being updated"),
                "unexpected error message: {err}"
            );

            // SAFETY: same as above.
            unsafe {
                (*reader.header_ptr.as_ptr().cast::<super::MappingHeader>())
                    .monotonic_published_at_ns = published_at_ns;
            }
            drop(reader);
        });
    }

    #[test]
    fn is_named_otel_mapping_matches_exact_mapping_name() {
        assert_is_otel_mapping("7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX");
        assert_is_otel_mapping("7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX (deleted)");
        assert_is_otel_mapping("7f000000-7f001000 rw-p 00000000 00:00 0 [anon_shmem:OTEL_CTX]");
        assert_is_otel_mapping("7f000000-7f001000 rw-p 00000000 00:00 0 [anon:OTEL_CTX]");

        assert_is_not_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX_BACKUP",
        );
        assert_is_not_otel_mapping("7f000000-7f001000 rw-p 00000000 00:00 0 [anon:OTEL_CTX_old]");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial]
    fn read_process_memory_copies_valid_memory() {
        with_published_mapping(|| {
            let payload = b"example process context payload";

            let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
            let copy = reader
                .read_process_memory(payload.as_ptr(), payload.len())
                .expect("payload copy through pipe should succeed");

            assert_eq!(copy, payload);
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial]
    fn read_process_memory_copies_more_than_pipe_capacity() {
        with_published_mapping(|| {
            let pipe = CopyPipe::new().expect("pipe creation should succeed");
            let len = pipe
                .capacity
                .checked_add(1)
                .expect("pipe capacity should fit payload length");
            let payload: Vec<_> = (0..len).map(|i| i as u8).collect();

            let copy = pipe
                .copy_via_pipe(payload.as_ptr(), payload.len())
                .expect("large payload copy through pipe should succeed");

            assert_eq!(copy, payload);
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial]
    fn read_process_memory_fails_on_unmapped_address() {
        with_published_mapping(|| {
            let ptr = map_anonymous_page();
            unmap_page(ptr);

            let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
            let err = reader
                .read_process_memory(ptr.cast(), 1)
                .expect_err("read from unmapped address should fail");

            assert_unmapped_read_error(err);
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial]
    fn read_process_memory_fails_when_range_extends_past_mapped_page() {
        with_published_mapping(|| {
            let page_size = page_size();
            let pages = map_anonymous(page_size * 2);
            let second_page = pages.wrapping_add(page_size);
            unmap_page(second_page);
            let first_page = MappedPage(pages);
            let len = page_size + 1;

            let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
            let err = reader
                .read_process_memory(first_page.as_ptr(), len)
                .expect_err("read past mapped page should fail");

            assert_unmapped_read_error(err);
        });
    }
}
