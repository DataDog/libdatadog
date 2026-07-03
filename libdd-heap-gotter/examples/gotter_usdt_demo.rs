// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sample app exercising `libdd-heap-gotter`: install the GOT overrides,
//! then drive libc's allocator functions directly (`malloc`, `calloc`,
//! `realloc`, `free`, `posix_memalign`, `aligned_alloc`) from multiple
//! threads with a mix of sizes and alignments — including alignments
//! that exceed the sampler's cap and hit the passthrough path.
//!
//! Every allocation is filled with a deterministic per-allocation byte
//! pattern; every free/realloc verifies that pattern first, so a bad
//! header stamp, wrong memmove, or misplaced offset shows up as a loud
//! panic rather than a silent corruption.
//!
//! Run (Linux):
//! ```
//! cargo run --example gotter_usdt_demo -p libdd-heap-gotter
//! ```
//! Options:
//! * `--stress` — keep one CPU core hot between iterations (for CPU profiles).
//! * `--secs N` — exit after N seconds instead of looping forever.
//! * `--threads N` — number of worker threads (default: 4).
//!
//! Attach a tracer in another shell, e.g.:
//! ```
//! sudo bpftrace -p <pid> -e '
//!   usdt:*:ddheap:alloc { @allocs = count(); }
//!   usdt:*:ddheap:free  { @frees  = count(); }
//!   interval:s:1 { print(@allocs); print(@frees); }'
//! ```
//!
//! The gotter crate is Linux-only; on other targets the example
//! compiles to an empty `main` so clippy/test on non-Linux don't fail
//! with "configured out".

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::hint::black_box;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Tracked live allocation: pointer, its user-visible size, and the
    /// seed used to fill it. Content integrity is verified before any
    /// realloc/free.
    struct LiveAlloc {
        ptr: *mut u8,
        size: usize,
        seed: u64,
    }
    // Raw pointers aren't Send by default; we're the sole owner in the
    // worker thread that produced them, so this is fine.
    unsafe impl Send for LiveAlloc {}
    unsafe impl Sync for LiveAlloc {}
    impl LiveAlloc {
        fn as_slice(&self) -> &[u8] {
            // SAFETY: `ptr` is the return of a libc allocator and
            // `size` is the user-requested size. Lifetime is scoped by
            // the caller and does not outlive the allocation.
            unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
        }

        fn as_slice_mut(&mut self) -> &mut [u8] {
            // SAFETY: `ptr` is the return of a libc allocator and
            // `size` is the user-requested size. `&mut self` prevents
            // callers from creating two mutable slices to the same live
            // allocation through this helper.
            unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
        }
    }

    /// Cheap deterministic PRNG (splitmix64). We use it for both
    /// scheduling decisions and content fills so a corruption bug
    /// reproduces deterministically for a given (thread, seed) pair.
    #[derive(Clone, Copy)]
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed)
        }

        fn next(&mut self) -> u64 {
            let mut z = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            self.0 = z;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn range(&mut self, lo: usize, hi: usize) -> usize {
            lo + (self.next() as usize) % (hi - lo).max(1)
        }

        fn choice<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            &xs[(self.next() as usize) % xs.len()]
        }
    }

    /// Fill `buf` with a pattern derived from `seed`. Verified later by
    /// `verify_content` so we notice content shifts or clobbers.
    fn fill_content(buf: &mut [u8], seed: u64) {
        let mut r = Rng::new(seed);
        for chunk in buf.chunks_mut(8) {
            let w = r.next().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&w[..n]);
        }
    }

    fn verify_content(buf: &[u8], seed: u64) {
        let mut r = Rng::new(seed);
        for (i, chunk) in buf.chunks(8).enumerate() {
            let w = r.next().to_le_bytes();
            let n = chunk.len();
            assert_eq!(
                chunk,
                &w[..n],
                "content mismatch at byte offset {} (chunk {})",
                i * 8,
                i,
            );
        }
    }

    /// Weighted alignment menu. Small alignments dominate; a rare 4096
    /// exercises the sampler's alignment cap; 8192 exceeds it and
    /// forces the passthrough path.
    const ALIGNMENTS: &[usize] = &[
        1, 8, 8, 8, 16, 16, 16, 16, 16, 32, 64, 128, 256, 512, 1024, 4096, 8192,
    ];

    fn pick_size(r: &mut Rng) -> usize {
        // Log-uniform bucket, then a uniform offset inside the bucket.
        // Skewed toward small allocs (matches typical workloads) but
        // occasionally reaches into MB territory.
        let bucket = r.range(0, 22);
        let hi = 1usize << bucket;
        let lo = hi / 2;
        r.range(lo.max(1), hi.max(2))
    }

    fn pick_alignment(r: &mut Rng) -> usize {
        *r.choice(ALIGNMENTS)
    }

    unsafe fn do_malloc(size: usize) -> Option<LiveAlloc> {
        let ptr = libc::malloc(size) as *mut u8;
        if ptr.is_null() {
            return None;
        }
        Some(LiveAlloc { ptr, size, seed: 0 })
    }

    unsafe fn do_calloc(nmemb: usize, size: usize) -> Option<LiveAlloc> {
        let ptr = libc::calloc(nmemb, size) as *mut u8;
        if ptr.is_null() {
            return None;
        }
        // calloc zeroes memory; verify that before we overwrite with a
        // seed pattern. Catches allocator confusion between raw and
        // user pointers.
        let total = nmemb.saturating_mul(size);
        let slice = std::slice::from_raw_parts(ptr, total);
        assert!(
            slice.iter().all(|&b| b == 0),
            "calloc returned non-zeroed memory"
        );
        Some(LiveAlloc {
            ptr,
            size: total,
            seed: 0,
        })
    }

    unsafe fn do_aligned_alloc(alignment: usize, size: usize) -> Option<LiveAlloc> {
        // aligned_alloc requires size % alignment == 0. Round up.
        let rounded = size.div_ceil(alignment) * alignment;
        let ptr = libc::aligned_alloc(alignment, rounded) as *mut u8;
        if ptr.is_null() {
            return None;
        }
        assert_eq!(
            (ptr as usize) % alignment,
            0,
            "aligned_alloc returned misaligned pointer"
        );
        Some(LiveAlloc {
            ptr,
            size: rounded,
            seed: 0,
        })
    }

    unsafe fn do_posix_memalign(alignment: usize, size: usize) -> Option<LiveAlloc> {
        // posix_memalign requires alignment to be a power of two and a
        // multiple of sizeof(void*).
        if alignment < std::mem::size_of::<*mut u8>() || !alignment.is_power_of_two() {
            return None;
        }
        let mut out: *mut libc::c_void = std::ptr::null_mut();
        let rc = libc::posix_memalign(&mut out, alignment, size);
        if rc != 0 || out.is_null() {
            return None;
        }
        assert_eq!(
            (out as usize) % alignment,
            0,
            "posix_memalign returned misaligned pointer"
        );
        Some(LiveAlloc {
            ptr: out as *mut u8,
            size,
            seed: 0,
        })
    }

    unsafe fn do_realloc(old: LiveAlloc, new_size: usize) -> Option<LiveAlloc> {
        // Verify old contents before releasing the block.
        verify_content(old.as_slice(), old.seed);
        let new_ptr = libc::realloc(old.ptr as *mut libc::c_void, new_size) as *mut u8;
        if new_ptr.is_null() {
            // Old block is still live on realloc failure. Return it as-is.
            return Some(old);
        }
        // Preserved bytes are `min(old.size, new_size)`; verify them
        // against the OLD seed. Any misplaced offset in the sampler's
        // realloc path shows up as a mismatch here.
        let preserved = old.size.min(new_size);
        let preserved_slice = std::slice::from_raw_parts(new_ptr, preserved);
        // Re-run the deterministic fill to know what those bytes should be.
        let mut expected = vec![0u8; preserved];
        fill_content(&mut expected, old.seed);
        assert_eq!(
            preserved_slice,
            &expected[..],
            "realloc did not preserve user contents"
        );
        Some(LiveAlloc {
            ptr: new_ptr,
            size: new_size,
            seed: 0,
        })
    }

    unsafe fn do_free(a: LiveAlloc) {
        verify_content(a.as_slice(), a.seed);
        libc::free(a.ptr as *mut libc::c_void);
    }

    fn worker(
        thread_id: u64,
        stop: Arc<AtomicBool>,
        allocs: Arc<AtomicU64>,
        frees: Arc<AtomicU64>,
        reallocs: Arc<AtomicU64>,
    ) {
        let mut rng = Rng::new(0xdead_beef_0000_0000 ^ thread_id);
        // Cap the working set to keep total RSS bounded; when full,
        // subsequent alloc ops replace a random slot (freeing it first).
        const MAX_LIVE: usize = 256;
        let mut live: Vec<LiveAlloc> = Vec::with_capacity(MAX_LIVE);

        while !stop.load(Ordering::Relaxed) {
            let op = rng.range(0, 100);
            let want_alloc = live.len() < MAX_LIVE / 2 || op < 40;

            unsafe {
                if want_alloc {
                    let size = pick_size(&mut rng);
                    let alignment = pick_alignment(&mut rng);
                    let flavor = rng.range(0, 4);
                    let a = match flavor {
                        0 => do_malloc(size),
                        1 => {
                            let nmemb = rng.range(1, 32);
                            let each = size.div_ceil(nmemb).max(1);
                            do_calloc(nmemb, each)
                        }
                        2 => do_aligned_alloc(alignment, size.max(alignment)),
                        _ => do_posix_memalign(alignment, size),
                    };
                    let Some(mut a) = a else { continue };
                    a.seed = rng.next();
                    let seed = a.seed;
                    fill_content(a.as_slice_mut(), seed);
                    allocs.fetch_add(1, Ordering::Relaxed);
                    if live.len() == MAX_LIVE {
                        let idx = rng.range(0, live.len());
                        let victim = live.swap_remove(idx);
                        do_free(victim);
                        frees.fetch_add(1, Ordering::Relaxed);
                    }
                    live.push(a);
                } else if !live.is_empty() && op < 80 {
                    // realloc a random live block.
                    let idx = rng.range(0, live.len());
                    let old = live.swap_remove(idx);
                    let new_size = pick_size(&mut rng);
                    let Some(mut resized) = do_realloc(old, new_size) else {
                        continue;
                    };
                    // Refill with a fresh seed — old bytes past
                    // min(old_size,new_size) are undefined per the
                    // realloc contract.
                    resized.seed = rng.next();
                    let seed = resized.seed;
                    fill_content(resized.as_slice_mut(), seed);
                    reallocs.fetch_add(1, Ordering::Relaxed);
                    live.push(resized);
                } else if !live.is_empty() {
                    let idx = rng.range(0, live.len());
                    let victim = live.swap_remove(idx);
                    do_free(victim);
                    frees.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // Drain remaining live allocations, verifying contents.
        while let Some(a) = live.pop() {
            unsafe { do_free(a) };
            frees.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn burn_cpu_for(duration: Duration, mut state: u64) -> u64 {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            state =
                state.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17) ^ 0xbf58_476d_1ce4_e5b9;
            black_box(state);
        }
        state
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        let stress = args.iter().any(|a| a == "--stress");
        let secs: Option<u64> = args
            .windows(2)
            .find(|w| w[0] == "--secs")
            .and_then(|w| w[1].parse().ok());
        let threads: usize = args
            .windows(2)
            .find(|w| w[0] == "--threads")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(4);

        println!(
            "pid={}; pre-install. Attach a tracer on 'usdt:*:ddheap:*'. \
             threads={threads} stress={stress} secs={:?}",
            std::process::id(),
            secs,
        );

        // Baseline noise before install — none of this should emit USDTs.
        {
            let warmup: Vec<String> = (0..16).map(|i| format!("warmup-{i}")).collect();
            println!("pre-install warmup: {} entries", warmup.len());
        }

        std::thread::sleep(Duration::from_secs(2));

        let ok = libdd_heap_gotter::install_heap_overrides();
        println!("install_heap_overrides() -> {ok}");

        let stop = Arc::new(AtomicBool::new(false));
        let allocs = Arc::new(AtomicU64::new(0));
        let frees = Arc::new(AtomicU64::new(0));
        let reallocs = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let stop = Arc::clone(&stop);
                let allocs = Arc::clone(&allocs);
                let frees = Arc::clone(&frees);
                let reallocs = Arc::clone(&reallocs);
                thread::spawn(move || worker(t as u64, stop, allocs, frees, reallocs))
            })
            .collect();

        let deadline = secs.map(|s| Instant::now() + Duration::from_secs(s));
        let mut cpu_state = 0x1234_5678_9abc_def0u64;
        let mut tick = 0u64;
        loop {
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    break;
                }
            }
            if stress {
                cpu_state = burn_cpu_for(Duration::from_secs(1), cpu_state ^ tick);
            } else {
                std::thread::sleep(Duration::from_secs(1));
            }
            tick += 1;
            println!(
                "[{tick}s] allocs={} frees={} reallocs={}",
                allocs.load(Ordering::Relaxed),
                frees.load(Ordering::Relaxed),
                reallocs.load(Ordering::Relaxed),
            );
        }

        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        println!(
            "done. allocs={} frees={} reallocs={}",
            allocs.load(Ordering::Relaxed),
            frees.load(Ordering::Relaxed),
            reallocs.load(Ordering::Relaxed),
        );
    }
} // mod linux
