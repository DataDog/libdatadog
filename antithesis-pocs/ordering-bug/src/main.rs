use antithesis_sdk::{antithesis_init, assert_unreachable};
use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

// Just load to let the cpu schedule things instead of pausing the thread by
// sleeping
fn pseudo_tribonacci(mut n: u64) -> u64 {
    n = black_box(n);

    let mut a = 0u64;
    let mut b = 1u64;
    let mut c = 1u64;

    while n > 0 {
        let next = black_box(a.wrapping_add(b).wrapping_add(c) ^ (a >> 3));
        a = b;
        b = c;
        c = next;
        n -= 1;
    }

    black_box(a ^ b ^ c)
}

fn main() {
    antithesis_init();

    let shared: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let a_shared = Arc::clone(&shared);
    let a = thread::spawn(move || {
        let t = Instant::now();
        let result = pseudo_tribonacci(black_box(5_000_000));
        let elapsed = t.elapsed();
        *a_shared.lock().unwrap() = Some(format!("payload (result={result}, took={:?})", elapsed));
    });

    let main_result = pseudo_tribonacci(black_box(100_000_000));
    println!("Main finished its own work: {}", main_result);

    let b_shared = Arc::clone(&shared);
    let b = thread::spawn(move || {
        if let Some(data) = b_shared.lock().unwrap().as_ref() {
            println!("B: read data = {}", black_box(data.clone()));
        } else {
            assert_unreachable!("A finished before B ooops!");
        }
    });

    let _ = b.join();
    let _ = a.join();
}
