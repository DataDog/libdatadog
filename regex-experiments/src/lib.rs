#![feature(test)]
extern crate test;

use std::sync::LazyLock;
use test::{black_box, Bencher};

static LINE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\d+:[^:]*:(.+)$").unwrap());

static LINE_REGEX_LITE: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"^\d+:[^:]*:(.+)$").unwrap());

/* ------------------------- Dataset generation ------------------------- */

// Tiny, deterministic PRNG so we don’t pull in rand.
#[derive(Clone, Copy)]
struct Prng(u64);
impl Prng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u32(&mut self) -> u32 {
        // xorshift32-ish (good enough for variety)
        let mut x = (self.0 as u32).wrapping_add(0x9E3779B9);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = (self.0 ^ (x as u64)).rotate_left(9);
        x
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        let idx = (self.next_u32() as usize) % xs.len();
        xs[idx]
    }
}

fn hex_len(pr: &mut Prng, n: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        let nybble = (pr.next_u32() & 0xF) as usize;
        s.push(HEX[nybble] as char);
    }
    s
}

fn gen_valid_line(pr: &mut Prng, payload_len: usize) -> String {
    // produce things like "12:pids:/docker/<hex...>"
    let id = hex_len(pr, payload_len.max(1));
    let subs = pr.pick(&[
        "name=systemd",
        "rdma",
        "pids",
        "hugetlb",
        "net_prio",
        "perf_event",
        "net_cls",
        "freezer",
        "devices",
        "memory",
        "blkio",
        "cpuacct",
        "cpu",
        "cpuset",
    ]);
    let root = pr.pick(&[
        "/docker/",
        "/ecs/",
        "/kubepods/something/pod123/",
        "/user.slice/u-0/",
    ]);
    let n = (pr.next_u32() % 15 + 1) as u32;
    format!("{n}:{subs}:{root}{id}")
}

fn gen_invalid_line(pr: &mut Prng, payload_len: usize) -> String {
    // A handful of shapes that should NOT match ^\d+:[^:]*:(.+)$
    match pr.next_u32() % 6 {
        0 => format!("notanumber:cpu:/docker/{}", hex_len(pr, payload_len)),
        1 => format!("{}-bad", gen_valid_line(pr, payload_len)), // junk suffix
        2 => format!("{}:missingcolons", pr.next_u32()),         // only one colon
        3 => format!(
            "{}:{}:{}:extra:colons",
            pr.next_u32(),
            "cpu",
            hex_len(pr, 8)
        ),
        4 => format!(":{}:{}", "cpu", hex_len(pr, payload_len)), // missing first field
        _ => String::new(),                                      // empty line
    }
}

fn make_dataset(seed: u64, lines: usize, valid_ratio_pc: u32, payload_len: usize) -> Vec<String> {
    let mut pr = Prng::new(seed);
    let mut out = Vec::with_capacity(lines);
    for _ in 0..lines {
        if pr.next_u32() % 100 < valid_ratio_pc {
            out.push(gen_valid_line(&mut pr, payload_len));
        } else {
            out.push(gen_invalid_line(&mut pr, payload_len));
        }
    }
    out
}

// You can tweak sizes via env (read once) without recompiling benches:
fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// Default datasets: ~50k lines each so each bench iteration does real work.
static DS_SHORT: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DS_SHORT_N", 50_000usize);
    let payload = env_or("DS_SHORT_PAYLOAD", 32usize);
    make_dataset(0xC0FFEE, n, 70, payload) // 70% valid
});
static DS_MED: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DS_MED_N", 50_000usize);
    let payload = env_or("DS_MED_PAYLOAD", 96usize);
    make_dataset(0xDEADBEEF, n, 70, payload)
});
static DS_LONG: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DS_LONG_N", 50_000usize);
    let payload = env_or("DS_LONG_PAYLOAD", 256usize);
    make_dataset(0xBAD5EED, n, 70, payload)
});

// lightweight view as &str to avoid re-alloc per loop
fn as_strs(v: &Vec<String>) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/* --------------------------- Parsers under test --------------------------- */

#[inline]
fn parse_line_regex(line: &str) -> Option<&str> {
    LINE_REGEX
        .captures(line)
        .map(|c| c.get(1).unwrap().as_str())
}
#[inline]
fn parse_line_regex_lite(line: &str) -> Option<&str> {
    LINE_REGEX_LITE
        .captures(line)
        .and_then(|c| c.get(1).map(|m| m.as_str()))
}
#[inline]
fn parse_line_hand(line: &str) -> Option<&str> {
    let mut it = line.splitn(3, ':');
    let first = it.next()?;
    let _second = it.next()?;
    let rest = it.next()?;
    if !first.as_bytes().iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(rest)
}

/* -------------------------------- Benches -------------------------------- */

#[cfg(test)]
mod benches {
    use super::*;
    use test::Bencher;

    fn bench_over_dataset(b: &mut Bencher, ds: &Vec<String>, f: fn(&str) -> Option<&str>) {
        let lines = as_strs(ds); // once per bench, outside the iter loop
        b.iter(|| {
            let mut hits = 0usize;
            for &line in &lines {
                if let Some(x) = f(line) {
                    black_box(x);
                    hits += 1;
                }
            }
            black_box(hits);
        });
    }

    // Short payloads (~32 hex chars)
    #[bench]
    fn regex_short(b: &mut Bencher) {
        bench_over_dataset(b, &DS_SHORT, parse_line_regex)
    }
    #[bench]
    fn regex_lite_short(b: &mut Bencher) {
        bench_over_dataset(b, &DS_SHORT, parse_line_regex_lite)
    }
    #[bench]
    fn hand_short(b: &mut Bencher) {
        bench_over_dataset(b, &DS_SHORT, parse_line_hand)
    }

    // Medium (~96)
    #[bench]
    fn regex_med(b: &mut Bencher) {
        bench_over_dataset(b, &DS_MED, parse_line_regex)
    }
    #[bench]
    fn regex_lite_med(b: &mut Bencher) {
        bench_over_dataset(b, &DS_MED, parse_line_regex_lite)
    }
    #[bench]
    fn hand_med(b: &mut Bencher) {
        bench_over_dataset(b, &DS_MED, parse_line_hand)
    }

    // Long (~256)
    #[bench]
    fn regex_long(b: &mut Bencher) {
        bench_over_dataset(b, &DS_LONG, parse_line_regex)
    }
    #[bench]
    fn regex_lite_long(b: &mut Bencher) {
        bench_over_dataset(b, &DS_LONG, parse_line_regex_lite)
    }
    #[bench]
    fn hand_long(b: &mut Bencher) {
        bench_over_dataset(b, &DS_LONG, parse_line_hand)
    }

    // Optional: correctness check once (not benched).
    #[test]
    fn equivalence_sanity() {
        for ds in [&*DS_SHORT, &*DS_MED, &*DS_LONG] {
            for line in ds {
                let a = parse_line_regex(line);
                let b = parse_line_regex_lite(line);
                let c = parse_line_hand(line);
                assert_eq!(a.is_some(), b.is_some(), "regex vs lite: {line}");
                assert_eq!(a.is_some(), c.is_some(), "regex vs hand: {line}");
                if let (Some(x), Some(y), Some(z)) = (a, b, c) {
                    assert_eq!(x, y, "cap mismatch (regex vs lite): {line}");
                    assert_eq!(x, z, "cap mismatch (regex vs hand): {line}");
                }
            }
        }
    }
}
