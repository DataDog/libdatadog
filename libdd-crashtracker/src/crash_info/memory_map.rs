// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::cmp::Ordering;

/// A single entry from /proc/self/maps.
///
/// Each line has the format:
/// ```text
/// start-end perms offset dev inode [pathname]
/// 55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryMapping {
    pub start: u64,
    pub end: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub private: bool,
    pub offset: u64,
    pub pathname: Option<String>,
}

impl MemoryMapping {
    /// Parse a single line from /proc/self/maps.
    pub fn from_maps_line(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();

        // Parse address range: "start-end"
        let addr_range = parts.next()?;
        let (start_str, end_str) = addr_range.split_once('-')?;
        let start = u64::from_str_radix(start_str, 16).ok()?;
        let end = u64::from_str_radix(end_str, 16).ok()?;

        // Parse permissions: "rwxp" or "rwxs"
        let perms = parts.next()?;
        if perms.len() < 4 {
            return None;
        }
        let perms_bytes = perms.as_bytes();
        let readable = perms_bytes[0] == b'r';
        let writable = perms_bytes[1] == b'w';
        let executable = perms_bytes[2] == b'x';
        let private = perms_bytes[3] == b'p';

        // Parse offset
        let offset_str = parts.next()?;
        let offset = u64::from_str_radix(offset_str, 16).ok()?;

        // Skip dev and inode
        let _dev = parts.next()?;
        let _inode = parts.next()?;

        // Pathname is everything remaining (may be absent)
        let pathname = parts.next().map(|s| s.to_string());

        Some(Self {
            start,
            end,
            readable,
            writable,
            executable,
            private,
            offset,
            pathname,
        })
    }

    /// Returns the permissions as a 4-char string like "r-xp".
    pub fn permissions_string(&self) -> String {
        format!(
            "{}{}{}{}",
            if self.readable { 'r' } else { '-' },
            if self.writable { 'w' } else { '-' },
            if self.executable { 'x' } else { '-' },
            if self.private { 'p' } else { 's' },
        )
    }
}

/// A sorted collection of memory mappings supporting efficient address lookup.
#[derive(Debug, Clone)]
pub struct MemoryMap {
    entries: Vec<MemoryMapping>,
}

impl MemoryMap {
    /// Parse all lines from /proc/self/maps into a sorted memory map.
    pub fn from_maps_lines(lines: &[String]) -> Self {
        let mut entries: Vec<MemoryMapping> = lines
            .iter()
            .filter_map(|line| MemoryMapping::from_maps_line(line))
            .collect();
        // Entries should already be sorted, but ensure it
        entries.sort_by_key(|e| e.start);
        Self { entries }
    }

    /// Find the mapping containing the given address using binary search.
    pub fn find_mapping(&self, addr: u64) -> Option<&MemoryMapping> {
        self.entries
            .binary_search_by(|m| {
                if addr < m.start {
                    Ordering::Greater
                } else if addr >= m.end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Find the `[stack]` mapping.
    pub fn find_stack(&self) -> Option<&MemoryMapping> {
        self.entries
            .iter()
            .find(|m| m.pathname.as_deref() == Some("[stack]"))
    }

    /// Find the `[heap]` mapping.
    pub fn find_heap(&self) -> Option<&MemoryMapping> {
        self.entries
            .iter()
            .find(|m| m.pathname.as_deref() == Some("[heap]"))
    }

    /// Check if an address is within `distance` bytes below the start of any
    /// mapping (i.e. in a guard region).
    pub fn is_near_mapping_start(&self, addr: u64, distance: u64) -> bool {
        self.entries
            .iter()
            .any(|m| addr < m.start && m.start.saturating_sub(addr) <= distance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_maps_lines() -> Vec<String> {
        vec![
            "55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp".to_string(),
            "55a3f2e00000-55a3f2f00000 rw-p 00200000 fd:01 1234567  /usr/bin/myapp".to_string(),
            "7f8a10000000-7f8a10200000 rw-p 00000000 00:00 0".to_string(),
            "7f8a12000000-7f8a12200000 r--p 00000000 fd:01 2345678  /usr/lib/libc.so.6".to_string(),
            "7f8a12200000-7f8a12400000 r-xp 00200000 fd:01 2345678  /usr/lib/libc.so.6".to_string(),
            "7ffc89a00000-7ffc89c00000 rw-p 00000000 00:00 0        [stack]".to_string(),
            "7ffc89d00000-7ffc89d02000 r-xp 00000000 00:00 0        [vdso]".to_string(),
            "ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0  [vsyscall]".to_string(),
        ]
    }

    #[test]
    fn test_parse_maps_line_with_pathname() {
        let line = "55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp";
        let m = MemoryMapping::from_maps_line(line).unwrap();
        assert_eq!(m.start, 0x55a3f2a00000);
        assert_eq!(m.end, 0x55a3f2c00000);
        assert!(m.readable);
        assert!(!m.writable);
        assert!(m.executable);
        assert!(m.private);
        assert_eq!(m.offset, 0);
        assert_eq!(m.pathname.as_deref(), Some("/usr/bin/myapp"));
    }

    #[test]
    fn test_parse_maps_line_no_pathname() {
        let line = "7f8a10000000-7f8a10200000 rw-p 00000000 00:00 0";
        let m = MemoryMapping::from_maps_line(line).unwrap();
        assert_eq!(m.start, 0x7f8a10000000);
        assert!(m.readable);
        assert!(m.writable);
        assert!(!m.executable);
        assert!(m.private);
        assert!(m.pathname.is_none());
    }

    #[test]
    fn test_parse_maps_line_stack() {
        let line = "7ffc89a00000-7ffc89c00000 rw-p 00000000 00:00 0        [stack]";
        let m = MemoryMapping::from_maps_line(line).unwrap();
        assert_eq!(m.pathname.as_deref(), Some("[stack]"));
    }

    #[test]
    fn test_permissions_string() {
        let m = MemoryMapping {
            start: 0,
            end: 0x1000,
            readable: true,
            writable: false,
            executable: true,
            private: true,
            offset: 0,
            pathname: None,
        };
        assert_eq!(m.permissions_string(), "r-xp");
    }

    #[test]
    fn test_find_mapping_hit() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        let m = map.find_mapping(0x55a3f2b00000).unwrap();
        assert_eq!(m.pathname.as_deref(), Some("/usr/bin/myapp"));
        assert!(m.executable);
    }

    #[test]
    fn test_find_mapping_miss() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        assert!(map.find_mapping(0x1000).is_none()); // way below any mapping
    }

    #[test]
    fn test_find_mapping_at_start_boundary() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        // Exactly at start should be found
        assert!(map.find_mapping(0x55a3f2a00000).is_some());
    }

    #[test]
    fn test_find_mapping_at_end_boundary() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        // Exactly at end should NOT be found (end is exclusive)
        assert!(map.find_mapping(0x55a3f2c00000).is_none());
    }

    #[test]
    fn test_find_stack() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        let stack = map.find_stack().unwrap();
        assert_eq!(stack.pathname.as_deref(), Some("[stack]"));
        assert_eq!(stack.start, 0x7ffc89a00000);
    }

    #[test]
    fn test_find_heap_missing() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        assert!(map.find_heap().is_none()); // sample data has no [heap]
    }

    #[test]
    fn test_find_heap_present() {
        let mut lines = sample_maps_lines();
        lines.push("01000000-02000000 rw-p 00000000 00:00 0        [heap]".to_string());
        let map = MemoryMap::from_maps_lines(&lines);
        assert!(map.find_heap().is_some());
    }

    #[test]
    fn test_empty_maps() {
        let map = MemoryMap::from_maps_lines(&[]);
        assert!(map.find_mapping(0x1000).is_none());
        assert!(map.find_stack().is_none());
    }

    #[test]
    fn test_malformed_line_skipped() {
        let lines = vec![
            "not a valid line".to_string(),
            "55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp".to_string(),
        ];
        let map = MemoryMap::from_maps_lines(&lines);
        // Only the valid line should be parsed
        assert!(map.find_mapping(0x55a3f2b00000).is_some());
    }

    #[test]
    fn test_is_near_mapping_start() {
        let map = MemoryMap::from_maps_lines(&sample_maps_lines());
        // stack starts at 0x7ffc89a00000; one page below is near
        assert!(map.is_near_mapping_start(0x7ffc899ff000, 0x1000));
        // far away is not near
        assert!(!map.is_near_mapping_start(0x1000, 0x1000));
    }
}
