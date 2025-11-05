use antithesis_sdk::{antithesis_init, assert_always, random::AntithesisRng};
use rand::Rng;
use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    antithesis_init();

    let v = Arc::new(Mutex::new(Vec::with_capacity(252)));
    {
        let mut g = v.lock().unwrap();
        g.extend([1_i32, 2, 3]);
    }

    let ptr: *mut i32 = {
        let mut g = v.lock().unwrap();
        let p = g.as_mut_ptr();
        p
    };

    // Background worker: usually harmless; *very* rarely forces a realloc
    let v2 = Arc::clone(&v);
    let handle = thread::spawn(move || {
        let mut r = AntithesisRng;
        const STEPS: usize = 100_000;

        for _ in 0..STEPS {
            let action: u32 = r.gen_range(0..1_000);

            if action < 996 {
                // Mostly reads
                let g = v2.lock().unwrap();
                let _ = g.get(0).zip(g.get(1)).map(|(a, b)| a + b);
                drop(g);
            } else if action < 999 {
                // Small mutations that should stay within capacity
                let mut g = v2.lock().unwrap();
                if action < 998 {
                    g.push(r.gen_range(0..1_000));
                } else {
                    g.pop();
                }
            } else {
                // Forces a reallocation
                // Probability per loop: 1 over 1_000 * 100_000
                if r.gen_ratio(1, 100_000) {
                    let mut g = v2.lock().unwrap();
                    let cap = g.capacity();
                    g.reserve_exact(cap + 1);
                    g.fill_with(|| 42);
                    g.push(42);
                }
            }
        }
    });

    handle.join().unwrap();

    unsafe {
        *ptr = 10;
    }

    let g = v.lock().unwrap();
    assert_always!(g[0] == 10, "the first cell is not 10");
}
