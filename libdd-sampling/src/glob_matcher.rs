// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::bounded_byte_cache::{BoundedByteCache, DEFAULT_MAX_BYTES, DEFAULT_MAX_ENTRIES};
use libdd_common::MutexExt;
use std::fmt;
use std::sync::{Arc, Mutex};

/// A backtracking implementation of the glob matching algorithm.
///
/// The glob pattern language supports `*` as a multiple character wildcard (including empty string)
/// and `?` as a single character wildcard (not including empty string). The match is case
/// insensitive.
///
/// # Performance
///
/// The matcher distinguishes two paths:
///
/// - **ASCII fast path** (`pattern` and `subject` both ASCII): matches in place on the byte slices
///   using `eq_ignore_ascii_case` semantics. No-wildcard patterns skip the cache (a hash lookup on
///   ~10 bytes costs more than the comparison itself). Wildcard patterns consult the shared LRU
///   cache keyed on the raw subject bytes, because backtracking on adversarial inputs can be O(P·S)
///   and repeated calls with the same subject are the common case.
/// - **Unicode fallback** (either side contains non-ASCII): lowercases the subject with
///   `str::to_lowercase` (one right-sized allocation, SIMD ASCII prefix) and consults the same
///   shared LRU cache keyed on the lowercased bytes.
///
/// # Complexity
///
/// The matching algorithm is the classic two-pointer backtracking glob, with a worst case of
/// `O(P * S)` where `P` is the pattern length and `S` is the subject length. This worst case
/// only occurs for adversarially crafted patterns with many `*` separated by literals that
/// almost match (e.g. `a*a*a*a*b` against `aaaaaaaa...`); in practice, the runtime is close to
/// `O(P + S)` for the kinds of patterns and subjects produced by sampling rules.
pub struct GlobMatcher {
    /// Lowercased pattern for case-insensitive matching.
    pattern_lower: String,
    /// Whether the pattern is pure ASCII. Computed once at construction.
    pattern_is_ascii: bool,
    /// Whether the pattern contains any `*` or `?` wildcard. Computed once at construction.
    pattern_has_wildcards: bool,
    /// Whether the pattern is exactly `*` (matches anything).
    pattern_is_star: bool,
    /// Shared LRU cache of matched subjects, keyed on raw bytes (ASCII path) or lowercased
    /// UTF-8 (Unicode path). Only consulted for wildcard patterns. Bounded by byte size to
    /// cap memory under arbitrary key sizes.
    cache: Arc<Mutex<BoundedByteCache<Vec<u8>, bool>>>,
}

impl fmt::Debug for GlobMatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobMatcher")
            .field("pattern_lower", &self.pattern_lower)
            .field("cache_size", &self.cache.lock_or_panic().len())
            .finish()
    }
}

impl GlobMatcher {
    /// Creates a new GlobMatcher with the given pattern.
    pub fn new(pattern: &str) -> Self {
        let pattern_lower = pattern.to_lowercase();
        let pattern_is_ascii = pattern_lower.is_ascii();
        let pattern_has_wildcards = pattern_lower.contains('*') || pattern_lower.contains('?');
        // Any non-empty run of only `*` matches everything (e.g. `*`, `**`, `***`).
        let pattern_is_star = !pattern_lower.is_empty() && pattern_lower.bytes().all(|b| b == b'*');
        GlobMatcher {
            pattern_lower,
            pattern_is_ascii,
            pattern_has_wildcards,
            pattern_is_star,
            cache: Arc::new(Mutex::new(BoundedByteCache::new(
                DEFAULT_MAX_ENTRIES,
                DEFAULT_MAX_BYTES,
            ))),
        }
    }

    /// Returns the pattern (lowercase version)
    pub fn pattern(&self) -> &str {
        &self.pattern_lower
    }

    /// Checks if the given subject matches the glob pattern.
    /// The match is case insensitive.
    pub fn matches(&self, subject: &str) -> bool {
        // "*" matches everything (covers the most common rule pattern).
        if self.pattern_is_star {
            return true;
        }

        // ASCII fast path: no allocation, no lock.
        if self.pattern_is_ascii && subject.is_ascii() {
            return self.matches_ascii(subject.as_bytes());
        }

        // Unicode fallback: allocate a lowercased subject and consult the cache.
        self.matches_unicode(subject)
    }

    /// ASCII fast path. Operates directly on the subject bytes using ASCII case folding.
    ///
    /// Pre-conditions: `self.pattern_lower` is ASCII (verified at construction) and `subject` is
    /// ASCII (verified by the caller).
    fn matches_ascii(&self, subject: &[u8]) -> bool {
        let pattern = self.pattern_lower.as_bytes();

        // Exact match (no wildcards): a plain case-insensitive byte compare beats a hash
        // lookup, so skip the cache entirely.
        if !self.pattern_has_wildcards {
            return pattern.eq_ignore_ascii_case(subject);
        }

        // Wildcard ASCII: cache results keyed on the raw subject bytes. Repeated calls with
        // the same input (the common case in sampling rules) become an O(1) lookup, avoiding
        // potentially O(P·S) backtracking. Different case spellings get separate entries,
        // which is acceptable: real callers don't mix cases for the same logical value, and
        // lowercasing for canonicalization would allocate on every call.
        //
        // The lock is released around `glob_match_bytes` so a slow (worst-case backtracking)
        // match on one thread doesn't block cache hits on other threads sharing this matcher.
        // A concurrent inserter racing us on the same key is harmless: `put` overwrites with
        // the same boolean result.
        if let Some(&result) = self.cache.lock_or_panic().get(subject) {
            return result;
        }
        let result = glob_match_bytes::<true>(pattern, subject);
        self.cache.lock_or_panic().put(subject.to_vec(), result);
        result
    }

    /// Unicode fallback. Lowercases the subject with `str::to_lowercase` (one right-sized
    /// allocation, SIMD-vectorized for the ASCII prefix) and runs the matcher on the resulting
    /// bytes. Results for wildcard patterns are cached in the shared LRU.
    fn matches_unicode(&self, subject: &str) -> bool {
        let subject_lower = subject.to_lowercase();

        // Exact match.
        if self.pattern_lower == subject_lower {
            return true;
        }
        // Pattern has no wildcards and isn't an exact match: definite miss.
        if !self.pattern_has_wildcards {
            return false;
        }

        let subject_lower_bytes = subject_lower.into_bytes();

        // Release the lock around the match so a slow backtracking computation doesn't block
        // cache hits on other threads. Concurrent inserts on the same key are harmless.
        if let Some(&result) = self.cache.lock_or_panic().get(&subject_lower_bytes) {
            return result;
        }
        // Unicode path: subject already lowercased — skip per-byte ASCII case-fold.
        let result = glob_match_bytes::<false>(self.pattern_lower.as_bytes(), &subject_lower_bytes);
        self.cache.lock_or_panic().put(subject_lower_bytes, result);
        result
    }
}

impl Clone for GlobMatcher {
    fn clone(&self) -> Self {
        // Share the cache across clones so previously cached results are reused and we don't
        // allocate a fresh empty `LruCache` on every clone.
        GlobMatcher {
            pattern_lower: self.pattern_lower.clone(),
            pattern_is_ascii: self.pattern_is_ascii,
            pattern_has_wildcards: self.pattern_has_wildcards,
            pattern_is_star: self.pattern_is_star,
            cache: Arc::clone(&self.cache),
        }
    }
}

/// Backtracking glob match on byte slices. `pattern` is always pre-lowercased.
///
/// `ASCII_FOLD` selects how literal bytes are compared:
/// - `true`: caller passes raw mixed-case subject bytes (ASCII path); fold per byte via
///   `eq_ignore_ascii_case`.
/// - `false`: caller has already lowercased the subject (Unicode fallback); plain byte equality is
///   sufficient and avoids redundant ASCII folding inside the hot loop.
///
/// The const-generic is monomorphized at compile time, so the branch is eliminated.
fn glob_match_bytes<const ASCII_FOLD: bool>(pattern: &[u8], subject: &[u8]) -> bool {
    let mut px = 0; // Pattern index.
    let mut sx = 0; // Subject index.
    let mut next_px = 0; // Next backtracking pattern index.
    let mut next_sx = 0; // Next backtracking subject index.

    while px < pattern.len() || sx < subject.len() {
        if px < pattern.len() {
            let p = pattern[px];

            if p == b'?' {
                // Single character wildcard.
                if sx < subject.len() {
                    px += 1;
                    sx += 1;
                    continue;
                }
            } else if p == b'*' {
                // Zero-or-more characters wildcard.
                next_px = px;
                next_sx = sx + 1;
                px += 1;
                continue;
            } else if sx < subject.len() && {
                // Pattern is always pre-lowercased, so only the subject side needs folding.
                // `eq_ignore_ascii_case` folds *both* operands, doubling the work in the hot
                // loop — measurably worse on backtracking-heavy inputs.
                let s = subject[sx];
                let folded = if ASCII_FOLD {
                    s.to_ascii_lowercase()
                } else {
                    s
                };
                folded == p
            } {
                px += 1;
                sx += 1;
                continue;
            }
        }

        // Backtrack to the last `*` if we have any subject left to consume.
        if 0 < next_sx && next_sx <= subject.len() {
            px = next_px;
            sx = next_sx;
            continue;
        }

        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_exact_match() {
        let matcher = GlobMatcher::new("hello");
        assert!(matcher.matches("hello"));
        assert!(matcher.matches("HELLO")); // Case insensitive
        assert!(!matcher.matches("hello world"));
        assert!(!matcher.matches("hell"));
    }

    #[test]
    fn test_glob_question_mark() {
        let matcher = GlobMatcher::new("h?llo");
        assert!(matcher.matches("hello"));
        assert!(matcher.matches("hallo"));
        assert!(!matcher.matches("hlo"));
        assert!(!matcher.matches("heello"));
    }

    #[test]
    fn test_glob_asterisk() {
        let matcher = GlobMatcher::new("h*o");
        assert!(matcher.matches("hello"));
        assert!(matcher.matches("ho"));
        assert!(matcher.matches("hello world o"));
        assert!(!matcher.matches("hell"));

        let matcher = GlobMatcher::new("h*");
        assert!(matcher.matches("hello"));
        assert!(matcher.matches("h"));
        assert!(!matcher.matches("world"));
    }

    #[test]
    fn test_glob_complex() {
        let matcher = GlobMatcher::new("c*t?r*");
        assert!(matcher.matches("contoroller"));
        assert!(matcher.matches("cater"));
        assert!(matcher.matches("ctfr!"));
        assert!(!matcher.matches("car"));

        let matcher = GlobMatcher::new("*service*");
        assert!(matcher.matches("myservice"));
        assert!(matcher.matches("service"));
        assert!(matcher.matches("my service name"));
        assert!(!matcher.matches("svc"));
    }

    #[test]
    fn test_debug_impl() {
        let matcher = GlobMatcher::new("svc-*");
        let dbg = format!("{matcher:?}");
        assert!(dbg.contains("svc-*"));
    }

    #[test]
    fn test_double_star_matches_everything() {
        let matcher = GlobMatcher::new("**");
        assert!(matcher.matches("anything"));
        assert!(matcher.matches(""));
    }

    #[test]
    fn test_all_star_patterns_short_circuit() {
        // Any pattern that is only `*` characters takes the `pattern_is_star` short-circuit.
        for pattern in ["*", "**", "***", "****"] {
            let matcher = GlobMatcher::new(pattern);
            assert!(
                matcher.pattern_is_star,
                "pattern {:?} should set pattern_is_star",
                pattern
            );
            assert!(matcher.matches(""));
            assert!(matcher.matches("anything"));
            assert!(matcher.matches("caf\u{00e9}"));
        }

        // Patterns containing non-`*` characters do not short-circuit.
        for pattern in ["", "a*", "*a*", "?"] {
            let matcher = GlobMatcher::new(pattern);
            assert!(
                !matcher.pattern_is_star,
                "pattern {:?} should not set pattern_is_star",
                pattern
            );
        }
    }

    #[test]
    fn test_unicode_exact_match_no_wildcard() {
        // No-wildcard unicode pattern: exercises the `pattern_lower == subject_lower` early
        // return in the unicode fallback path.
        let matcher = GlobMatcher::new("caf\u{00e9}");
        assert!(!matcher.pattern_has_wildcards);
        assert!(matcher.matches("caf\u{00e9}")); // exact lowercase
        assert!(matcher.matches("CAF\u{00c9}")); // uppercase folds to same
        assert!(!matcher.matches("cafe")); // ASCII miss
        assert!(!matcher.matches("caf\u{00e9}s")); // longer miss
    }

    #[test]
    fn test_unicode_subject_against_ascii_pattern() {
        // Pattern is ASCII but subject is not -> takes the unicode fallback path.
        let matcher = GlobMatcher::new("caf*");
        assert!(matcher.matches("caf\u{00e9}")); // "café"
        assert!(!matcher.matches("\u{00e9}cole")); // "école"
    }

    #[test]
    fn test_unicode_pattern() {
        // Non-ASCII pattern -> always unicode path.
        let matcher = GlobMatcher::new("caf\u{00e9}*");
        assert!(matcher.matches("caf\u{00e9}-shop"));
        assert!(matcher.matches("CAF\u{00c9}-SHOP")); // Uppercase Unicode
        assert!(!matcher.matches("cafe-shop"));
    }

    #[test]
    fn test_unicode_repeated_calls() {
        // Smoke test: repeated unicode calls should all succeed (hits + misses interleaved).
        let matcher = GlobMatcher::new("caf\u{00e9}*");
        for _ in 0..10 {
            assert!(matcher.matches("caf\u{00e9}-controller"));
            assert!(!matcher.matches("x\u{00e9}"));
        }
    }

    #[test]
    fn test_clone_independent() {
        let matcher = GlobMatcher::new("caf*");
        let clone = matcher.clone();
        assert!(matcher.matches("caf\u{00e9}"));
        assert!(clone.matches("caf\u{00e9}"));
    }

    #[test]
    fn test_ascii_no_wildcard_skips_cache() {
        let matcher = GlobMatcher::new("svc-web");
        assert!(matcher.matches("svc-web"));
        assert!(!matcher.matches("svc-db"));
        let cache = matcher.cache.lock_or_panic();
        assert_eq!(
            cache.len(),
            0,
            "non-wildcard ASCII path should not touch cache"
        );
    }

    #[test]
    fn test_ascii_wildcard_populates_cache() {
        let matcher = GlobMatcher::new("svc-*");
        assert!(matcher.matches("svc-web"));
        assert!(matcher.matches("svc-db"));
        let cache = matcher.cache.lock_or_panic();
        assert_eq!(
            cache.len(),
            2,
            "ASCII wildcard path should cache each unique subject"
        );
    }

    #[test]
    fn test_unicode_path_populates_cache() {
        let matcher = GlobMatcher::new("caf*");
        assert!(matcher.matches("caf\u{00e9}"));
        let cache = matcher.cache.lock_or_panic();
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_clone_shares_cache() {
        let matcher = GlobMatcher::new("caf*");
        let clone = matcher.clone();
        assert!(matcher.matches("caf\u{00e9}"));
        // Clone sees the same cache entry (Arc-shared).
        let cache = clone.cache.lock_or_panic();
        assert_eq!(cache.len(), 1);
    }
}
