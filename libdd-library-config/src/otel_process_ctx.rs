// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
        fs::File,
        io::{self, BufRead, BufReader},
        mem::ManuallyDrop,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        ptr,
        sync::{
            atomic::{fence, AtomicU64, Ordering},
            Mutex, MutexGuard,
        },
        time::Duration,
    };

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
        any_value, AnyValue, KeyValue, ProcessContext,
    };
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
    /// We use `monotonic_published_at_ns` for synchronization with the reader. `AtomicU64` has the
    /// same in-memory representation as `u64` and is 8-bytes aligned. The field lands at offset 16
    /// in the struct (after 8 bytes of signature + 4 bytes version + 4 bytes payload_size), which
    /// satisfies that alignment on any page-aligned base address.
    #[repr(C)]
    struct MappingHeader {
        signature: [u8; 8],
        version: u32,
        payload_size: u32,
        monotonic_published_at_ns: AtomicU64,
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

    // SAFETY: MemMapping represents ownership over the mapped region. It never leaks or
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
        fn new() -> io::Result<Self> {
            let size = mapping_size();

            try_memfd(MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL | libc::MFD_ALLOW_SEALING)
                .or_else(|_| try_memfd(MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING))
                .and_then(|fd| {
                    // SAFETY: fd is a valid open file descriptor.
                    check_syscall_retval(
                        unsafe {
                            libc::ftruncate(fd.as_raw_fd(), mapping_size() as libc::off_t)
                        },
                        "ftruncate failed"
                    )?;
                    // SAFETY: we pass a null pointer to mmap which is unconditionally ok
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
                    // SAFETY: we pass a null pointer to mmap, no precondition to uphold
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
        fn set_name(&mut self) -> io::Result<()> {
            // SAFETY: self.start_addr is valid for mapping_size() bytes as per MemMapping
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
        fn free(mut self) -> io::Result<()> {
            // SAFETY: We put `self` in a `ManuallyDrop`, which prevents drop and future calls to
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
        unsafe fn unmap(&mut self) -> io::Result<()> {
            check_syscall_retval(
                // SAFETY: upheld by the caller.
                unsafe { libc::munmap(self.start_addr, mapping_size()) },
                "munmap failed when freeing the process context",
            )?;

            Ok(())
        }
    }

    impl Drop for MemMapping {
        fn drop(&mut self) {
            // SAFETY: `self` is being dropped
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
        fn publish(payload: Vec<u8>) -> io::Result<Self> {
            let mut mapping = MemMapping::new()?;
            let size = mapping_size();

            check_syscall_retval(
                // SAFETY: the invariants of MemMapping ensures `start_addr` is not null and comes
                // from a previous call to `mmap`
                unsafe { libc::madvise(mapping.start_addr, size, libc::MADV_DONTFORK) },
                "madvise MADVISE_DONTFORK failed",
            )?;

            let published_at_ns = since_boottime_ns().ok_or_else(|| {
                io::Error::other("failed to get current time for process context publication")
            })?;

            let header = mapping.start_addr as *mut MappingHeader;

            unsafe {
                // SAFETY: header points to a freshly mmaped region valid for at least
                // `mapping_size()` bytes, which we ensure is >= size_of::<MappingHeader>(). The
                // base address is page-aligned, so all fields including `monotonic_published_at_ns`
                // (at offset 16) satisfy their alignment requirements.
                ptr::write(
                    header,
                    MappingHeader {
                        signature: *SIGNATURE,
                        version: PROCESS_CTX_VERSION,
                        payload_size: payload
                            .len()
                            .try_into()
                            .map_err(|_| io::Error::other("payload size overflowed"))?,
                        // will be set atomically at last
                        monotonic_published_at_ns: AtomicU64::new(0),
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
                (*header)
                    .monotonic_published_at_ns
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
                // SAFETY: getpid() is always safe to call.
                pid: unsafe { libc::getpid() },
            })
        }

        /// Updates the context after initial publication.
        fn update(&mut self, payload: Vec<u8>) -> io::Result<()> {
            let header = self.mapping.start_addr as *mut MappingHeader;

            let monotonic_published_at_ns = since_boottime_ns()
                .ok_or_else(|| io::Error::other("could not get the current timestamp"))?;
            let payload_size = payload.len().try_into().map_err(|_| {
                io::Error::other("couldn't update process context: new payload too large")
            })?;

            // SAFETY: the mapping is live and valid; the header pointer is page-aligned which
            // satisfies AtomicU64's alignment.
            let published_at_atomic = unsafe { &(*header).monotonic_published_at_ns };

            // A process shouldn't try to concurrently update its own context
            //
            // Note: be careful of early return while `monotonic_published_at` is still zero, as
            // this would effectively "lock" any future publishing. Move throwing code above this
            // swap, or properly restore the previous value if the former can't be done.
            if published_at_atomic.swap(0, Ordering::Relaxed) == 0 {
                return Err(io::Error::other(
                    "concurrent update of the process context is not supported",
                ));
            }

            fence(Ordering::SeqCst);
            self.payload = payload;

            // SAFETY: we own the mapping, which is live and valid for writes. The header is packed
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
    fn check_mapping_addr(addr: *mut c_void, msg: &'static str) -> io::Result<*mut c_void> {
        if addr == libc::MAP_FAILED {
            let e = io::Error::last_os_error();
            Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
        } else {
            Ok(addr)
        }
    }

    /// Returns `Err` wrapping the current `errno` with `msg` as context if `ret` is negative,
    /// `Ok(ret)` otherwise.
    fn check_syscall_retval(ret: libc::c_int, msg: &'static str) -> io::Result<libc::c_int> {
        if ret < 0 {
            let e = io::Error::last_os_error();
            Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
        } else {
            Ok(ret)
        }
    }

    /// Creates a `memfd` file descriptor with the given name and flags.
    fn try_memfd(name: &CStr, flags: libc::c_uint) -> io::Result<OwnedFd> {
        // We use the raw syscall rather than `libc::memfd_create` because the latter requires
        // glibc >= 2.27, while `syscall()` + `SYS_memfd_create` works with any glibc version.
        check_syscall_retval(
            // SAFETY: name is a valid NUL-terminated string; flags are constant bit flags.
            unsafe {
                libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags as libc::c_long)
                    as libc::c_int
            },
            "memfd_create failed",
        )
        // SAFETY: fd is a valid file descriptor just returned by memfd_create.
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
        // SAFETY: ts is a valid, writable timespec.
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
    fn lock_context_handle() -> io::Result<MutexGuard<'static, Option<ProcessContextHandle>>> {
        PROCESS_CONTEXT_HANDLER.lock().map_err(|_| {
            io::Error::other("a thread panicked while operating on the process context handler")
        })
    }

    /// Parses the start address from a /proc/self/maps line.
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

    /// Find the OTEL_CTX mapping in /proc/self/maps.
    fn find_otel_mapping() -> io::Result<usize> {
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

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "couldn't find the mapping of the OTel process context",
        ))
    }

    /// Reads and decodes the current process's OTel process context.
    pub fn read() -> io::Result<ProcessContext> {
        let mapping_addr = find_otel_mapping()?;
        let header: *mut MappingHeader = ptr::with_exposed_provenance_mut(mapping_addr);

        // SAFETY: we're reading from our own process memory at an address we found in
        // /proc/self/maps. The mapping must be readable if it is listed as the OTel context.
        let published_at = unsafe { (*header).monotonic_published_at_ns.load(Ordering::Relaxed) };
        if published_at == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context is currently being updated",
            ));
        }
        fence(Ordering::SeqCst);

        let (signature, version, payload_size, payload_ptr) =
            // SAFETY: a non-zero published timestamp means the header is initialized.
            if let Some(header) = unsafe { header.as_ref() } {
                (
                    header.signature,
                    header.version,
                    header.payload_size,
                    header.payload_ptr,
                )
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "null process context header",
                ));
            };

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
        if payload_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context payload pointer is null",
            ));
        }

        // SAFETY: the publisher stores a pointer to `payload_size` initialized bytes and keeps that
        // allocation alive until the next update. The timestamp check below detects concurrent
        // updates and discards the read.
        let payload = unsafe { std::slice::from_raw_parts(payload_ptr, payload_size as usize) };
        let context = ProcessContext::decode(payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        fence(Ordering::SeqCst);
        let published_at_after =
            unsafe { (*header).monotonic_published_at_ns.load(Ordering::Relaxed) };
        if published_at != published_at_after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context changed while being read",
            ));
        }

        Ok(context)
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

    fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
    }

    /// Returns the thread-local attribute key map from a decoded process context.
    pub fn threadlocal_attribute_key_map(context: &ProcessContext) -> Option<Vec<String>> {
        let key = "threadlocal.attribute_key_map";

        context
            .resource
            .as_ref()
            .and_then(|resource| find_attr(&resource.attributes, key))
            .or_else(|| find_attr(&context.extra_attributes, key))
            .and_then(string_array)
    }

    /// Reads the current process context and returns its thread-local attribute key map.
    pub fn read_threadlocal_attribute_key_map() -> io::Result<Option<Vec<String>>> {
        Ok(threadlocal_attribute_key_map(&read()?))
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
    pub fn publish(context: &ProcessContext) -> io::Result<()> {
        publish_raw_payload(context.encode_to_vec())
    }

    fn publish_raw_payload(payload: Vec<u8>) -> io::Result<()> {
        let mut guard = lock_context_handle()?;

        // SAFETY: getpid() is always safe to call.
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
    pub fn unpublish() -> io::Result<()> {
        let mut guard = lock_context_handle()?;

        if let Some(ProcessContextHandle { mapping, .. }) = guard.take() {
            mapping.free()?;
        }

        Ok(())
    }

    #[cfg(test)]
    #[serial_test::serial]
    mod tests {
        use std::sync::atomic::Ordering;

        use super::MappingHeader;

        /// Read the process context from the current process.
        ///
        /// This searches `/proc/self/maps` for an OTEL_CTX mapping and decodes its contents.
        ///
        /// **CAUTION**: Note that the reader implemented in this module, as well as the helper
        /// functions it relies on, are specialized for tests (for example, it doesn't check for
        /// concurrent writers after reading the header, because we know they can't be). Do not
        /// extract or use as it is as a generic Rust OTel process context reader.
        fn read_process_context() -> io::Result<MappingHeader> {
            let mapping_addr = super::find_otel_mapping()?;
            let header_ptr: *const MappingHeader = std::ptr::with_exposed_provenance(mapping_addr);
            // SAFETY: the mapping was published by this test before being read.
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
            // SAFETY: the published context must have put valid bytes of size payload_size in the
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
                header.monotonic_published_at_ns.load(Ordering::Relaxed) > 0,
                "monotonic_published_at_ns is zero"
            );
            assert!(read_payload == payload_v1.as_bytes(), "payload mismatch");

            let published_at_ns_v1 = header.monotonic_published_at_ns.load(Ordering::Relaxed);
            // Ensure the clock advances so the updated timestamp is strictly greater
            std::thread::sleep(std::time::Duration::from_nanos(10));

            super::publish_raw_payload(payload_v2.as_bytes().to_vec())
                .expect("couldn't update the process context");

            let header = read_process_context().expect("couldn't read back the process context");
            // SAFETY: the published context must have put valid bytes of size payload_size in the
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
                header.monotonic_published_at_ns.load(Ordering::Relaxed) > published_at_ns_v1,
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
            super::find_otel_mapping().expect("couldn't find the otel mapping after publishing");

            super::unpublish().expect("couldn't unpublish the context");

            // After unpublishing the name must no longer appear in /proc/self/maps
            assert!(
                super::find_otel_mapping().is_err(),
                "otel mapping should not be visible after unpublish"
            );
        }
    }
}
