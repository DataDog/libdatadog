// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implementation of the Linux parts of the [OTEL process
//! context specification](https://github.com/open-telemetry/opentelemetry-specification/pull/4719).
//!
//! The update/read protocol is seqlock-style: the publisher marks the mapping as unavailable,
//! writes the payload metadata, publishes a non-zero version, and readers accept a copy only if
//! the version they observed before copying still matches afterward. The general algorithm and
//! the C++ memory-model constraints are described in Boehm's
//! [Can Seqlocks Get Along With Programming Language Memory Models?](https://web.archive.org/web/20211106170334/https://www.hpl.hp.com/techreports/2012/HPL-2012-68.pdf).
//! Linux has its own [seqlock/seqcount implementation](https://github.com/torvalds/linux/blob/master/include/linux/seqlock.h),
//! but its barriers are specified by the Linux kernel memory model, not by the C++/Rust models.
//!
//! This implementation differs from the usual odd/even counter form in two ways: `0` is the
//! in-progress sentinel, and each non-zero `monotonic_published_at_ns` value is the
//! reader-visible version. Updates force that timestamp to advance so readers can detect torn
//! reads even when the clock returns the same value twice. Concurrent writers are rejected, and
//! retry policy is left to the reader's caller.

#[cfg(target_os = "linux")]
#[cfg(target_has_atomic = "64")]
pub mod linux {
    use core::{
        convert::TryInto,
        ffi::{c_void, CStr},
        mem::{size_of, swap, ManuallyDrop},
        ptr::{self, NonNull},
        sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, Ordering},
        time::Duration,
    };
    use std::{
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        sync::{Mutex, MutexGuard},
    };

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext;
    use prost::Message;

    mod self_reader;
    pub use self_reader::ProcessContextSelfReader;

    /// Current version of the process context format
    pub const PROCESS_CTX_VERSION: u32 = 2;
    /// Signature bytes for identifying process context mappings
    pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
    /// The discoverable name of the memory mapping.
    pub const MAPPING_NAME: &CStr = c"OTEL_CTX";
    /// Sentinel timestamp indicating that the context is unpublished or being updated.
    const UNPUBLISHED_OR_UPDATING: u64 = 0;

    /// The header structure written at the start of the mapping. This must match the C
    /// layout of the specification.
    ///
    /// Header fields intentionally use the plain C layout types specified by OTel. There are no
    /// atomics here: publication relies on naturally atomic aligned word-sized accesses on the
    /// supported Linux architectures, plus explicit fences to constrain store/load ordering.
    #[repr(C)]
    struct MappingHeader {
        signature: [u8; 8],
        version: u32,
        payload_size: AtomicU32,
        monotonic_published_at_ns: AtomicU64,
        payload_ptr: AtomicPtr<u8>,
    }

    #[repr(C)]
    struct MappingHeaderSnapshot {
        signature: [u8; 8],
        version: u32,
        payload_size: u32,
        monotonic_published_at_ns: u64,
        payload_ptr: *const u8,
    }

    // Compile-time verification that MappingHeader matches the field offsets and total size
    // mandated by the OTel process context spec:
    // https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md
    const _: () = {
        use core::mem::{offset_of, size_of};
        assert!(offset_of!(MappingHeader, signature) == 0);
        assert!(offset_of!(MappingHeader, version) == 8);
        assert!(offset_of!(MappingHeader, payload_size) == 12);
        assert!(offset_of!(MappingHeader, monotonic_published_at_ns) == 16);
        assert!(offset_of!(MappingHeader, payload_ptr) == 24);
        assert!(size_of::<MappingHeader>() == 32);
        assert!(core::mem::align_of::<MappingHeader>() == 8);
        assert!(size_of::<*const u8>() == size_of::<libc::c_ulong>());
    };

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
        start_addr: NonNull<c_void>,
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
        ///
        /// Both allocation paths produce zero-filled memory: `MAP_ANONYMOUS` mappings are
        /// initialized to zero, and the memfd path maps a newly-created file extended by
        /// `ftruncate()`, whose extended bytes read as `\0`. This matters because a memfd-backed
        /// mapping is discoverable before `set_name()` runs, so early readers may race with header
        /// initialization. They must observe [`UNPUBLISHED_OR_UPDATING`] (0) and stop until the
        /// final timestamp store publishes the initialized header.
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
                    // int prctl(PR_SET_VMA, long attr, unsigned long addr, unsigned long size,
                    // const char *_Nullable val);
                    libc::prctl(
                        libc::PR_SET_VMA,
                        libc::PR_SET_VMA_ANON_NAME as libc::c_ulong,
                        TryInto::<libc::c_ulong>::try_into(self.start_addr.as_ptr() as usize)
                            .expect("start addr overflowed"),
                        TryInto::<libc::c_ulong>::try_into(mapping_size())
                            .expect("mapping size overflowed"),
                        MAPPING_NAME.as_ptr(),
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
                unsafe { libc::munmap(self.start_addr.as_ptr(), mapping_size()) },
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
        /// `payload` might be read by external processes and thus must not move (e.g. by resizing
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
            let payload_size: u32 = payload
                .len()
                .try_into()
                .map_err(|_| io::Error::other("payload size overflowed"))?;

            let mut mapping = MemMapping::new()?;
            let size = mapping_size();
            check_syscall_retval(
                // SAFETY: the invariants of MemMapping ensures `start_addr` is not null and comes
                // from a previous call to `mmap`
                unsafe { libc::madvise(mapping.start_addr.as_ptr(), size, libc::MADV_DONTFORK) },
                "madvise MADVISE_DONTFORK failed",
            )?;

            let published_at_ns = since_boottime_ns().ok_or_else(|| {
                io::Error::other("failed to get current time for process context publication")
            })?;

            let header = mapping.start_addr.as_ptr() as *mut MappingHeader;

            // SAFETY: header points to a zero-filled, page-aligned mapping of at least
            // mapping_size() bytes; field projections are in-bounds and aligned.
            // The pointer writes do not happen while there are live &MappingHeader references
            // and, to the extent the atomic stores do, this is fine because the mutated bytes
            // are inside UnsafeCells.
            unsafe {
                ptr::addr_of_mut!((*header).signature).write(*SIGNATURE);
                ptr::addr_of_mut!((*header).version).write(PROCESS_CTX_VERSION);
                (*header)
                    .payload_size
                    .store(payload_size, Ordering::Relaxed);
                (*header)
                    .payload_ptr
                    .store(payload.as_ptr().cast_mut(), Ordering::Relaxed);

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
            let header = self.mapping.start_addr.as_ptr() as *mut MappingHeader;

            let monotonic_published_at_ns = since_boottime_ns()
                .ok_or_else(|| io::Error::other("could not get the current timestamp"))?;
            let payload_size: u32 = payload.len().try_into().map_err(|_| {
                io::Error::other("couldn't update process context: new payload too large")
            })?;
            // A process shouldn't try to concurrently update its own context.
            //
            // `UNPUBLISHED_OR_UPDATING` is an out-of-band sentinel, not a value that
            // `CLOCK_BOOTTIME` is expected to produce after publication. Published non-zero
            // timestamp values must advance monotonically; the field may temporarily hold the
            // sentinel while an update is in progress.
            //
            // Note: be careful of early return while `monotonic_published_at` is still zero, as
            // this would effectively "lock" any future publishing. Move throwing code above this
            // swap, or properly restore the previous value if the former can't be done.
            // SAFETY: the mapping is live and valid for writes.
            // Note: this does not use CAS because we assume the write lock is being held by the
            // caller.
            let previous_published_at_ns = unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .swap(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed)
            };
            // should never happen (publish() and the several update() calls are serialized by the
            // lock)
            if previous_published_at_ns == UNPUBLISHED_OR_UPDATING {
                panic!("concurrent update of the process context is not supported");
            }

            // The timestamp also acts as the seqlock version, so it must advance even if the
            // clock source returns the same value for two rapid updates.
            let monotonic_published_at_ns =
                monotonic_published_at_ns.max(previous_published_at_ns.saturating_add(1));

            // Pair this with the reader's SeqCst fence before its second timestamp copy. If a
            // reader starts from the previous non-zero timestamp but copies data after this update
            // begins, it must not accept that copy as the previous version: its final timestamp
            // check should see `UNPUBLISHED_OR_UPDATING` or the later published timestamp.
            // Note: only needs
            fence(Ordering::SeqCst);
            self.payload = payload;

            // SAFETY: the mapping is live and valid, and the global mutex prevents concurrent
            // in-process writers from mutating the plain header fields.
            unsafe {
                (*header)
                    .payload_ptr
                    .store(self.payload.as_ptr().cast_mut(), Ordering::Relaxed);
                (*header)
                    .payload_size
                    .store(payload_size, Ordering::Relaxed);
            }

            fence(Ordering::SeqCst);
            // SAFETY: same as above.
            unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .store(monotonic_published_at_ns, Ordering::Relaxed);
            }

            Ok(())
        }
    }

    /// Returns `Err` wrapping the current `errno` with `msg` as context if `addr` equals
    /// `MAP_FAILED`, `Ok(addr)` otherwise.
    fn check_mapping_addr(addr: *mut c_void, msg: &'static str) -> io::Result<NonNull<c_void>> {
        if addr == libc::MAP_FAILED {
            let e = io::Error::last_os_error();
            Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
        } else {
            // SAFETY: mmap returns a non-null pointer on success.
            Ok(unsafe { NonNull::new_unchecked(addr) })
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
                swap(&mut local_handler, handler);
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

        if let Some(ProcessContextHandle {
            mapping, payload, ..
        }) = guard.take()
        {
            // Mark the context as unavailable before freeing the mapping/payload. The fence forces
            // the writing CPU not to reorder the unavailable timestamp store and the deallocation
            // stores. This gives readers more of a chance (but no guarantee) to observe an
            // unavailable context before the mapping is removed.
            //
            // SAFETY: the mapping is still live and valid, and the global mutex prevents
            // concurrent in-process writers from mutating the plain header fields.
            let header = mapping.start_addr.as_ptr() as *mut MappingHeader;
            unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .store(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed);
            }
            fence(Ordering::SeqCst);

            mapping.free()?; // payload will still drop if it fails
                             // but we'll be stuck with a zero timestamp
            drop(payload);
        }

        Ok(())
    }

    #[cfg(test)]
    #[serial_test::serial]
    mod tests {
        use core::{ptr, time::Duration};
        use std::io;

        use super::ProcessContext;
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
            any_value, AnyValue, KeyValue,
        };
        use prost::Message;

        /// Read the process context from the current process.
        ///
        /// This searches `/proc/self/maps` for an OTEL_CTX mapping and decodes its contents.
        ///
        /// **CAUTION**: Note that the reader implemented in this module, as well as the helper
        /// functions it relies on, are specialized for tests (for example, it doesn't check for
        /// concurrent writers after reading the header, because we know they can't be). Do not
        /// extract or use as it is as a generic Rust OTel process context reader.
        fn read_process_context() -> io::Result<super::MappingHeaderSnapshot> {
            let mapping_addr = super::ProcessContextSelfReader::find_otel_mapping()?;
            let header_ptr: *const super::MappingHeaderSnapshot =
                ptr::with_exposed_provenance(mapping_addr);
            // SAFETY: the mapping was published by this test before being read; the tests are
            // serial and don't update the mapping while this header is copied.
            Ok(unsafe { ptr::read(header_ptr) })
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn publish_then_read_process_context() {
            let context = ProcessContext {
                resource: None,
                extra_attributes: vec![KeyValue {
                    key: "service.name".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("checkout".to_string())),
                    }),
                    key_ref: 0,
                }],
            };

            super::publish(&context).expect("couldn't publish the process context");
            let header = read_process_context().expect("couldn't read back the process context");
            // SAFETY: the published context must have put valid bytes of size payload_size in the
            // context if the signature check succeded.
            let read_payload = unsafe {
                core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
            };
            let read_context =
                ProcessContext::decode(read_payload).expect("couldn't decode the process context");
            super::unpublish().expect("couldn't unpublish the context");

            assert!(header.signature == *super::SIGNATURE, "wrong signature");
            assert!(
                header.version == super::PROCESS_CTX_VERSION,
                "wrong context version"
            );
            assert!(
                header.monotonic_published_at_ns > 0,
                "monotonic_published_at_ns is zero"
            );
            assert!(read_context == context, "read back a different context");
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
                core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
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
            std::thread::sleep(Duration::from_nanos(10));

            super::publish_raw_payload(payload_v2.as_bytes().to_vec())
                .expect("couldn't update the process context");

            let header = read_process_context().expect("couldn't read back the process context");
            // SAFETY: the published context must have put valid bytes of size payload_size in the
            // context if the signature check succeded.
            let read_payload = unsafe {
                core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
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
            super::ProcessContextSelfReader::find_otel_mapping()
                .expect("couldn't find the otel mapping after publishing");

            super::unpublish().expect("couldn't unpublish the context");

            // After unpublishing the name must no longer appear in /proc/self/maps
            assert!(
                super::ProcessContextSelfReader::find_otel_mapping().is_err(),
                "otel mapping should not be visible after unpublish"
            );
        }
    }
}
