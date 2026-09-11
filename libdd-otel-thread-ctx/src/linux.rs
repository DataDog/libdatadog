// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linux implementation of the thread-level context publisher.
//!
//! This module holds everything that is common to both ownership modes: the TLS symbol and its
//! TLSDESC accessor, the record layout and the raw attach/detach primitives. The mode-specific
//! handle types live in the private `owned` and `shared` submodules, exactly one of which is
//! compiled (see the crate-level documentation).

// Exactly one ownership mode must be selected, otherwise the crate exposes no context type at all.
#[cfg(not(any(feature = "owned-context", feature = "shared-context")))]
compile_error!(
    "No ownership mode selected for otel-thread-ctx. Enable either `owned-context` (the default) \
     or `shared-context`."
);

#[cfg(feature = "owned-context")]
mod owned;
#[cfg(feature = "owned-context")]
pub use owned::*;

// `owned-context` deliberately takes precedence when both modes are requested, so that enabling
// all features of the workspace (which the FFI crate does, and which requires the owned mode)
// still builds. `build.rs` warns about this. The two modes can't coexist: see the documentation of
// the context types about mixing them.
#[cfg(all(feature = "shared-context", not(feature = "owned-context")))]
mod shared;
#[cfg(all(feature = "shared-context", not(feature = "owned-context")))]
pub use shared::*;

use std::{
    mem, ptr,
    sync::atomic::{compiler_fence, AtomicPtr, AtomicU8, Ordering},
};

// Define the thread-local pointer that external readers (e.g. the eBPF profiler) discover via
// the dynamic symbol table. It must be an exported ELF `STT_TLS` object accessed via the
// TLSDESC dialect, as mandated by the OTel thread-level context sharing spec.
//
// Stable `rustc` cannot select the TLS dialect for a `#[thread_local]` static, so we declare
// the symbol directly in assembly (an 8-byte, zero-initialised slot in `.tbss`) and resolve
// its per-thread address through TLSDESC in [`tls_slot`].
core::arch::global_asm!(
    ".section .tbss,\"awT\",@nobits",
    ".globl otel_thread_ctx_v1",
    ".type  otel_thread_ctx_v1, @tls_object",
    ".size  otel_thread_ctx_v1, 8",
    ".balign 8",
    "otel_thread_ctx_v1:",
    ".zero  8",
    ".previous",
);

/// Return the address of the current thread's `otel_thread_ctx_v1` TLS slot, resolved through
/// the TLSDESC dialect.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn tls_slot() -> *mut *mut ThreadContextRecord {
    let ptr: usize;
    // WARNING: keep the assembly below in the canonical compiler-emitted TLSDESC form. Linkers
    // rely on these exact relocation-bearing instruction patterns for TLS relaxation,
    // especially when this crate is linked statically. Harmless-looking rewrites can hide part
    // of the sequence from the linker and produce a partially relaxed access that computes an
    // invalid TLS address.
    //
    // This code match byte-per-byte what clang generates, and this is verified during tests.
    core::arch::asm!(
        "leaq otel_thread_ctx_v1@tlsdesc(%rip), %rax",
        "call *otel_thread_ctx_v1@TLSCALL(%rax)",
        "addq %fs:0, %rax",
        // There is a call instruction, but the whole point of TLSDESC is to use a fast calling
        // convention. GCC's x86-64 port assumes that FLAGS_REG and RAX are changed while all
        // other registers are preserved[^1]. LLVM similarly only clobbers RAX[^2] (and flags).
        // So we don't need to clobber additional registers or to use `clobber_abi` here (which
        // would negate most of the advantage of TLSDESC).
        //
        // [^1]: https://maskray.me/blog/2021-02-14-all-about-thread-local-storage
        // [^2]: https://raw.githubusercontent.com/llvm/llvm-project/main/llvm/lib/Target/X86/X86InstrCompiler.td
        out("rax") ptr,
        options(att_syntax),
    );
    ptr as *mut *mut ThreadContextRecord
}

/// Return the address of the current thread's `otel_thread_ctx_v1` TLS slot, resolved through
/// the TLSDESC dialect.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn tls_slot() -> *mut *mut ThreadContextRecord {
    let ptr: usize;
    // WARNING: do not change the assembly below. See the warning above for amd64, and
    // https://github.com/ARM-software/abi-aa/blob/main/sysvabi64/sysvabi64.rst#general-dynamic.
    // This code match byte-per-byte what clang generates, and this is verified during tests.
    core::arch::asm!(
        "adrp  x0, :tlsdesc:otel_thread_ctx_v1",
        "ldr   x1, [x0, :tlsdesc_lo12:otel_thread_ctx_v1]",
        "add   x0, x0, :tlsdesc_lo12:otel_thread_ctx_v1",
        ".tlsdesccall otel_thread_ctx_v1",
        "blr   x1",
        "mrs   x8, tpidr_el0",
        "add   x0, x8, x0",
        out("x0") ptr,
        out("x1") _,
        out("x8") _,
        out("x30") _,
    );
    ptr as *mut *mut ThreadContextRecord
}

/// Run `f` with an atomic view of the current thread's TLS slot.
///
/// The address calculation goes through the TLSDESC dialect via [`tls_slot`]. The returned
/// address is stable (per thread), so callers should try to do as much work as possible
/// inside a single call.
///
/// The slot is read by an async signal handler. Atomic operations should in general use
/// [Ordering::Relaxed], but modifications to the record might need additional compiler-only
/// fences (see `OwnedThreadContext::update` for an example).
fn with_tls_slot<F, R>(f: F) -> R
where
    F: FnOnce(&AtomicPtr<ThreadContextRecord>) -> R,
{
    const {
        assert!(
            mem::align_of::<AtomicPtr<ThreadContextRecord>>()
                == mem::align_of::<*mut ThreadContextRecord>()
        )
    }

    // Safety: the const assertion above ensures the alignment is correct. The TLS slot is
    // valid for the lifetime of the current thread, and all accesses go through the
    // `AtomicPtr` wrapper.
    let slot = unsafe { AtomicPtr::from_ptr(tls_slot()) };
    f(slot)
}
// We maintain the convention in libdatadog that the `local_root_span_id` attribute key is
// always the very first in the string table, so its key index is guaranteed to be zero.
const ROOT_SPAN_KEY_INDEX: u8 = 0;

/// Maximum size in bytes of the `attrs_data` field.
///
/// Chosen so that the total record size (`28 + MAX_ATTRS_DATA_SIZE`) stays within the 640-byte
/// limit recommended by the spec (the eBPF profiler read limit).
pub const MAX_ATTRS_DATA_SIZE: usize = 612;
/// In-memory layout of a thread-level context.
///
/// **CAUTION**: The structure MUST match exactly the OTel thread-level context specification.
/// It is read by external, out-of-process code. Do not re-order fields or modify in any way,
/// unless you know exactly what you're doing.
///
/// # Synchronization
///
/// Readers are async-signal handlers. The writer is always stopped while a reader runs.
/// Sharing memory with a signal handler still requires some form of synchronization, which is
/// achieved through atomics and compiler fence, using `valid` and/or the TLS slot as
/// synchronization points.
///
/// - The writer stores `valid = 0` *before* modifying fields in-place, guarded by a fence.
/// - The writer stores `valid = 1` *after* all fields are populated, guarded by a fence.
/// - `valid` starts at `1` on construction and is never set to `0` except during an in-place
///   update.
// Note: we don't need to make this struct packed, because it's already designed to avoid
// padding. Moreover, doing so would make it 1-aligned, potentially making access to
// `attrs_data_size` unaligned and thus slower, and prevent us from using `AtomicU8` for
// `valid`. We use const assertions below to verify size and offsets at compile time.
#[repr(C)]
struct ThreadContextRecord {
    /// Trace identifier; all-zeroes means "no trace".
    trace_id: [u8; 16],
    /// Span identifier.
    span_id: [u8; 8],
    /// Whether the record is ready/consistent. Always set to `1` except during in-place update
    /// of the current record.
    valid: AtomicU8,
    /// W3C Trace Context trace-flags byte associated with `trace_id` and `span_id`.
    trace_flags: u8,
    /// Number of populated bytes in `attrs_data`.
    attrs_data_size: u16,
    /// Packed variable-length key-value records.
    ///
    /// It's a contiguous list of blocks with layout:
    ///
    /// 1. 1-byte `key_index`
    /// 2. 1-byte `val_len`
    /// 3. `val_len` bytes of a string value.
    ///
    /// # Size
    ///
    /// Currently, we always allocate the max recommended size. This potentially wastes a few
    /// hundred bytes per thread, but it guarantees that we can modify the context in-place
    /// without (re)allocation in the hot path. Having a hybrid scheme (starting smaller and
    /// resizing up a few times) is not out of the question.
    attrs_data: [u8; MAX_ATTRS_DATA_SIZE],
}

const _: () = {
    assert!(size_of::<ThreadContextRecord>() == 640);
    assert!(mem::offset_of!(ThreadContextRecord, trace_id) == 0);
    assert!(mem::offset_of!(ThreadContextRecord, span_id) == 16);
    assert!(mem::offset_of!(ThreadContextRecord, valid) == 24);
    assert!(mem::offset_of!(ThreadContextRecord, trace_flags) == 25);
    assert!(mem::offset_of!(ThreadContextRecord, attrs_data_size) == 26);
    assert!(mem::offset_of!(ThreadContextRecord, attrs_data) == 28);
};

impl ThreadContextRecord {
    /// Build a record with the given trace id, span id and attributes. The
    /// `local_root_span_id` is a distinguished attribute with special handling for
    /// convenience, but it ends up as other attributes in `attrs_data`.
    pub fn new(
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) -> Self {
        let mut record = Self {
            trace_id,
            span_id,
            trace_flags,
            ..Default::default()
        };
        record.set_attrs(local_root_span_id, attrs);
        record
    }

    /// Encode `attributes` into `self.attrs_data` as packed key-value records. Existing data
    /// are overridden (and if there were more entries than `attributes.len()`, they aren't
    /// zeroed, but they will be ignored by readers).
    ///
    /// # Return
    ///
    /// Returns `true` if all attributes were properly encoded, or `false` if some of the data
    /// needed to be dropped. See Size limits below.
    ///
    /// # Arguments
    ///
    /// Each input entry is a pair of a 1-byte `key_index` and a string value.
    ///
    /// # Size limits
    ///
    /// Any value over 255 bytes will be capped at this size. If the total size of the encoded
    /// attributes is over [MAX_ATTRS_DATA_SIZE], extra attributes are ignored. We do this
    /// instead of raising an error because we encode the attributes on-the-fly. Proper error
    /// recovery would require us to be able to rollback to the previous attributes which would
    /// hurt the happy path, or leave the record in an inconsistent state. Another possibility
    /// would be to error out and reset the record in that situation.
    fn set_attrs(&mut self, local_root_span_id: [u8; 8], attributes: &[(u8, &str)]) -> bool {
        let mut fully_encoded = true;

        const { assert!(MAX_ATTRS_DATA_SIZE >= 18) }
        // The local root span id is provided as raw bytes (can be seen as a big-endian u64),
        // but readers will expect a string hex representation. We convert it to a fixed
        // 16-characters string in the usual lowercase hex format.
        //
        // There's currently no easy way to use Rust format capabilities to write directly in a
        // fixed-size array. Since the conversion is simple, we do it manually.
        const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
        self.attrs_data[0] = ROOT_SPAN_KEY_INDEX;
        self.attrs_data[1] = 16;
        for (i, &byte) in local_root_span_id.iter().enumerate() {
            self.attrs_data[2 + i * 2] = HEX_DIGITS[(byte >> 4) as usize];
            self.attrs_data[2 + i * 2 + 1] = HEX_DIGITS[(byte & 0xF) as usize];
        }

        let mut offset = 18;

        for &(key_index, val) in attributes {
            let val_bytes = val.as_bytes();
            let val_len = u8::try_from(val_bytes.len()).unwrap_or_else(|_| {
                fully_encoded = false;
                u8::MAX
            });
            let entry_size = 2 + val_len as usize;

            if offset + entry_size > MAX_ATTRS_DATA_SIZE {
                fully_encoded = false;
                break;
            }

            self.attrs_data[offset] = key_index;
            self.attrs_data[offset + 1] = val_len;
            self.attrs_data[offset + 2..offset + 2 + val_len as usize]
                .copy_from_slice(&val_bytes[..val_len as usize]);
            offset += entry_size;
        }

        // `offset < MAX_ATTRS_DATA_SIZE`, which guarantees it fits in a `u16`. This also
        // effectively hides the remaining of the previous `attrs` bytes, so we don't have to
        // zero them.
        self.attrs_data_size = offset as u16;
        fully_encoded
    }

    /// Publish `record_ptr` into the current thread's TLS slot and return the raw pointer to
    /// the previously attached record (null if none).
    ///
    /// This is the low-level attach primitive shared by `OwnedThreadContext` and
    /// `SharedThreadContext`. It only moves raw pointers in and out of the slot; the caller
    /// is responsible for the ownership semantics of both the passed-in pointer (which is now
    /// published and must be kept alive until detached) and the returned one (which must be
    /// reclaimed into an owning handle to avoid leaks).
    ///
    /// # Memory ordering
    ///
    /// [^tls-slot-ordering]: since we get back the previous record, we could in principle use
    /// an `Acquire` (thus combining into an `AcqRel`) compiler fence to make sure we don't get
    /// back a not-yet-initialized record. However, this thread (excluding the reader signal
    /// handler) is the only one to ever _write_ to the slot, so the store we load the value
    /// from automatically happens-before (because it's sequenced-before) the swap. We still
    /// need a release fence to avoid exposing uninitialized memory to the handler.
    fn attach_raw(record_ptr: *mut ThreadContextRecord) -> *mut ThreadContextRecord {
        compiler_fence(Ordering::Release);
        with_tls_slot(|slot| slot.swap(record_ptr, Ordering::Relaxed))
    }

    /// Detach the current record from the current thread's TLS slot, returning the raw pointer
    /// to the previously attached record (null if none).
    ///
    /// This is the low-level detach primitive shared by `OwnedThreadContext` and
    /// `SharedThreadContext`. As with [`Self::attach_raw`], the caller is responsible for
    /// releasing the returned memory.
    fn detach_raw() -> *mut ThreadContextRecord {
        // We don't need any fence here, see [^tls-slot-ordering].
        with_tls_slot(|slot| slot.swap(ptr::null_mut(), Ordering::Relaxed))
    }
}

impl Default for ThreadContextRecord {
    fn default() -> Self {
        Self {
            trace_id: [0u8; 16],
            span_id: [0u8; 8],
            // We only ever set `valid` to `0` during in-place update of an attached context.
            valid: AtomicU8::new(1),
            trace_flags: 0,
            attrs_data_size: 0,
            attrs_data: [0u8; MAX_ATTRS_DATA_SIZE],
        }
    }
}

/// A thread-level context.
///
/// This is the public, value-level view of a thread context. It is a thin, transparent wrapper
/// around the internal record layout, intentionally hiding the underlying structure (which
/// requires care to manipulate: async-signal-safety, the seq-lock-like update protocol, etc.).
#[repr(transparent)]
#[derive(Default)]
pub struct ThreadContext(ThreadContextRecord);

impl ThreadContext {
    /// Create a new thread context with the given trace/span IDs and encoded attributes.
    #[inline]
    pub fn new(
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) -> Self {
        Self(ThreadContextRecord::new(
            trace_id,
            span_id,
            trace_flags,
            local_root_span_id,
            attrs,
        ))
    }
}

/// Read the TLS pointer for the current thread (the value stored in the TLS slot, not the address
/// of the slot itself). Shared by the tests of both ownership modes.
#[cfg(test)]
fn read_tls_context_ptr() -> *const ThreadContextRecord {
    with_tls_slot(|slot| slot.load(Ordering::Relaxed))
}
