#![feature(test)]
extern crate test;

use std::sync::LazyLock;
use test::black_box;

/* ------------------------------ Common utils ------------------------------ */

#[derive(Clone, Copy)]
struct Prng(u64);

// AI generated random thing
impl Prng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = (self.0 as u32).wrapping_add(0x9E3779B9);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = (self.0 ^ (x as u64)).rotate_left(9);
        x
    }

    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        let i = (self.next_u32() as usize) % xs.len();
        xs[i]
    }
}

// Construction of random payload of certain size
fn hex_len(pr: &mut Prng, n: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        s.push(HEX[(pr.next_u32() & 0xF) as usize] as char);
    }
    s
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/* =============================== CGROUP SET =============================== */

static CGROUP_LINE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\d+:[^:]*:(.+)$").unwrap());

static CGROUP_LINE_REGEX_LITE: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"^\d+:[^:]*:(.+)$").unwrap());

fn gen_valid_cgroup_line(pr: &mut Prng, payload_len: usize) -> String {
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
    format!("{n}:{subs}:{root}{}", hex_len(pr, payload_len.max(1)))
}

fn gen_invalid_cgroup_line(pr: &mut Prng, payload_len: usize) -> String {
    match pr.next_u32() % 6 {
        0 => format!("notanumber:cpu:/docker/{}", hex_len(pr, payload_len)),
        1 => format!("{}-bad", gen_valid_cgroup_line(pr, payload_len)),
        2 => format!("{}:missingcolons", pr.next_u32()),
        3 => format!(
            "{}:{}:{}:extra:colons",
            pr.next_u32(),
            "cpu",
            hex_len(pr, 8)
        ),
        4 => format!(":{}:{}", "cpu", hex_len(pr, payload_len)),
        _ => String::new(),
    }
}

fn make_cgroup_dataset(seed: u64, lines: usize, valid_pc: u32, payload_len: usize) -> Vec<String> {
    let mut pr = Prng::new(seed);
    let mut out = Vec::with_capacity(lines);
    for _ in 0..lines {
        if pr.next_u32() % 100 < valid_pc {
            out.push(gen_valid_cgroup_line(&mut pr, payload_len));
        } else {
            out.push(gen_invalid_cgroup_line(&mut pr, payload_len));
        }
    }
    out
}

static CG_DS_SHORT: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("CG_DS_SHORT_N", 50_000usize);
    let payload = env_or("CG_DS_SHORT_PAYLOAD", 32usize);
    make_cgroup_dataset(0xC0FFEE, n, 70, payload)
});

static CG_DS_MED: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("CG_DS_MED_N", 50_000usize);
    let payload = env_or("CG_DS_MED_PAYLOAD", 96usize);
    make_cgroup_dataset(0xDEADBEEF, n, 70, payload)
});

static CG_DS_LONG: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("CG_DS_LONG_N", 50_000usize);
    let payload = env_or("CG_DS_LONG_PAYLOAD", 256usize);
    make_cgroup_dataset(0xBAD5EED, n, 70, payload)
});

#[inline]
fn cg_parse_regex(line: &str) -> Option<&str> {
    CGROUP_LINE_REGEX
        .captures(line)
        .map(|c| c.get(1).unwrap().as_str())
}

#[inline]
fn cg_parse_regex_lite(line: &str) -> Option<&str> {
    CGROUP_LINE_REGEX_LITE
        .captures(line)
        .and_then(|c| c.get(1).map(|m| m.as_str()))
}

#[inline]
fn cg_parse_hand(line: &str) -> Option<&str> {
    let mut it = line.splitn(3, ':');
    let first = it.next()?;
    let _second = it.next()?;
    let rest = it.next()?;
    if !first.as_bytes().iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(rest)
}

/* =========================== C DEFINE / TYPEDEF =========================== */

static DEF_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::RegexBuilder::new(
        r"^(?:/\*\*(?:[^*]|\*+[^*/])*\*+/\n)?(?:# *(define [a-zA-Z_0-9]+ [^\n]+)|(typedef))",
    )
    .multi_line(true)
    .build()
    .unwrap()
});

/// Same pattern for regex_lite; lite has no builder API, but accepts this syntax.
static DEF_REGEX_LITE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(
        r"^(?:/\*\*(?:[^*]|\*+[^*/])*\*+/\n)?(?:# *(define [a-zA-Z_0-9]+ [^\n]+)|(typedef))",
    )
    .unwrap()
});

fn ident(pr: &mut Prng, min: usize, max: usize) -> String {
    let len = min + (pr.next_u32() as usize % (max - min + 1));
    let mut s = String::with_capacity(len);
    let first = b'A' + (pr.next_u32() % 26) as u8;
    s.push(first as char);
    for _ in 1..len {
        let r = pr.next_u32() % 64;
        let ch = match r {
            0..=25 => b'A' + r as u8,
            26..=51 => b'a' + (r - 26) as u8,
            52..=61 => b'0' + (r - 52) as u8,
            _ => b'_',
        };
        s.push(ch as char);
    }
    s
}

fn doc_comment(pr: &mut Prng, max_body: usize) -> String {
    // Not perfect C syntax, but conforms to the regex's comment sub-language
    let body_len = pr.next_u32() as usize % (max_body + 1);
    let mut s = String::from("/**");
    for _ in 0..body_len {
        match pr.next_u32() % 5 {
            0 => s.push(' '),
            1 => s.push('*'),
            2 => s.push('/'),
            3 => s.push('a'),
            _ => s.push('x'),
        }
    }
    s.push_str("*/\n");
    s
}

fn gen_valid_define_line(pr: &mut Prng, body_len: usize, with_doc: bool) -> String {
    let mut s = String::new();
    if with_doc {
        s.push_str(&doc_comment(pr, 32));
    }
    let spaces = (pr.next_u32() % 4) as usize;
    s.push('#');
    for _ in 0..spaces {
        s.push(' ');
    }
    let name = ident(pr, 3, 12);
    let body = match pr.next_u32() % 4 {
        0 => hex_len(pr, body_len.max(1)),
        1 => format!("{}", (pr.next_u32() % 1000)),
        2 => format!("(x) + {}", pr.next_u32() % 17),
        _ => format!("{} + {}", ident(pr, 3, 8), ident(pr, 3, 8)),
    };
    s.push_str(&format!("define {name} {body}"));
    s
}

fn gen_valid_typedef_line(pr: &mut Prng, body_len: usize, with_doc: bool) -> String {
    let mut s = String::new();
    if with_doc {
        s.push_str(&doc_comment(pr, 32));
    }
    // Simple typedef shapes. the regex only checks the leading token anyway.
    let base = match pr.next_u32() % 5 {
        0 => "unsigned int",
        1 => "long long",
        2 => "struct S",
        3 => "const char *",
        _ => "void (*fn_name)(int)",
    };
    s.push_str(&format!("typedef {base} {}", ident(pr, 3, 12)));
    // add some trailing payload to lengthen line
    for _ in 0..(body_len / 16) {
        s.push_str(" /*x*/");
    }
    s
}

fn gen_invalid_define_line(pr: &mut Prng, body_len: usize) -> String {
    match pr.next_u32() % 8 {
        0 => format!("#{}", ident(pr, 5, 12)),
        1 => format!("# def {}", ident(pr, 3, 10)),
        2 => format!("# define {}", ident(pr, 3, 10)),
        3 => format!("define {} {}", ident(pr, 3, 10), hex_len(pr, body_len)),
        4 => format!("/* unterminated comment\n# define X 1"),
        5 => format!("// comment\n#    def {} {}", ident(pr, 3, 10), 1),
        6 => String::from("#define"),
        _ => String::new(),
    }
}

fn make_define_dataset(seed: u64, lines: usize, valid_pc: u32, body_len: usize) -> Vec<String> {
    let mut pr = Prng::new(seed);
    let mut out = Vec::with_capacity(lines);
    for _ in 0..lines {
        if pr.next_u32() % 100 < valid_pc {
            // 60% defines, 40% typedefs; each optionally prefixed by a doc comment
            let with_doc = (pr.next_u32() % 2) == 0;
            if pr.next_u32() % 5 < 3 {
                out.push(gen_valid_define_line(&mut pr, body_len, with_doc));
            } else {
                out.push(gen_valid_typedef_line(&mut pr, body_len, with_doc));
            }
        } else {
            out.push(gen_invalid_define_line(&mut pr, body_len));
        }
    }
    out
}

static DEF_DS_SHORT: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DEF_DS_SHORT_N", 50_000usize);
    let body = env_or("DEF_DS_SHORT_BODY", 24usize);
    make_define_dataset(0xDEF1DEF1, n, 70, body)
});

static DEF_DS_MED: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DEF_DS_MED_N", 50_000usize);
    let body = env_or("DEF_DS_MED_BODY", 96usize);
    make_define_dataset(0xDEF1BEEF, n, 70, body)
});

static DEF_DS_LONG: LazyLock<Vec<String>> = LazyLock::new(|| {
    let n = env_or("DEF_DS_LONG_N", 50_000usize);
    let body = env_or("DEF_DS_LONG_BODY", 256usize);
    make_define_dataset(0xDEF1FEED, n, 70, body)
});

#[inline]
fn def_parse_regex(line: &str) -> bool {
    DEF_REGEX.is_match(line)
}

#[inline]
fn def_parse_regex_lite(line: &str) -> bool {
    DEF_REGEX_LITE.is_match(line)
}

// This whole "by hand" design is also AI-generated, so it might not be 100%
// correct, but it should be enough to get a rough idea of the complexity and
// performance we would get if a human made it
#[inline]
fn def_parse_hand(line: &str) -> bool {
    let mut s = line;
    if let Some(rest) = strip_one_doc_comment(s) {
        s = rest;
    }
    if s.starts_with('#') {
        let mut i = 1usize;
        while i < s.len() && s.as_bytes()[i] == b' ' {
            i += 1;
        }
        if !s[i..].starts_with("define ") {
            return false;
        }
        let after_kw = &s[i + "define ".len()..];
        // IDENT
        let mut j = 0usize;
        let bytes = after_kw.as_bytes();
        if j >= bytes.len() || !is_ident_start(bytes[j]) {
            return false;
        }
        j += 1;
        while j < bytes.len() && is_ident_continue(bytes[j]) {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b' ' {
            return false;
        }
        j += 1;
        if j >= after_kw.len() {
            return false;
        }
        return !after_kw[j..].is_empty() && !after_kw[j..].contains('\n');
    } else if s.starts_with("typedef") {
        return true;
    }
    false
}

fn strip_one_doc_comment(s: &str) -> Option<&str> {
    if !s.starts_with("/**") {
        return None;
    }
    if let Some(pos) = s.as_bytes()[3..].windows(3).position(|w| w == b"*/\n") {
        let start = 3 + pos + 3;
        return Some(&s[start..]);
    }
    None
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_lowercase() || b == b'_'
}

#[inline]
fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

/* --------------------------------- BENCHES -------------------------------- */

#[cfg(test)]
mod benches {
    use super::*;
    use test::Bencher;

    fn bench_lines_bool(b: &mut Bencher, ds: &Vec<String>, f: fn(&str) -> bool) {
        let lines = as_strs(ds);
        b.iter(|| {
            let mut hits = 0usize;
            for &line in &lines {
                if f(line) {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    }
    fn bench_lines_opt(b: &mut Bencher, ds: &Vec<String>, f: fn(&str) -> Option<&str>) {
        let lines = as_strs(ds);
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

    /* ---------- CGROUP category: cgroup_* ---------- */
    #[bench]
    fn cgroup_regex_short(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_SHORT, cg_parse_regex)
    }
    #[bench]
    fn cgroup_regex_lite_short(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_SHORT, cg_parse_regex_lite)
    }
    #[bench]
    fn cgroup_hand_short(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_SHORT, cg_parse_hand)
    }

    #[bench]
    fn cgroup_regex_med(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_MED, cg_parse_regex)
    }
    #[bench]
    fn cgroup_regex_lite_med(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_MED, cg_parse_regex_lite)
    }
    #[bench]
    fn cgroup_hand_med(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_MED, cg_parse_hand)
    }

    #[bench]
    fn cgroup_regex_long(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_LONG, cg_parse_regex)
    }
    #[bench]
    fn cgroup_regex_lite_long(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_LONG, cg_parse_regex_lite)
    }
    #[bench]
    fn cgroup_hand_long(b: &mut Bencher) {
        bench_lines_opt(b, &CG_DS_LONG, cg_parse_hand)
    }

    /* ---------- C define/typedef category: cdefine_* ---------- */
    #[bench]
    fn cdefine_regex_short(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_SHORT, def_parse_regex)
    }
    #[bench]
    fn cdefine_regex_lite_short(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_SHORT, def_parse_regex_lite)
    }
    #[bench]
    fn cdefine_hand_short(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_SHORT, def_parse_hand)
    }

    #[bench]
    fn cdefine_regex_med(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_MED, def_parse_regex)
    }
    #[bench]
    fn cdefine_regex_lite_med(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_MED, def_parse_regex_lite)
    }
    #[bench]
    fn cdefine_hand_med(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_MED, def_parse_hand)
    }

    #[bench]
    fn cdefine_regex_long(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_LONG, def_parse_regex)
    }
    #[bench]
    fn cdefine_regex_lite_long(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_LONG, def_parse_regex_lite)
    }
    #[bench]
    fn cdefine_hand_long(b: &mut Bencher) {
        bench_lines_bool(b, &DEF_DS_LONG, def_parse_hand)
    }
}
