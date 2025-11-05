use std::thread;

use antithesis_sdk::{antithesis_init, assert_always};

// This is UB because static mut can be read/written from multiple threads without synchronization.
static mut HITS: u64 = 0;

fn main() {
    antithesis_init();

    // Pretend we're counting requests handled by a pool of workers.
    const WORKERS: usize = 8;
    const INCREMENTS_PER_WORKER: u64 = 1_000;

    let mut handles = Vec::with_capacity(WORKERS);

    for _id in 0..WORKERS {
        let handle = thread::spawn(move || {
            for _ in 0..INCREMENTS_PER_WORKER {
                unsafe {
                    // A realistic mistake: read-modify-write on shared data with no synchronization
                    HITS = HITS.wrapping_add(1);
                }
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().expect("worker panicked");
    }

    let expected = WORKERS as u64 * INCREMENTS_PER_WORKER;
    let observed = unsafe { HITS };

    assert_always!(
        expected == observed,
        "Expected total: {expected}\nObserved total: {observed}"
    );
    println!("Expected total: {expected}");
    println!("Observed total: {observed}");
    if observed != expected {
        eprintln!(
            "⚠️ Data race detected: lost {} updates (and technically UB).",
            expected - observed
        );
    }
}
