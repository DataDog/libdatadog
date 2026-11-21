// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Request body compression: none, gzip or zstd.
//!
//! gzip is compcol's `embed` deflate, in safe Rust: it allocates nothing
//! itself, so its hash table and output buffer come from the caller's
//! allocator, and it has no panic paths once optimized (the no-panic check
//! holds it to that). zstd is the C library from `zstd-sys`, which cannot panic
//! and allocates only through the hooks here, from the same allocator. Either
//! way, allocation failure is [`EncodeError::OutOfMemory`], not an abort.

use crate::collections::alloc::Allocator;
#[cfg(test)]
use crate::collections::alloc::Global;
use crate::collections::vec::TryVec;
use crate::encode::EncodeError;
#[cfg(feature = "zstd")]
use core::alloc::Layout;
#[cfg(feature = "zstd")]
use core::ffi::{c_int, c_void};
#[cfg(feature = "zstd")]
use core::ptr::{self, NonNull};

/// How a request body is compressed.
///
/// [`Compression::Auto`] leaves the choice to the format: for pprof (which the
/// Datadog Agent forwards to the intake as-is), zstd when built in, else gzip
/// when built in, else none; and none for OTLP (a collector decompresses,
/// processes and enriches the profile, then each exporter picks its own
/// compression).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Compression {
    #[default]
    Auto,
    None,
    /// Available with the `gzip` feature, which `otel` enables.
    Gzip,
    /// Available with the `zstd` feature.
    Zstd,
}

impl Compression {
    /// [`Self::Auto`] resolved for pprof: zstd if built in, else gzip if built
    /// in, else none.
    #[must_use]
    pub const fn for_pprof(self) -> Self {
        match self {
            Self::Auto if cfg!(feature = "zstd") => Self::Zstd,
            Self::Auto if cfg!(feature = "gzip") => Self::Gzip,
            Self::Auto => Self::None,
            other => other,
        }
    }

    /// [`Self::Auto`] resolved for OTLP: none.
    #[must_use]
    pub const fn for_otlp(self) -> Self {
        match self {
            Self::Auto => Self::None,
            other => other,
        }
    }

    /// Whether this build can compress this way.
    #[must_use]
    pub const fn is_available(self) -> bool {
        match self {
            Self::Auto | Self::None => true,
            Self::Gzip => cfg!(feature = "gzip"),
            Self::Zstd => cfg!(feature = "zstd"),
        }
    }

    /// The `Content-Encoding` header value, if any.
    #[must_use]
    pub const fn content_encoding(self) -> Option<&'static str> {
        match self {
            Self::Auto | Self::None => None,
            Self::Gzip => Some("gzip"),
            Self::Zstd => Some("zstd"),
        }
    }
}

/// Entries in gzip's match-finding hash table (16 KiB). On OTLP profiles,
/// 8192 gets within about 1% of the largest table, 65536.
#[cfg(feature = "gzip")]
const GZIP_TABLE: usize = 8192;
/// zstd's fastest regular level, as libdd-profiling v1 used: uploads happen
/// on the profiler's own time, so CPU matters more than the last few bytes.
#[cfg(feature = "zstd")]
const ZSTD_LEVEL: c_int = 1;

/// `body`, compressed (or as-is for [`Compression::None`] and
/// [`Compression::Auto`]; resolve `Auto` first with [`Compression::for_pprof`]
/// or [`Compression::for_otlp`]).
///
/// Everything compression allocates, the output and the compressor's working
/// memory, comes from (a clone of) `body`'s allocator.
///
/// # Errors
///
/// [`EncodeError::CompressionUnavailable`] for a compressor this build lacks,
/// [`EncodeError::OutOfMemory`], [`EncodeError::TooLarge`] for a body too
/// large for zstd, and [`EncodeError::Compression`].
pub fn compress<A: Allocator + Clone>(
    body: TryVec<u8, A>,
    compression: Compression,
) -> Result<TryVec<u8, A>, EncodeError> {
    match compression {
        Compression::Auto | Compression::None => Ok(body),
        #[cfg(feature = "gzip")]
        Compression::Gzip => gzip(body.allocator().clone(), &body),
        #[cfg(feature = "zstd")]
        Compression::Zstd => zstd(body.allocator().clone(), &body),
        #[cfg(not(all(feature = "gzip", feature = "zstd")))]
        _ => Err(EncodeError::CompressionUnavailable),
    }
}

// ---- Allocation hooks -----------------------------------------------------

/// Bytes in front of each block, holding its total size: zstd frees by
/// pointer alone. 16 keeps blocks 16-byte aligned, like malloc's.
#[cfg(feature = "zstd")]
const HEADER: usize = 16;

/// A zstd allocation of `size` bytes from `alloc`, or null.
#[cfg(feature = "zstd")]
fn hook_alloc<A: Allocator>(alloc: &A, size: usize) -> *mut c_void {
    let Some(total) = size.checked_add(HEADER) else {
        return ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, HEADER) else {
        return ptr::null_mut();
    };
    let Ok(block) = alloc.allocate(layout) else {
        return ptr::null_mut();
    };
    let base = block.cast::<u8>();
    // SAFETY: the block holds `total >= HEADER` bytes, 16-byte aligned.
    unsafe {
        base.cast::<usize>().write(total);
        base.add(HEADER).as_ptr().cast()
    }
}

#[cfg(feature = "zstd")]
/// # Safety
///
/// `address` is null or came from [`hook_alloc`] with `alloc` and is not used
/// again.
unsafe fn hook_free<A: Allocator>(alloc: &A, address: *mut c_void) {
    let Some(address) = NonNull::new(address.cast::<u8>()) else {
        return;
    };
    // SAFETY: `hook_alloc` put the block's total size `HEADER` bytes before
    // `address`, and allocated it from `alloc` with this layout.
    unsafe {
        let base = address.sub(HEADER);
        let total = base.cast::<usize>().read();
        alloc.deallocate(base, Layout::from_size_align_unchecked(total, HEADER));
    }
}

/// # Safety
///
/// `opaque` points to the `A` that [`zstd`] passed in `ZSTD_customMem`.
#[cfg(feature = "zstd")]
unsafe extern "C" fn zstd_alloc<A: Allocator>(opaque: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: the caller's contract; the allocator outlives the context.
    hook_alloc(unsafe { &*opaque.cast::<A>() }, size)
}

/// # Safety
///
/// As [`zstd_alloc`], and `address` came from it.
#[cfg(feature = "zstd")]
unsafe extern "C" fn zstd_free<A: Allocator>(opaque: *mut c_void, address: *mut c_void) {
    // SAFETY: zstd frees only what `zstd_alloc` returned, with the same opaque.
    unsafe { hook_free(&*opaque.cast::<A>(), address) }
}

// ---- zstd ---------------------------------------------------------------

#[cfg(feature = "zstd")]
fn zstd<A: Allocator + Clone>(alloc: A, input: &[u8]) -> Result<TryVec<u8, A>, EncodeError> {
    use zstd_sys::{
        ZSTD_CCtx_setParameter, ZSTD_cParameter, ZSTD_compress2, ZSTD_compressBound,
        ZSTD_createCCtx_advanced, ZSTD_customMem, ZSTD_freeCCtx, ZSTD_isError,
    };

    /// Frees the context on every path.
    struct Context(NonNull<zstd_sys::ZSTD_CCtx>);
    impl Drop for Context {
        fn drop(&mut self) {
            // SAFETY: created below and freed only here.
            unsafe { ZSTD_freeCCtx(self.0.as_ptr()) };
        }
    }

    // The context's working memory comes from the same allocator, through a
    // handle of its own that `opaque` points to and zstd passes back to the
    // hooks. It must not move or drop while the context lives: `hooks` is
    // declared before `context`, so it is dropped after it.
    let hooks = alloc.clone();
    let memory = ZSTD_customMem {
        customAlloc: Some(zstd_alloc::<A>),
        customFree: Some(zstd_free::<A>),
        opaque: ptr::from_ref(&hooks).cast_mut().cast(),
    };
    // SAFETY: the hooks and `hooks` are valid for the context's whole life.
    let context = Context(
        NonNull::new(unsafe { ZSTD_createCCtx_advanced(memory) })
            .ok_or(EncodeError::OutOfMemory)?,
    );
    // SAFETY: `context` is live.
    let result = unsafe {
        ZSTD_CCtx_setParameter(
            context.0.as_ptr(),
            ZSTD_cParameter::ZSTD_c_compressionLevel,
            ZSTD_LEVEL,
        )
    };
    // SAFETY: pure function.
    if unsafe { ZSTD_isError(result) } != 0 {
        return Err(EncodeError::Compression);
    }
    // SAFETY: pure function.
    let bound = unsafe { ZSTD_compressBound(input.len()) };
    // SAFETY: pure function.
    if unsafe { ZSTD_isError(bound) } != 0 {
        return Err(EncodeError::TooLarge);
    }
    let mut out: TryVec<u8, A> =
        TryVec::try_with_capacity_in(bound, alloc).map_err(|_| EncodeError::OutOfMemory)?;
    // SAFETY: `out` has `bound` bytes of spare capacity, and `input` is
    // `input.len()` readable bytes.
    let written = unsafe {
        ZSTD_compress2(
            context.0.as_ptr(),
            out.spare_capacity_ptr().cast(),
            bound,
            input.as_ptr().cast(),
            input.len(),
        )
    };
    // SAFETY: pure function.
    if unsafe { ZSTD_isError(written) } != 0 || written > bound {
        return Err(EncodeError::Compression);
    }
    // SAFETY: zstd initialized the first `written <= bound` bytes.
    unsafe { out.set_len(written) };
    Ok(out)
}

// ---- gzip ---------------------------------------------------------------

/// gzip in one call, so matches are found across the whole body.
///
/// compcol takes initialized `&mut` slices for its table and output, which it
/// only writes before reading. Both come from [`TryVec::try_zeroed_in`], so an
/// allocator that hands out fresh zero pages leaves the output's unwritten tail
/// (the bound's slack, which is most of it once the body compresses) untouched.
#[cfg(feature = "gzip")]
fn gzip<A: Allocator + Clone>(alloc: A, input: &[u8]) -> Result<TryVec<u8, A>, EncodeError> {
    // compcol bounds the stream at an eighth more than the input, "plus a few
    // bytes": the gzip header and trailer (18) and the block's framing. A
    // slice is at most `isize::MAX` bytes, so this cannot overflow.
    let bound = input.len() + input.len() / 8 + 64;
    let mut table = TryVec::<u16, _>::try_zeroed_in(GZIP_TABLE, alloc.clone())
        .map_err(|_| EncodeError::OutOfMemory)?;
    let mut out = TryVec::try_zeroed_in(bound, alloc).map_err(|_| EncodeError::OutOfMemory)?;
    let written = compcol::gzip::compress(input, &mut table, &mut out)
        .map_err(|_| EncodeError::Compression)?;
    out.truncate(written);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &[u8]) -> TryVec<u8, Global> {
        let mut body = TryVec::new_in(Global);
        for _ in 0..200 {
            body.try_extend_from_slice(text).unwrap();
        }
        body
    }

    #[test]
    fn none_passes_through() {
        let input = body(b"profile ");
        let out = compress(input.try_clone().unwrap(), Compression::None).unwrap();
        assert_eq!(out, input);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_has_the_gzip_header_and_shrinks() {
        let input = body(b"profile ");
        let out = compress(input.try_clone().unwrap(), Compression::Gzip).unwrap();
        assert_eq!(&out[..3], &[0x1f, 0x8b, 8], "gzip magic and deflate");
        assert!(out.len() < input.len() / 10);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_round_trips() {
        use rust_alloc::vec::Vec;
        use std::io::Read as _;
        // Incompressible and repetitive parts, longer than deflate's window.
        let mut input = TryVec::new_in(Global);
        let mut state = 0x2545_f491_u32;
        for i in 0..200_000_u32 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let byte = if i % 3 == 0 {
                state.to_le_bytes()[0]
            } else {
                b"profile"[usize::try_from(i % 7).unwrap()]
            };
            input.try_push(byte).unwrap();
        }
        let out = compress(input.try_clone().unwrap(), Compression::Gzip).unwrap();
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&out[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(&decoded[..], &input[..]);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_has_the_zstd_magic_and_shrinks() {
        let input = body(b"profile ");
        let out = compress(input.try_clone().unwrap(), Compression::Zstd).unwrap();
        assert_eq!(&out[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd frame magic");
        assert!(out.len() < input.len() / 10);
    }

    #[test]
    fn empty_bodies_compress() {
        for compression in [Compression::Gzip, Compression::Zstd] {
            if compression.is_available() {
                assert!(
                    !compress(TryVec::new_in(Global), compression)
                        .unwrap()
                        .is_empty()
                );
            } else {
                assert_eq!(
                    compress(TryVec::new_in(Global), compression).unwrap_err(),
                    EncodeError::CompressionUnavailable
                );
            }
        }
    }

    #[test]
    fn auto_resolves_per_format() {
        assert_eq!(Compression::Auto.for_otlp(), Compression::None);
        let pprof = if cfg!(feature = "zstd") {
            Compression::Zstd
        } else if cfg!(feature = "gzip") {
            Compression::Gzip
        } else {
            Compression::None
        };
        assert_eq!(Compression::Auto.for_pprof(), pprof);
        assert_eq!(Compression::Gzip.for_pprof(), Compression::Gzip);
        assert_eq!(Compression::None.for_otlp(), Compression::None);
    }
}
