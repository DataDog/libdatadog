// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implementation of the publisher part of the [OTEL process
//! context](https://github.com/open-telemetry/opentelemetry-specification/pull/4719)
//!
//! # A note on race conditions
//!
//! Process context sharing implies concurrently writing to a memory area that another process
//! might be actively reading. However, reading isn't done as direct memory accesses but go through
//! the OS, so the Rust definition of race conditions doesn't really apply.

/// Current version of the process context format
pub const PROCESS_CTX_VERSION: u32 = 2;
/// Signature bytes for identifying process context mappings
pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
/// The discoverable name of the memory mapping.
pub const MAPPING_NAME: &str = "OTEL_CTX";

#[cfg(target_os = "linux")]
#[cfg(target_has_atomic = "64")]
pub mod linux {
    use super::{MAPPING_NAME, PROCESS_CTX_VERSION, SIGNATURE};

    use std::{
        ffi::c_void,
        mem::ManuallyDrop,
        os::fd::AsFd as _,
        ptr::{self, addr_of_mut},
        sync::{
            atomic::{fence, AtomicU64, Ordering},
            Mutex, MutexGuard,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use anyhow::Context;

    use rustix::{
        fs::{ftruncate, memfd_create, MemfdFlags},
        mm::{madvise, mmap, mmap_anonymous, munmap, Advice, MapFlags, ProtFlags},
        param::page_size,
        process::set_virtual_memory_region_name,
    };

    /// The header structure written at the start of the mapping. This must match the C
    /// layout of the specification.
    ///
    /// # Atomic accesses
    ///
    /// The publishing protocol requires some form of synchronization. Using fences or any non-OS
    /// based synchronization requires the use of atomics to have any effect (see [Mandatory
    /// atomic](https://doc.rust-lang.org/std/sync/atomic/fn.fence.html#mandatory-atomic))
    ///
    /// We use `signature` as a release notification for publication, and `published_at_ns` for
    /// updates. Ideally, those should be two `AtomicU64`, but this isn't compatible with
    /// `#[repr(C, packed)]`, since `AtomicU64` can't be used in a packed structure for alignment
    /// reason (what's more, their alignment might be bigger than the one of `u64` on some
    /// platforms).
    ///
    /// In practice, given the page size and the layout of `MappingHeader`, the alignment should
    /// match (we statically test for it anyway). We can then use [`AtomicU64::from_ptr`] to create
    /// an atomic view of those fields when synchronization is needed.
    #[repr(C, packed)]
    struct MappingHeader {
        signature: [u8; 8],
        version: u32,
        payload_size: u32,
        published_at_ns: u64,
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
        /// `memfd` is the preferred method, but this function fallbacks to an anonymous mapping on
        /// old kernels that don't support `memfd` (or if `memfd` failed).
        fn new() -> anyhow::Result<Self> {
            let size = mapping_size();

            memfd_create(
                MAPPING_NAME,
                MemfdFlags::CLOEXEC | MemfdFlags::NOEXEC_SEAL | MemfdFlags::ALLOW_SEALING,
            )
            .or_else(|_| memfd_create(MAPPING_NAME, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING))
            .and_then(|fd| {
                ftruncate(fd.as_fd(), mapping_size() as u64)?;
                // Safety: we pass a null pointer to mmap which is unconditionally ok
                let start_addr = unsafe {
                    mmap(
                        ptr::null_mut(),
                        size,
                        ProtFlags::WRITE | ProtFlags::READ,
                        MapFlags::PRIVATE,
                        fd.as_fd(),
                        0,
                    )?
                };

                // We (implicitly) close the file descriptor right away, but this ok
                Ok(MemMapping {
                    start_addr,
                })
            })
            // If any previous step failed, we fallback to an anonymous mapping
            .or_else(|_| {
                // Safety: we pass a null pointer to mmap, no precondition to uphold
                let start_addr = unsafe {
                    mmap_anonymous(
                        ptr::null_mut(),
                        size,
                        ProtFlags::WRITE | ProtFlags::READ,
                        MapFlags::PRIVATE,
                    )
                    .context(
                        "Couldn't create a memfd or anonymous mmapped region for process context publication",
                    )?
                };

                Ok(MemMapping { start_addr })
            })
        }

        /// Makes this mapping discoverable by giving it a name.
        fn set_name(&mut self) -> anyhow::Result<()> {
            // Safety: the invariants of `MemMapping` ensures that `start` is non null and comes
            // from a previous call to `mmap` of size `mapping_size()`
            set_virtual_memory_region_name(
                unsafe { std::slice::from_raw_parts(self.start_addr as *const u8, mapping_size()) },
                Some(
                    std::ffi::CString::new(MAPPING_NAME)
                        .context("unexpected null byte in process context mapping name")?
                        .as_c_str(),
                ),
            )?;
            Ok(())
        }

        /// Unmaps the underlying memory region and close the memfd file descriptor, if set. This
        /// has same effect as dropping `self`, but propagates potential errors.
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
        /// Practically, `self` must be put in a `ManuallyDrop` wrapper and forgotten.
        unsafe fn unmap(&mut self) -> anyhow::Result<()> {
            unsafe {
                munmap(self.start_addr, mapping_size()).map_err(|errno| {
                    anyhow::anyhow!(
                        "munmap failed when freeing the process context with error {errno}"
                    )
                })
            }
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
    }

    impl ProcessContextHandle {
        /// Initial publication of the process context. Creates an appropriate memory mapping.
        fn publish(payload: Vec<u8>) -> anyhow::Result<Self> {
            let mut mapping = MemMapping::new()?;
            let size = mapping_size();

            // Checks that the layout allow us to access `signature` and `published_at_ns` as
            // atomics u64. Page size is at minimum 4KB and will be always 8 bytes aligned even on
            // exotic platforms. The respective offsets of `signature` and `published_at_ns` are
            // 0 and 8 bytes, so it suffices for `AtomicU64` to require an alignment of at most 8
            // (which is the expected alignment anyway).
            //
            // Note that `align_of` is a `const fn`, so this is in fact a compile-time check and
            // will be optimized away, hence the `allow(unreachable_code)`.
            #[allow(unreachable_code)]
            if std::mem::align_of::<AtomicU64>() > 8 {
                return Err(anyhow::anyhow!("alignment constraints forbid the use of atomics for publishing the protocol context"));
            }

            // Safety: the invariants of MemMapping ensures `start_addr` is not null and comes
            // from a previous call to `mmap`
            unsafe { madvise(mapping.start_addr, size, Advice::LinuxDontFork) }
                .context("madvise MADVISE_DONTFORK failed")?;

            let published_at_ns = time_now_ns().ok_or_else(|| {
                anyhow::anyhow!("failed to get current time for process context publication")
            })?;

            let header = mapping.start_addr as *mut MappingHeader;

            unsafe {
                // Safety: MappingHeader is packed, thus have no alignment requirement. It points
                // to a freshly mmaped region which is valid for writing at least PAGE_SIZE bytes,
                // which is greater than the size of MappingHeader.
                ptr::write(
                    header,
                    MappingHeader {
                        // signature will be set atomically at last
                        signature: [0; 8],
                        version: PROCESS_CTX_VERSION,
                        payload_size: payload
                            .len()
                            .try_into()
                            .context("payload size overflowed")?,
                        published_at_ns,
                        payload_ptr: payload.as_ptr(),
                    },
                );
                // We typically want to avoid the compiler and the hardware to re-order the write to
                // the signature (which should be last according to the
                // specification) with the writes to other fields of the header.
                //
                // To do so, we implement synchronization during publication _as if the reader were
                // another thread of this program_, using atomics and fences.
                AtomicU64::from_ptr((*header).signature.as_mut_ptr().cast::<u64>())
                    // To avoid shuffling bytes, we must use the native endianness
                    .store(u64::from_ne_bytes(*SIGNATURE), Ordering::Release);
                fence(Ordering::SeqCst);
            }

            let _ = mapping.set_name();

            Ok(ProcessContextHandle { mapping, payload })
        }

        /// Updates the context after initial publication.
        fn update(&mut self, payload: Vec<u8>) -> anyhow::Result<()> {
            let header = self.mapping.start_addr as *mut MappingHeader;

            // Note that setting `published_at_ns` to zero doesn't entirely avoid data races with
            // the reader in theory, which could have read a previous non-zero value just before we
            // flipped it but still see subsequent writes. However, since the reader is totally
            // unable to manifest itself to the updating process, we can't have a truly atomic
            // update of the whole header, and is the best we can do.
            let published_at_atomic =
                unsafe { AtomicU64::from_ptr(addr_of_mut!((*header).published_at_ns)) };

            // A process shouldn't try to concurrently update its own context, so this shouldn't
            // really happen.
            if published_at_atomic.swap(0, Ordering::Acquire) == 0 {
                return Err(anyhow::anyhow!(
                    "concurrent update of the process context is not supported"
                ));
            }

            let published_at_ns = time_now_ns()
                .ok_or_else(|| anyhow::anyhow!("could not get the current timestamp"))?;

            self.payload = payload;

            unsafe {
                (*header).payload_ptr = self.payload.as_ptr();
                (*header).payload_size = self.payload.len().try_into().map_err(|_| {
                    anyhow::anyhow!("couldn't update process protocol: new payload too large")
                })?;
            }

            published_at_atomic.store(published_at_ns, Ordering::Release);

            Ok(())
        }
    }

    // Rustix's page_size caches the value in a static atomic, so it's ok to call mapping_size()
    // repeatedly; it won't result in a syscall each time.
    fn mapping_size() -> usize {
        page_size()
    }

    fn time_now_ns() -> Option<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_nanos()).ok())
    }

    /// Locks the context handle. Returns a uniform error if the lock has been poisoned.
    fn lock_context_handle() -> anyhow::Result<MutexGuard<'static, Option<ProcessContextHandle>>> {
        PROCESS_CONTEXT_HANDLER.lock().map_err(|_| {
            anyhow::anyhow!("a thread panicked while operating on the process context handler")
        })
    }

    /// Publishes or updates the process context for it to be visible by external readers.
    ///
    /// If this is the first publication, or if [unpublish] has been called last, this will follow
    /// the Publish protocol of the process context specification.
    ///
    /// Otherwise, the context is updated following the Update protocol.
    pub fn publish(payload: Vec<u8>) -> anyhow::Result<()> {
        let mut guard = lock_context_handle()?;

        match &mut *guard {
            Some(handler) => handler.update(payload),
            None => {
                *guard = Some(ProcessContextHandle::publish(payload)?);
                Ok(())
            }
        }
    }

    /// Unmaps the region used to share the process context and close the associated file
    /// descriptor, if any. If no context has ever been published, this is no-op.
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
    mod tests {
        use super::MappingHeader;
        use anyhow::ensure;
        use std::{
            fs::File,
            io::{BufRead, BufReader},
            sync::atomic::{fence, AtomicU64, Ordering},
        };

        /// Parses the start address from a /proc/self/maps line
        fn parse_mapping_start(line: &str) -> Option<usize> {
            usize::from_str_radix(line.split('-').next()?, 16).ok()
        }

        /// Checks if a mapping line refers to the OTEL_CTX mapping.
        ///
        /// Handles both anonymous naming (`[anon:OTEL_CTX]`) and memfd naming
        /// (`/memfd:OTEL_CTX` which may have ` (deleted)` suffix).
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

        /// Reads the signature from a memory address to verify it's an OTEL_CTX mapping. This also
        /// establish proper synchronization/memory ordering through atomics since the reader is
        /// the same process in this test setup.
        fn verify_signature_at(addr: usize) -> bool {
            let ptr: *mut u64 = std::ptr::with_exposed_provenance_mut(addr);
            // Safety: We're reading from our own process memory at an address
            // we found in /proc/self/maps. This should be safe as long as the
            // mapping exists and has read permissions.
            //
            // The atomic alignment constraints are checked during publication.
            let signature = unsafe { AtomicU64::from_ptr(ptr).load(Ordering::Relaxed) };
            fence(Ordering::SeqCst);
            &signature.to_ne_bytes() == super::SIGNATURE
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
        pub fn read_process_context() -> anyhow::Result<MappingHeader> {
            let mapping_addr = find_otel_mapping()?;
            let header_ptr = mapping_addr as *const MappingHeader;

            // Note: verifying the signature ensures proper synchronization
            ensure!(
                verify_signature_at(mapping_addr),
                "verification of the signature failed"
            );

            // Safety: we found this address in /proc/self/maps and verified the signature
            Ok(unsafe { std::ptr::read(header_ptr) })
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn publish_then_read_context() {
            let payload = "example process context payload";

            super::publish(payload.as_bytes().to_vec())
                .expect("couldn't publish the process context");
            let header = read_process_context().expect("couldn't read back the process contex");
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
                header.payload_size == payload.len() as u32,
                "wrong payload size"
            );
            assert!(header.published_at_ns > 0, "published_at_ns is zero");
            assert!(read_payload == payload.as_bytes(), "payload mismatch");

            super::unpublish().expect("couldn't unpublish the context");
        }
    }
}
