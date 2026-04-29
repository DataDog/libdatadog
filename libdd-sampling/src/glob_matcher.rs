// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use lru::LruCache;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Mutex;

/// A backtracking implementation of the glob matching algorithm.
///
/// The glob pattern language supports `*` as a multiple character wildcard (including empty string)
/// and `?` as a single character wildcard (not including empty string). The match is case
/// insensitive.
///
/// This implementation includes an LRU cache for faster repeated matching.
pub struct GlobMatcher {
    /// Lowercased pattern for case-insensitive matching
    pattern_lower: String,
    /// LRU cache of previously matched strings to their results
    cache: Mutex<LruCache<String, bool>>,
}

impl fmt::Debug for GlobMatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobMatcher")
            .field("pattern_lower", &self.pattern_lower)
            .field("cache_size", &self.cache.lock().unwrap().len())
            .finish()
    }
}

impl GlobMatcher {
    /// Creates a new GlobMatcher with the given pattern
    pub fn new(pattern: &str) -> Self {
        // Use a cache of size 256
        let cache_size = NonZeroUsize::new(256).unwrap();
        GlobMatcher {
            pattern_lower: pattern.to_lowercase(),
            cache: Mutex::new(LruCache::new(cache_size)),
        }
    }

    /// Returns the pattern (lowercase version)
    pub fn pattern(&self) -> String {
        self.pattern_lower.clone()
    }

    /// Checks if the given subject matches the glob pattern
    /// The match is case insensitive.
    pub fn matches(&self, subject: &str) -> bool {
        let subject_lower = subject.to_lowercase();

        // short circuit for common cases
        // "*" matches everything
        if self.pattern_lower == "*" {
            return true;
        }
        // exact match
        if self.pattern_lower == subject_lower {
            return true;
        }
        // if not exact, and no wildcards, return false
        if !self.pattern_lower.contains('*') && !self.pattern_lower.contains('?') {
            return false;
        }

        // Try to get from cache first
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(&result) = cache.get(&subject_lower) {
                return result;
            }
        }

        // Backtracking algorithm
        let pattern = self.pattern_lower.as_bytes();
        let subject = subject_lower.as_bytes();

        let mut px = 0; // Pattern index
        let mut sx = 0; // Subject index
        let mut next_px = 0; // Next backtracking pattern index
        let mut next_sx = 0; // Next backtracking subject index

        while px < pattern.len() || sx < subject.len() {
            if px < pattern.len() {
                let char = pattern[px];

                if char == b'?' {
                    // Single character wildcard
                    if sx < subject.len() {
                        px += 1;
                        sx += 1;
                        continue;
                    }
                } else if char == b'*' {
                    // Zero-or-more characters wildcard
                    next_px = px;
                    next_sx = sx + 1;
                    px += 1;
                    continue;
                } else if sx < subject.len() && subject[sx] == char {
                    // Normal character match
                    px += 1;
                    sx += 1;
                    continue;
                }
            }

            // If we can backtrack (we've seen a * and have more characters in subject)
            if 0 < next_sx && next_sx <= subject.len() {
                px = next_px;
                sx = next_sx;
                continue;
            }

            // If we're here, we've exhausted all options and no match was found
            // Store in cache and return
            {
                let mut cache = self.cache.lock().unwrap();
                cache.put(subject_lower, false);
            }
            return false;
        }

        // If we reached here, we've consumed both strings entirely - it's a match
        // Store in cache and return
        {
            let mut cache = self.cache.lock().unwrap();
            cache.put(subject_lower, true);
        }
        true
    }
}

impl Clone for GlobMatcher {
    fn clone(&self) -> Self {
        // Create a new matcher with the same pattern
        // This doesn't clone the cache since each instance maintains its own cache
        GlobMatcher {
            pattern_lower: self.pattern_lower.clone(),
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
        }
    }
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
    fn test_glob_caching() {
        let matcher = GlobMatcher::new("c*t?r*");

        // First match should populate cache
        assert!(matcher.matches("contoroller"));

        // Check the cache
        let cache = matcher.cache.lock().unwrap();
        assert!(cache.contains(&"contoroller".to_string()));
        drop(cache);

        // Add another entry to cache
        assert!(!matcher.matches("car"));

        // Verify both are in cache
        let cache = matcher.cache.lock().unwrap();
        assert!(cache.contains(&"contoroller".to_string()));
        assert!(cache.contains(&"car".to_string()));
    }
}
