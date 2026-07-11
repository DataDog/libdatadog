// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Drives `libdd-profiling-heap-sampler`'s `ddheap:alloc`/`ddheap:free` USDT
//! probes from `jemalloc`'s own experimental sampling hooks
//! (`experimental.hooks.prof_sample`/`prof_sample_free`/`prof_backtrace`,
//! exposed by `tikv-jemalloc-ctl`'s `profiling_hooks` feature). This lets an
//! external sampler (e.g. the eBPF full host profiler) piggyback
//! `jemalloc`'s own allocation-sampling decision and clock instead of
//! wrapping the allocator with a second, independent sampler — see
//! [`libdd_profiling_heap_allocator`](../libdd_profiling_heap_allocator) for
//! that alternative.
//!
//! [`install`] also replaces `jemalloc`'s backtrace hook with a no-op:
//! `jemalloc` still decides *whether* to sample at the configured rate, but
//! stops walking the stack itself, since an out-of-process profiler captures
//! its own.
//!
//! Requires `jemalloc` to actually be the process's allocator (e.g. via
//! `tikv-jemallocator`) built with `profiling`/`profiling_hooks` enabled so
//! `opt.prof` is compiled in.
//!
//! # Example
//!
//! ```no_run
//! #[global_allocator]
//! static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
//!
//! fn main() {
//!     libdd_profiling_heap_jemalloc::install().unwrap();
//!     // ... rest of the application ...
//! }
//! ```

#[cfg(target_os = "linux")]
use tikv_jemalloc_ctl::profiling::{
    noop_prof_backtrace_hook, prof_active, prof_reset, set_prof_backtrace_hook,
    set_prof_sample_free_hook, set_prof_sample_hook,
};

/// Resets `jemalloc`'s sampling interval to
/// `libdd_profiling_heap_sampler::DD_SAMPLING_INTERVAL_DEFAULT` (512 KiB),
/// installs `jemalloc`'s `prof_sample`/`prof_sample_free` hooks so that
/// `jemalloc`'s own sampling decision drives `libdd-profiling-heap-sampler`'s
/// `ddheap:alloc`/`ddheap:free` USDTs, installs
/// [`noop_prof_backtrace_hook`](tikv_jemalloc_ctl::profiling::noop_prof_backtrace_hook)
/// so `jemalloc` stops walking the stack for each sample, and sets
/// `prof.active` to `true`.
///
/// That last step matters: the `profiling_hooks` feature bakes in
/// `prof:true,prof_active:false` by default, so `jemalloc`'s profiling
/// machinery is compiled in and installable but inert — no allocation is
/// ever sampled and `prof_sample` never fires — until something sets
/// `prof.active`. Without it, the hooks above install cleanly but silently
/// never run.
///
/// The interval reset is what lets [`hooks::on_prof_sample`] report an exact
/// `weight` instead of an approximation: forcing a known interval means each
/// hook call is one sample against that known interval, matching
/// `libdd-profiling-heap-sampler`'s own `nsamples * interval` weight
/// contract with `nsamples` fixed at `1`. If another consumer calls
/// `prof.reset` after [`install`] runs, the live interval changes underneath
/// this crate and the fixed weight it reports becomes wrong until
/// [`install`] runs again.
///
/// A no-op on non-Linux targets, where `libdd-profiling-heap-sampler`'s USDT
/// emission is unavailable, and when
/// [`libdd_profiling_heap_sampler::heap_sampling_enabled`] returns `false`.
///
/// # Errors
///
/// Returns an error (`ENOENT`) if `opt.prof` is `false` at runtime, e.g.
/// because `MALLOC_CONF` overrode the `prof:true` `profiling_hooks` bakes
/// into the default config. See `tikv_jemalloc_ctl::profiling::prof_reset`.
pub fn install() -> tikv_jemalloc_ctl::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if !libdd_profiling_heap_sampler::heap_sampling_enabled() {
            return Ok(());
        }
        prof_reset(hooks::LG_SAMPLE_INTERVAL)?;
        set_prof_sample_hook(Some(hooks::on_prof_sample))?;
        set_prof_sample_free_hook(Some(hooks::on_prof_sample_free))?;
        set_prof_backtrace_hook(noop_prof_backtrace_hook)?;
        prof_active::write(true)?;
    }
    Ok(())
}

/// Uninstalls the hooks [`install`] set and sets `prof.active` back to
/// `false`, restoring `jemalloc` to the same inert state it was in before
/// [`install`] ran. Most consumers install once at startup and leave the
/// hooks in place for the life of the process; this exists for tests and for
/// consumers that need to toggle the integration off at runtime.
///
/// A no-op on non-Linux targets. See [`install`] for error semantics.
pub fn uninstall() -> tikv_jemalloc_ctl::Result<()> {
    #[cfg(target_os = "linux")]
    {
        prof_active::write(false)?;
        set_prof_sample_hook(None)?;
        set_prof_sample_free_hook(None)?;
    }
    Ok(())
}

// USDT emission (`dd_probe_alloc`/`dd_probe_free`) only exists in
// `libdd-profiling-heap-sampler` on Linux; the crate compiles to an empty
// rlib on every other target (see its `src/lib.rs`).
#[cfg(target_os = "linux")]
mod hooks {
    use std::os::raw::{c_uint, c_void};

    use libdd_profiling_heap_sampler::{
        dd_probe_alloc, dd_probe_free, DD_SAMPLING_INTERVAL_DEFAULT,
    };

    /// `lg_prof_sample` (log2 bytes) equivalent to
    /// `DD_SAMPLING_INTERVAL_DEFAULT`, passed to `prof.reset` by [`super::install`].
    ///
    /// `DD_SAMPLING_INTERVAL_DEFAULT` is a power of two, so this recovers its
    /// exact exponent rather than approximating it.
    pub(super) const LG_SAMPLE_INTERVAL: usize =
        DD_SAMPLING_INTERVAL_DEFAULT.trailing_zeros() as usize;

    /// `libdd-profiling-heap-sampler`'s "unbiased size estimator" weight for
    /// a `jemalloc`-sampled allocation.
    ///
    /// [`super::install`] resets `jemalloc`'s sampling interval to
    /// `DD_SAMPLING_INTERVAL_DEFAULT` before installing this hook, and
    /// `jemalloc` invokes `prof_sample` exactly once per sampling decision
    /// (it has no notion of one hook call representing multiple samples) —
    /// so every call here is `nsamples = 1` against a known `interval`,
    /// matching `libdd-profiling-heap-sampler`'s own `nsamples * interval`
    /// weight contract exactly, with no dependence on the sampled object's
    /// own size.
    const SAMPLE_WEIGHT: u64 = DD_SAMPLING_INTERVAL_DEFAULT as u64;

    /// # Safety
    ///
    /// Only ever invoked by `jemalloc` itself as an
    /// `experimental.hooks.prof_sample` hook.
    pub(super) unsafe extern "C" fn on_prof_sample(
        ptr: *const c_void,
        _size: usize,
        _backtrace: *mut *mut c_void,
        _backtrace_length: c_uint,
        usable_size: usize,
    ) {
        dd_probe_alloc(ptr as *mut c_void, usable_size as u64, SAMPLE_WEIGHT);
    }

    /// # Safety
    ///
    /// Only ever invoked by `jemalloc` itself as an
    /// `experimental.hooks.prof_sample_free` hook.
    pub(super) unsafe extern "C" fn on_prof_sample_free(ptr: *const c_void, _usable_size: usize) {
        dd_probe_free(ptr as *mut c_void);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn install_and_uninstall_round_trip() {
        install().unwrap();
        uninstall().unwrap();
    }
}
