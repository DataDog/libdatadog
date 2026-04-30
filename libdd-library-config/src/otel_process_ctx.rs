// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "std")]

//! Implementation of the publisher part of the [OTEL process
//! context](https://github.com/open-telemetry/opentelemetry-specification/pull/4719)
//!
//! # A note on race conditions
//!
//! Process context sharing implies concurrently writing to a memory area that another process
//! might be actively reading. However, reading isn't done as direct memory accesses but goes
//! through the OS, so the Rust definition of race conditions doesn't really apply. We also use
//! atomics and fences, see MappingHeader's documentation.

#[cfg(target_os = "linux")]
#[cfg(target_has_atomic = "64")]
pub mod linux {
    use std::{
        ffi::{c_void, CStr},
        mem::ManuallyDrop,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        ptr::{self, addr_of_mut},
        sync::{
            atomic::{fence, AtomicU64, Ordering},
            Mutex, MutexGuard,
        },
        time::Duration,
    };

    use anyhow::Context;

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext;
    use prost::Message;

    /// Current version of the process context format
    pub const PROCESS_CTX_VERSION: u32 = 2;
    /// Signature bytes for identifying process context mappings
    pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
    /// The discoverable name of the memory mapping.
    pub const MAPPING_NAME: &CStr = c"OTEL_CTX";

    /// The header structure written at the start of the mapping. This must match the C
    /// layout of the specification.
    ///
    /// # Atomic accesses
    ///
    /// The publishing protocol requires some form of synchronization. Using fences or any non-OS
    /// based synchronization requires the use of atomics to have any effect (see [Mandatory
    /// atomic](https://doc.rust-lang.org/std/sync/atomic/fn.fence.html#mandatory-atomic))
    ///
    /// We use `monotonic_published_at_ns` for synchronization with the reader. Ideally, it should
    /// be an `AtomicU64`, but this is incompatible with `#[repr(C, packed)]` by default, as it
    /// could be misaligned. In our case, given the page size and the layout of `MappingHeader`, it
    /// is actually 8-bytes aligned: we use [`AtomicU64::from_ptr`] to create an atomic view when
    /// synchronization is needed.
    #[repr(C, packed)]
    struct MappingHeader {
        signature: [u8; 8],
        version: u32,
        payload_size: u32,
        monotonic_published_at_ns: u64,
        payload_ptr: *const u8,
    }

    /// The shared memory mapped area to publish the context to. The memory region is owned by a
    /// [MemMapping] instance and is automatically unmapped upon drop.
    ///
    /// # Safety
    ///
    /// The following invariants MUST always hold for safety and are guaranteed by [MemMapping]:
    /// - `start` is non-null, is coming from a previous call to `mmap` with a size value of
    ///   [mapping_size] and hasn't been unmmaped since.
    /// - once `self` has been dropped, no memory access must be performed on the memory previously
    ///   pointed to by `start`.
    struct MemMapping {
        start_addr: *mut c_void,
    }

    // Safety: MemMapping represents ownership over the mapped region. It never leaks or
    // share the internal pointer. It's also safe to drop (`munmap`) from a different thread.
    unsafe impl Send for MemMapping {}

    /// The global instance of the context for the current process.
    ///
    /// We need a mutex to put the handle in a static and avoid bothering the users of this API
    /// with storing the handle, but we don't expect this mutex to actually be contended. Ideally a
    /// single thread should handle context updates, even if it's not strictly required.
    static PROCESS_CONTEXT_HANDLER: Mutex<Option<ProcessContextHandle>> = Mutex::new(None);

    impl MemMapping {
        /// Creates a suitable memory mapping for the context protocol to be published.
        ///
        /// `memfd` is the preferred method, but this function fallbacks to an anonymous mapping if
        /// `memfd` failed for any reason.
        fn new() -> anyhow::Result<Self> {
            let size = mapping_size();

            try_memfd(MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL | libc::MFD_ALLOW_SEALING)
                .or_else(|_| try_memfd(MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING))
                .and_then(|fd| {
                    // Safety: fd is a valid open file descriptor.
                    check_syscall_retval(
                        unsafe {
                            libc::ftruncate(fd.as_raw_fd(), mapping_size() as libc::off_t)
                        },
                        "ftruncate failed"
                    )?;
                    // Safety: we pass a null pointer to mmap which is unconditionally ok
                    let start_addr = check_mapping_addr(
                        unsafe {
                            libc::mmap(
                                ptr::null_mut(),
                                size,
                                libc::PROT_WRITE | libc::PROT_READ,
                                libc::MAP_PRIVATE,
                                fd.as_raw_fd(),
                                0,
                            )
                        },
                        "mmap failed"
                    )?;

                    // We (implicitly) close the file descriptor right away, but this ok
                    Ok(MemMapping { start_addr })
                })
                // If any previous step failed, we fallback to an anonymous mapping
                .or_else(|_| {
                    // Safety: we pass a null pointer to mmap, no precondition to uphold
                    let start_addr = check_mapping_addr(
                        unsafe {
                            libc::mmap(
                                ptr::null_mut(),
                                size,
                                libc::PROT_WRITE | libc::PROT_READ,
                                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                                -1,
                                0,
                            )
                        },
                        "mmap failed: couldn't create a memfd or anonymous mmapped region for process context publication"
                    )?;

                    Ok(MemMapping { start_addr })
                })
        }

        /// Makes this mapping discoverable by giving it a name.
        fn set_name(&mut self) -> anyhow::Result<()> {
            // Safety: self.start_addr is valid for mapping_size() bytes as per MemMapping
            // invariants. name is a valid NUL-terminated string that outlives the prctl call.
            check_syscall_retval(
                unsafe {
                    libc::prctl(
                        libc::PR_SET_VMA,
                        libc::PR_SET_VMA_ANON_NAME as libc::c_ulong,
                        self.start_addr as libc::c_ulong,
                        mapping_size() as libc::c_ulong,
                        MAPPING_NAME.as_ptr() as libc::c_ulong,
                    )
                },
                "prctl PR_SET_VMA_ANON_NAME failed",
            )?;

            Ok(())
        }

        /// Unmaps the underlying memory region. This has same effect as dropping `self`, but
        /// propagates potential errors.
        fn free(mut self) -> anyhow::Result<()> {
            // Safety: We put `self` in a `ManuallyDrop`, which prevents drop and future calls to
            // `free()`.
            unsafe {
                self.unmap()?;
            }

            // Prevent `Self::drop` from being called
            let _ = ManuallyDrop::new(self);

            Ok(())
        }

        /// Unmaps the underlying memory region. For internal use only; prefer `free()` or `drop()`.
        ///
        /// # Safety
        ///
        /// This method must only be called once. After calling `unmap()`, no other method of
        /// `MemMapping` must be ever called on `self` again, including `unmap()` and `drop()`.
        ///
        /// Practically, `self` must be put in a `ManuallyDrop` wrapper and forgotten, or being in
        /// the process of being dropped.
        unsafe fn unmap(&mut self) -> anyhow::Result<()> {
            check_syscall_retval(
                // Safety: upheld by the caller.
                unsafe { libc::munmap(self.start_addr, mapping_size()) },
                "munmap failed when freeing the process context",
            )?;

            Ok(())
        }
    }

    impl Drop for MemMapping {
        fn drop(&mut self) {
            // Safety: `self` is being dropped
            let _ = unsafe { self.unmap() };
        }
    }

    /// Handle for future updates of a published process context.
    struct ProcessContextHandle {
        mapping: MemMapping,
        /// Once published, and until the next update is complete, the backing allocation of
        /// `payload` might be read by external processes and thus most not move (e.g. by resizing
        /// or drop).
        #[allow(unused)]
        payload: Vec<u8>,
        /// The process id of the last publisher. This is useful to detect forks(), and publish a
        /// new context accordingly.
        pid: libc::pid_t,
    }

    impl ProcessContextHandle {
        /// Initial publication of the process context. Creates an appropriate memory mapping.
        fn publish(payload: Vec<u8>) -> anyhow::Result<Self> {
            let mut mapping = MemMapping::new()?;
            let size = mapping_size();

            check_syscall_retval(
                // Safety: the invariants of MemMapping ensures `start_addr` is not null and comes
                // from a previous call to `mmap`
                unsafe { libc::madvise(mapping.start_addr, size, libc::MADV_DONTFORK) },
                "madvise MADVISE_DONTFORK failed",
            )?;

            let published_at_ns = since_boottime_ns().ok_or_else(|| {
                anyhow::anyhow!("failed to get current time for process context publication")
            })?;

            let header = mapping.start_addr as *mut MappingHeader;

            unsafe {
                // Safety: MappingHeader is packed, thus have no alignment requirement. It points
                // to a freshly mmaped region which is valid for writing at least `mapping_size()`,
                // which we make sure is greater than the size of MappingHeader.
                ptr::write(
                    header,
                    MappingHeader {
                        signature: *SIGNATURE,
                        version: PROCESS_CTX_VERSION,
                        payload_size: payload
                            .len()
                            .try_into()
                            .context("payload size overflowed")?,
                        // will be set atomically at last
                        monotonic_published_at_ns: 0,
                        payload_ptr: payload.as_ptr(),
                    },
                );
                // We typically want to avoid the compiler and the hardware to re-order the write
                // to the `monotonic_published_at_ns` (which should be last according to the
                // specification) with the writes to other fields of the header.
                //
                // To do so, we implement synchronization during publication _as if the reader were
                // another thread of this program_, using atomics and fences.
                fence(Ordering::SeqCst);
                AtomicU64::from_ptr(addr_of_mut!((*header).monotonic_published_at_ns))
                    .store(published_at_ns, Ordering::Relaxed);
            }

            // Note that naming must be unconditionally attempted, even on kernels where we might
            // know it will fail. It is ok for naming to fail - we must only make sure that at
            // least we tried, as per the
            // [spec](https://github.com/open-telemetry/opentelemetry-specification/pull/4719).
            let _ = mapping.set_name();

            Ok(ProcessContextHandle {
                mapping,
                payload,
                // Safety: getpid() is always safe to call.
                pid: unsafe { libc::getpid() },
            })
        }

        /// Updates the context after initial publication.
        fn update(&mut self, payload: Vec<u8>) -> anyhow::Result<()> {
            let header = self.mapping.start_addr as *mut MappingHeader;

            let monotonic_published_at_ns = since_boottime_ns()
                .ok_or_else(|| anyhow::anyhow!("could not get the current timestamp"))?;
            let payload_size = payload.len().try_into().map_err(|_| {
                anyhow::anyhow!("couldn't update process context: new payload too large")
            })?;

            // Safety:
            //
            // [^atomic-u64-alignment]: Page size is at minimum 4KB and will be always 8 bytes
            // aligned even on exotic platforms. The offset `monotonic_published_at_ns` is 16
            // bytes, so it's 8-bytes aligned (`AtomicU64` has both a size and align of 8 bytes).
            //
            // The header memory is valid for both read and writes.
            let published_at_atomic =
                unsafe { AtomicU64::from_ptr(addr_of_mut!((*header).monotonic_published_at_ns)) };

            // A process shouldn't try to concurrently update its own context
            //
            // Note: be careful of early return while `monotonic_published_at` is still zero, as
            // this would effectively "lock" any future publishing. Move throwing code above this
            // swap, or properly restore the previous value if the former can't be done.
            if published_at_atomic.swap(0, Ordering::Relaxed) == 0 {
                return Err(anyhow::anyhow!(
                    "concurrent update of the process context is not supported"
                ));
            }

            fence(Ordering::SeqCst);
            self.payload = payload;

            // Safety: we own the mapping, which is live and valid for writes. The header is packed
            // and thus has no alignment constraints.
            unsafe {
                (*header).payload_ptr = self.payload.as_ptr();
                (*header).payload_size = payload_size;
            }

            fence(Ordering::SeqCst);
            published_at_atomic.store(monotonic_published_at_ns, Ordering::Relaxed);

            Ok(())
        }
    }

    /// Returns `Err` wrapping the current `errno` with `msg` as context if `addr` equals
    /// `MAP_FAILED`, `Ok(addr)` otherwise.
    fn check_mapping_addr(addr: *mut c_void, msg: &'static str) -> anyhow::Result<*mut c_void> {
        if addr == libc::MAP_FAILED {
            Err(std::io::Error::last_os_error()).context(msg)
        } else {
            Ok(addr)
        }
    }

    /// Returns `Err` wrapping the current `errno` with `msg` as context if `ret` is negative,
    /// `Ok(ret)` otherwise.
    fn check_syscall_retval(ret: libc::c_int, msg: &'static str) -> anyhow::Result<libc::c_int> {
        if ret < 0 {
            Err(std::io::Error::last_os_error()).context(msg)
        } else {
            Ok(ret)
        }
    }

    /// Creates a `memfd` file descriptor with the given name and flags.
    fn try_memfd(name: &CStr, flags: libc::c_uint) -> anyhow::Result<OwnedFd> {
        // We use the raw syscall rather than `libc::memfd_create` because the latter requires
        // glibc >= 2.27, while `syscall()` + `SYS_memfd_create` works with any glibc version.
        check_syscall_retval(
            // Safety: name is a valid NUL-terminated string; flags are constant bit flags.
            unsafe {
                libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags as libc::c_long)
                    as libc::c_int
            },
            "memfd_create failed",
        )
        // Safety: fd is a valid file descriptor just returned by memfd_create.
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    // The returned size is guaranteed to be larger or equal to the size of `MappingHeader`.
    fn mapping_size() -> usize {
        size_of::<MappingHeader>()
    }

    /// Returns the value of the monotonic BOOTTIME clock in nanoseconds.
    fn since_boottime_ns() -> Option<u64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: ts is a valid, writable timespec.
        let ret = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
        if ret != 0 {
            return None;
        }
        let secs: u64 = ts.tv_sec.try_into().ok()?;
        let nanos: u32 = ts.tv_nsec.try_into().ok()?;
        let duration = Duration::new(secs, nanos);
        u64::try_from(duration.as_nanos()).ok()
    }

    /// Locks the context handle. Returns a uniform error if the lock has been poisoned.
    fn lock_context_handle() -> anyhow::Result<MutexGuard<'static, Option<ProcessContextHandle>>> {
        PROCESS_CONTEXT_HANDLER.lock().map_err(|_| {
            anyhow::anyhow!("a thread panicked while operating on the process context handler")
        })
    }

    /// Publishes or updates the process context for it to be visible by external readers.
    ///
    /// If any of the following condition holds:
    ///
    /// - this is the first publication
    /// - [unpublish] has been called last
    /// - the previous context has been published from a different process id (that is, a `fork()`
    ///   happened and we're the child process)
    ///
    /// Then we follow the Publish protocol of the OTel process context specification (allocating a
    /// fresh mapping).
    ///
    /// Otherwise, if a context has been previously published from the same process and hasn't been
    /// unpublished since, we follow the Update protocol.
    ///
    /// # Fork safety
    ///
    /// If we're a forked children of the original publisher, we are extremely restricted in the
    /// set of operations that we can do (we must be async-signal-safe). On paper, heap allocation
    /// is Undefined Behavior, for example. We assume that a forking runtime (such as Python or
    /// Ruby) that doesn't follow with an immediate `exec` is already "taking that risk", so to
    /// speak (typically, if no thread is ever spawned before the fork, things are mostly fine).
    #[inline]
    pub fn publish(context: &ProcessContext) -> anyhow::Result<()> {
        publish_raw_payload(context.encode_to_vec())
    }

    fn publish_raw_payload(payload: Vec<u8>) -> anyhow::Result<()> {
        let mut guard = lock_context_handle()?;

        // Safety: getpid() is always safe to call.
        match &mut *guard {
            Some(handler) if handler.pid == unsafe { libc::getpid() } => handler.update(payload),
            Some(handler) => {
                let mut local_handler = ProcessContextHandle::publish(payload)?;
                // If we've been forked, we need to prevent the mapping from being dropped
                // normally, as it would try to unmap a region that isn't mapped anymore in the
                // child process, or worse, could have been remapped to something else in the
                // meantime.
                //
                // To do so, we get the old handler back in `local_handler` and prevent `mapping`
                // from being dropped specifically.
                std::mem::swap(&mut local_handler, handler);
                let _: ManuallyDrop<MemMapping> = ManuallyDrop::new(local_handler.mapping);

                Ok(())
            }
            None => {
                *guard = Some(ProcessContextHandle::publish(payload)?);
                Ok(())
            }
        }
    }

    /// Unmaps the region used to share the process context. If no context has ever been published,
    /// this is no-op.
    ///
    /// A call to [publish] following an [unpublish] will create a new mapping.
    pub fn unpublish() -> anyhow::Result<()> {
        let mut guard = lock_context_handle()?;

        if let Some(ProcessContextHandle { mapping, .. }) = guard.take() {
            mapping.free()?;
        }

        Ok(())
    }

    #[cfg(test)]
    #[serial_test::serial]
    mod tests {
        use super::MappingHeader;
        use anyhow::ensure;
        use std::{
            fs::File,
            io::{BufRead, BufReader},
            ptr::{self, addr_of_mut},
            sync::atomic::{fence, AtomicU64, Ordering},
        };

        /// Parses the start address from a /proc/self/maps line
        fn parse_mapping_start(line: &str) -> Option<usize> {
            usize::from_str_radix(line.split('-').next()?, 16).ok()
        }

        /// Checks if a mapping line refers to the OTEL_CTX mapping.
        fn is_named_otel_mapping(line: &str) -> bool {
            let trimmed = line.trim_end();

            // The name of the mapping is the 6th column. The separator changes (both ' ' and '\t')
            // but `split_whitespace()` takes care of that.
            let Some(name) = trimmed.split_whitespace().nth(5) else {
                return false;
            };

            name.starts_with("/memfd:OTEL_CTX")
                || name.starts_with("[anon_shmem:OTEL_CTX]")
                || name.starts_with("[anon:OTEL_CTX]")
        }

        /// Establishes proper synchronization/memory ordering with the writer, checking that
        /// `monotonic_published_at` is not zero and that the signature is correct. Returns a
        /// pointer to the initialized header in case of success.
        fn verify_mapping_at(addr: usize) -> anyhow::Result<*const MappingHeader> {
            let header: *mut MappingHeader = ptr::with_exposed_provenance_mut(addr);
            // Safety: we're reading from our own process memory at an address we found in
            // /proc/self/maps. This should be safe as long as the mapping exists and has read
            // permissions.
            //
            // For the alignment constraint of `AtomicU64`, see [^atomic-u64-alignment].
            let published_at = unsafe {
                AtomicU64::from_ptr(addr_of_mut!((*header).monotonic_published_at_ns))
                    .load(Ordering::Relaxed)
            };
            ensure!(published_at != 0, "monotonic_published_at_ns is zero: couldn't read an initialized header in the candidate mapping");
            fence(Ordering::SeqCst);

            // Safety: if `monotonic_published_at_ns` is non-zero, the header is properly
            // initialized and thus readable.
            let signature = unsafe { &header.as_ref().unwrap().signature };
            ensure!(
                signature == super::SIGNATURE,
                "invalid signature in the candidate mapping"
            );

            Ok(header)
        }

        /// Find the OTEL_CTX mapping in /proc/self/maps
        fn find_otel_mapping() -> anyhow::Result<usize> {
            let file = File::open("/proc/self/maps")?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line?;

                if is_named_otel_mapping(&line) {
                    if let Some(addr) = parse_mapping_start(&line) {
                        return Ok(addr);
                    }
                }
            }

            Err(anyhow::anyhow!(
                "couldn't find the mapping of the process context"
            ))
        }

        /// Read the process context from the current process.
        ///
        /// This searches `/proc/self/maps` for an OTEL_CTX mapping and decodes its contents.
        ///
        /// **CAUTION**: Note that the reader implemented in this module, as well as the helper
        /// functions it relies on, are specialized for tests (for example, it doesn't check for
        /// concurrent writers after reading the header, because we know they can't be). Do not
        /// extract or use as it is as a generic Rust OTel process context reader.
        fn read_process_context() -> anyhow::Result<MappingHeader> {
            let mapping_addr = find_otel_mapping()?;
            let header_ptr = verify_mapping_at(mapping_addr)?;
            // Safety: the pointer returned by `verify_mapping_at` points to an initialized header
            Ok(unsafe { std::ptr::read(header_ptr) })
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn publish_then_update_process_context() {
            let payload_v1 = "example process context payload";
            let payload_v2 = "another example process context payload of different size";

            super::publish_raw_payload(payload_v1.as_bytes().to_vec())
                .expect("couldn't publish the process context");

            let header = read_process_context().expect("couldn't read back the process context");
            // Safety: the published context must have put valid bytes of size payload_size in the
            // context if the signature check succeded.
            let read_payload = unsafe {
                std::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
            };

            assert!(header.signature == *super::SIGNATURE, "wrong signature");
            assert!(
                header.version == super::PROCESS_CTX_VERSION,
                "wrong context version"
            );
            assert!(
                header.payload_size == payload_v1.len() as u32,
                "wrong payload size"
            );
            assert!(
                header.monotonic_published_at_ns > 0,
                "monotonic_published_at_ns is zero"
            );
            assert!(read_payload == payload_v1.as_bytes(), "payload mismatch");

            let published_at_ns_v1 = header.monotonic_published_at_ns;
            // Ensure the clock advances so the updated timestamp is strictly greater
            std::thread::sleep(std::time::Duration::from_nanos(10));

            super::publish_raw_payload(payload_v2.as_bytes().to_vec())
                .expect("couldn't update the process context");

            let header = read_process_context().expect("couldn't read back the process context");
            // Safety: the published context must have put valid bytes of size payload_size in the
            // context if the signature check succeded.
            let read_payload = unsafe {
                std::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
            };

            assert!(header.signature == *super::SIGNATURE, "wrong signature");
            assert!(
                header.version == super::PROCESS_CTX_VERSION,
                "wrong context version"
            );
            assert!(
                header.payload_size == payload_v2.len() as u32,
                "wrong payload size"
            );
            assert!(
                header.monotonic_published_at_ns > published_at_ns_v1,
                "published_at_ns should be strictly greater after update"
            );
            assert!(read_payload == payload_v2.as_bytes(), "payload mismatch");

            super::unpublish().expect("couldn't unpublish the context");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn unpublish_process_context() {
            let payload = "example process context payload";

            super::publish_raw_payload(payload.as_bytes().to_vec())
                .expect("couldn't publish the process context");

            // The mapping must be discoverable right after publishing
            find_otel_mapping().expect("couldn't find the otel mapping after publishing");

            super::unpublish().expect("couldn't unpublish the context");

            // After unpublishing the name must no longer appear in /proc/self/maps
            assert!(
                find_otel_mapping().is_err(),
                "otel mapping should not be visible after unpublish"
            );
        }
    }
}
