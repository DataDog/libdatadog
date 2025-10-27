What changes between regex and regex-lite
=========================================

Performance loss
----------------

Refer to the [Benchmark](#Benchmark) section for specific numbers.

Artifact size reduction
-----------------------

Refer to the [PR comment](https://github.com/DataDog/libdatadog/pull/1232#issuecomment-3318665873) to evaluate the impact.

Unicode correctness
-------------------

This one needs more detail since it's not just numbers. Basically, unicode edge-cases (especially around multi codepoint characters) contributed a lot to the complexity (and thus size of the automatas).

### What regex-lite still does for Unicode

Both engines fundamentally match Unicode scalar values (code points) in &str haystacks; . matches a full code point (not a single byte) unless you explicitly disable Unicode in regex. regex-lite therefore can match arbitrary non-ASCII characters literally, it just lacks the higher-level Unicode semantics of the following sections. 

```rs
// Literal Unicode still works in both
assert!(regex_lite::Regex::new("ΔδΔ").unwrap().is_match("xxΔδΔyy"));
```


### No Unicode properties (\p{...} / \P{...})

`regex`: Supports Unicode general categories, scripts, script extensions, ages and many boolean properties via `\p{...}` / `\P{...}` (e.g., `\p{Letter}`, `\p{Greek}`, `\p{Emoji}`, `\p{Age:6.0}`), and lets you combine them inside classes.

regex-lite: Does not support `\p{...}`/`\P{...}` at all (patterns using them won't compile). 

```rs
// regex: OK – matches all Greek letters
let re = regex::Regex::new(r"\p{Greek}+").unwrap();
assert!(re.is_match("ΔδΔ"));

// regex-lite: compile error – \p{...} unsupported
let re = regex_lite::Regex::new(r"\p{Greek}+").unwrap(); // ERROR
```

### ASCII-only "Perl classes" (`\w`, `\d`, `\s`) and word boundaries

regex: In Unicode mode (default), `\w`, `\d`, `\s` are Unicode-aware; `\b`/`\B` use Unicode's notion of "word" characters. ASCII-only variants are opt-in via (?-u:...). 

regex-lite: `\w`, `\d`, `\s` are ASCII only (`\w` = [0-9A-Za-z_], `\d` = [0-9], `\s` = [`\t`n``v``f``r ``]). Since `\w` is ASCII-only, word boundaries behave accordingly (i.e., effectively ASCII). 

```rs
// \w on non-ASCII letters
assert!(regex::Regex::new(r"^\w+$").unwrap().is_match("résumé"));     // true (Unicode-aware)
assert!(!regex_lite::Regex::new(r"^\w+$").unwrap().is_match("résumé"));// false (ASCII-only)

// \d on non-ASCII digits (e.g., Devanagari '३')
assert!(regex::Regex::new(r"^\d$").unwrap().is_match("३"));           // true
assert!(!regex_lite::Regex::new(r"^\d$").unwrap().is_match("३"));      // false

// \b word boundary with non-ASCII letters
assert!(regex::Regex::new(r"\bword\b").unwrap().is_match("… wordًا …")); // true
assert!(!regex_lite::Regex::new(r"\bword\b").unwrap().is_match("… wordًا …")); // often false
```

### No Unicode-aware case-insensitive matching

regex: (`?i`) is Unicode-aware (uses Unicode "simple case folding"). E.g., Δ matches δ under (?i). 

regex-lite: (`?i`) is ASCII-only; non-ASCII letters won't fold. 

```rs
assert!(regex::Regex::new(r"(?i)Δ+").unwrap().is_match("ΔδΔ"));      // true
assert!(!regex_lite::Regex::new(r"(?i)Δ+").unwrap().is_match("ΔδΔ")); // false
```

### No Unicode-centric character-class set ops beyond union

regex: Inside \[...\], supports intersection &&, difference --, symmetric difference ~~, and nested classes, very handy with Unicode properties (e.g., Greek letters only). 

regex-lite: Only union is supported; &&, --, ~~ are not. 

```rs
// regex: Greek letters only (Greek ∩ Letter)
let re = regex::Regex::new(r"[\p{Greek}&&\pL]+").unwrap(); // OK
// regex-lite: ERROR – intersection unsupported, and \p{…} unsupported
let re = regex_lite::Regex::new(r"[\p{Greek}&&\pL]+").unwrap(); // ERROR
```

### No "Unicode Perl classes" feature or Unicode word-boundary tables

regex: Its Unicode feature set includes dedicated data for Unicode-aware `\w`, `\s`, `\d` and for Unicode word-boundary logic; these are part of its documented Unicode features. 

regex-lite: Opts out of "robust Unicode support" entirely; there are no Unicode data tables enabling those behaviors. 

```rs
// Unicode whitespace (e.g., NO-BREAK SPACE \u{00A0})
assert!(regex::Regex::new(r"\s").unwrap().is_match("\u{00A0}"));      // true
assert!(!regex_lite::Regex::new(r"\s").unwrap().is_match("\u{00A0}")); // false
```

Benchmark
=========

These benchmarks are split into two categories:
- cgroup_* tests parse Linux cgroup lines like those found in /proc/self/cgroup.
- cdefine_* tests detect C/C++ preprocessor macros or typedefs.

Each category runs three parser implementations, regex, regex_lite, and a hand-rolled parser,  on synthetic datasets of varying size.
The suffixes \_short, \_med, and \_long indicate progressively larger payloads or macro bodies, allowing you to see how each implementation scales with input length.

The reported ns/iter values measure the total time to process the entire dataset once per benchmark iteration, so smaller numbers mean faster throughput.


```sh
$ cargo bench
```

```
    Finished `bench` profile [optimized] target(s) in 0.00s
     Running unittests src/lib.rs (target/release/deps/cgroup_parse_bench-c1b1b28a53195300)

running 18 tests
test benches::cdefine_hand_long        ... bench:   1,356,066.66 ns/iter (+/- 117,849.80)
test benches::cdefine_hand_med         ... bench:   1,329,366.68 ns/iter (+/- 132,407.79)
test benches::cdefine_hand_short       ... bench:   1,286,426.56 ns/iter (+/- 125,846.09)
test benches::cdefine_regex_lite_long  ... bench:  21,279,454.20 ns/iter (+/- 549,742.88)
test benches::cdefine_regex_lite_med   ... bench:  21,216,983.30 ns/iter (+/- 721,263.72)
test benches::cdefine_regex_lite_short ... bench:  20,854,745.80 ns/iter (+/- 783,203.29)
test benches::cdefine_regex_long       ... bench:   2,671,409.38 ns/iter (+/- 69,159.82)
test benches::cdefine_regex_med        ... bench:   2,654,479.15 ns/iter (+/- 97,009.58)
test benches::cdefine_regex_short      ... bench:   2,668,466.60 ns/iter (+/- 67,595.87)
test benches::cgroup_hand_long         ... bench:   1,281,364.55 ns/iter (+/- 57,103.09)
test benches::cgroup_hand_med          ... bench:   1,121,480.21 ns/iter (+/- 41,114.21)
test benches::cgroup_hand_short        ... bench:   1,095,742.20 ns/iter (+/- 44,502.66)
test benches::cgroup_regex_lite_long   ... bench: 256,230,283.40 ns/iter (+/- 3,334,367.42)
test benches::cgroup_regex_lite_med    ... bench: 114,994,416.60 ns/iter (+/- 1,492,667.53)
test benches::cgroup_regex_lite_short  ... bench:  57,620,791.60 ns/iter (+/- 1,935,595.32)
test benches::cgroup_regex_long        ... bench:  20,438,225.10 ns/iter (+/- 797,679.19)
test benches::cgroup_regex_med         ... bench:  10,140,350.00 ns/iter (+/- 607,504.16)
test benches::cgroup_regex_short       ... bench:   5,910,920.80 ns/iter (+/- 278,048.02)

test result: ok. 0 passed; 0 failed; 0 ignored; 18 measured; 0 filtered out; finished in 197.21s
```
