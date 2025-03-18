// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::encodable::{try_encode_with_tag, LenEncodable, Location, Mapping};
use super::proto::Function;
use hashbrown::HashTable;
use rustc_hash::FxHasher;
use std::hash::Hasher;
use std::ops::Range;

// This type exists because Range<u32> is not Copy.
#[derive(Clone, Copy, Debug)]
struct ByteRange {
    pub start: u32,
    pub end: u32,
}

pub struct ProfileProtoMap {
    ht: HashTable<(ByteRange, u64)>,
    buf: Vec<u8>,
}

pub trait Identifiable: LenEncodable {
    fn id(&self) -> u64;

    /// Return true if this should dedup to id=0. This is likely a performance
    /// sensitive operation, make sure you branch on the most helpful field(s)
    /// first.
    fn is_zero_id(&self) -> bool;
}

impl ProfileProtoMap {
    #[inline]
    fn project(buf: &Vec<u8>, byte_range: ByteRange) -> &[u8] {
        let range = Range {
            start: byte_range.start as usize,
            end: byte_range.end as usize,
        };
        // SAFETY: the ByteRange's are not exposed, and we constructed them
        // in-range, and we never modify the existing bytes (only append).
        unsafe { &buf.as_slice().get_unchecked(range) }
    }

    #[inline]
    fn hash(bytes: &[u8]) -> u64 {
        let mut hasher = FxHasher::default();
        hasher.write(bytes);
        hasher.finish()
    }

    fn insert_byte_range(&mut self, range: ByteRange, id: u64) -> (u64, bool) {
        // We need an immutable reference to the buffer, and a mutable
        // reference to the hash table, and that goes against borrow rules if
        // we directly refer to self. So instead of using self references for
        // both, steal the buffer and now that's a separate borrow.
        // Put it back later, of course.
        let mut buf = Vec::new();
        core::mem::swap(&mut self.buf, &mut buf);

        // These are the bytes of the item being added. We'll compare against
        // this byte slice possibly many times.
        let bytes = Self::project(&buf, range);
        // The hash of the bytes of the item being added.
        let hash = Self::hash(bytes);

        // Find the hash table entry. If it doesn't exist, there's an
        // AbsentEntry API to insert it.
        let item = self.ht.find(hash, |(range, _id)| {
            let bytes2 = Self::project(&buf, *range);
            bytes.eq(bytes2)
        });

        let already_existed = item.is_some();
        let deduped_id = if let Some((_range, existing_id)) = item {
            *existing_id
        } else {
            _ = self.ht.insert_unique(hash, (range, id), |(range, _id)| {
                let bytes = Self::project(&buf, *range);
                Self::hash(bytes)
            });
            id
        };

        // Restore buffer before returning.
        core::mem::swap(&mut self.buf, &mut buf);
        (deduped_id, already_existed)
    }

    #[cfg_attr(debug_assertions, track_caller)]
    fn insert_with_tag(&mut self, tag: u32, encodable: &impl Identifiable) -> (u64, bool) {
        if encodable.is_zero_id() {
            return (0, true);
        }

        // Items get pushed to the end, then deduplicated. If they already
        // existed, then we can shrink back to this size. This means the
        // buffer cannot have any other writes!
        // todo: can we rewrite the code so this is structurally true?
        let checkpoint = self.buf.len();
        let id = encodable.id();

        // PANIC: the global allocator doesn't panic, and the other condition
        // is a value over 2 GiB
        let range = match try_encode_with_tag(encodable, tag, &mut self.buf) {
            Ok(range) => range,
            Err(err) => {
                self.buf.truncate(checkpoint);
                panic!("failed inserting message with tag {tag}: {err}")
            }
        };
        debug_assert!(range.end >= range.start);

        let byte_range = ByteRange {
            start: range.start,
            end: range.end,
        };

        let (id, already_existed) = self.insert_byte_range(byte_range, id);
        if already_existed {
            // The len should be shrunk back down to the checkpoint, but the
            // capacity should remain; this matches `truncate` exactly.
            self.buf.truncate(checkpoint);
        }
        (id, already_existed)
    }
}

impl ProfileProtoMap {
    pub const fn new() -> Self {
        Self {
            ht: HashTable::new(),
            buf: Vec::new(),
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.ht.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.ht.is_empty()
    }

    #[inline]
    pub fn clear(&mut self) -> Vec<u8> {
        self.ht.clear();
        core::mem::take(&mut self.buf)
    }
}

impl Default for ProfileProtoMap {
    fn default() -> Self {
        Self::new()
    }
}

pub trait Insert<T: Identifiable> {
    fn insert(&mut self, value: &T) -> (u64, bool);
}

impl Insert<Mapping> for ProfileProtoMap {
    fn insert(&mut self, value: &Mapping) -> (u64, bool) {
        self.insert_with_tag(3u32, value)
    }
}

impl Insert<Location> for ProfileProtoMap {
    fn insert(&mut self, value: &Location) -> (u64, bool) {
        self.insert_with_tag(4u32, value)
    }
}

impl Insert<Function> for ProfileProtoMap {
    fn insert(&mut self, value: &Function) -> (u64, bool) {
        self.insert_with_tag(5u32, value)
    }
}

impl Identifiable for Mapping {
    fn id(&self) -> u64 {
        self.id
    }

    #[inline]
    fn is_zero_id(&self) -> bool {
        // In PHP, Python, and Ruby, these are all zero (they don't really use
        // mappings except as required by APIs).
        // .NET currently only sets filename.
        // The native profiler uses all the fields.
        // This implementation does a mix of branching and branch-free to try
        // and have middle-ground performance for all languages.
        let filename = self.filename;
        let build_id = self.build_id;
        let c = filename | build_id;
        if c != 0 {
            return false;
        }

        let memory_start = self.memory_start;
        let memory_limit = self.memory_limit;
        let file_offset = self.file_offset;
        0 == (memory_start | memory_limit | file_offset)
    }
}

impl Identifiable for Location {
    fn id(&self) -> u64 {
        self.id
    }

    fn is_zero_id(&self) -> bool {
        // I don't expect any profilers to set a zero value here, so pay
        // for the branch; I expect this to basically always return false.
        if self.line.function_id != 0 {
            return false;
        }
        if self.line.line != 0 {
            return false;
        }

        // These members are not used by some of the profilers, so optimize
        // for them all being zero by doing bitwise operations.
        // If any bit is set in any of them, then it's not zero, so bitwise-or
        // them all together.
        let mapping_id = self.mapping_id;
        let address = self.address;
        let c = mapping_id | address;
        c == 0
    }
}

impl Identifiable for Function {
    fn id(&self) -> u64 {
        self.id
    }

    #[inline]
    fn is_zero_id(&self) -> bool {
        // I expect everyone to set a non-zero function name, as pprof tools
        // seem to expect a name here and don't handle the missing case well
        // (at least when I tried). This branch should be predictable.
        if self.name != 0 {
            return false;
        }

        // But I don't see the point in branching for any of these, if the
        // function name was zero, I think the rest are probably zero as well.
        let system_name = self.system_name;
        let filename = self.filename;
        let start_line = self.start_line;
        0 == (system_name | filename | start_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pprof::Line;

    #[test]
    fn test_insert() {
        let mut map = ProfileProtoMap::new();

        // Adding the same items multiple times should result in the same ids.
        let mut already_existed = false;
        loop {
            // Empty mapping, when excluding the ID.
            let (id0, b0) = map.insert(&Mapping {
                id: 1,
                memory_start: 0,
                memory_limit: 0,
                file_offset: 0,
                filename: 0,
                build_id: 0,
            });
            assert_eq!(0, id0);
            assert!(b0);

            // Empty Function, when excluding the ID.
            let (id0, b0) = map.insert(&Function {
                id: 2,
                name: 0,
                system_name: 0,
                filename: 0,
                start_line: 0,
            });
            assert_eq!(0, id0);
            assert!(b0);

            // Empty Location, when excluding the ID.
            let (id0, b0) = map.insert(&Location {
                id: 1,
                mapping_id: 0,
                address: 0,
                line: Default::default(),
            });
            assert_eq!(0, id0);
            assert!(b0);

            // Empty Location, when excluding the ID.
            let (id0, b0) = map.insert(&Location {
                id: 1,
                mapping_id: 0,
                address: 0,
                line: Line {
                    function_id: 0,
                    line: 0,
                },
            });
            assert_eq!(0, id0);
            assert!(b0);

            let (id1, b1) = map.insert(&Function {
                id: 1,
                name: 1,
                system_name: 0,
                filename: 0,
                start_line: 0,
            });
            assert_eq!(1, id1);
            assert_eq!(already_existed, b1);

            let (id1, b1) = map.insert(&Function {
                id: 2, // Same as 1 except for id, so it'll dedup.
                name: 1,
                system_name: 0,
                filename: 0,
                start_line: 0,
            });
            assert_eq!(1, id1);
            assert!(b1);

            let (id2, b2) = map.insert(&Mapping {
                id: 2,
                memory_start: 0,
                memory_limit: 0,
                file_offset: 0,
                filename: 2,
                build_id: 0,
            });
            assert_eq!(2, id2);
            assert_eq!(already_existed, b2);

            let (id3, b3) = map.insert(&Location {
                id: 3,
                mapping_id: 0,
                address: 0,
                line: Line {
                    function_id: 1,
                    line: 1,
                },
            });
            assert_eq!(3, id3);
            assert_eq!(already_existed, b3);

            let (id4, b4) = map.insert(&Function {
                id: 4,
                name: 4,
                system_name: 0,
                filename: 2,
                start_line: 1,
            });
            assert_eq!(4, id4);
            assert_eq!(already_existed, b4);

            let (id5, b5) = map.insert(&Location {
                id: 5,
                mapping_id: 0,
                address: 0,
                line: Line {
                    function_id: 4,
                    line: 2,
                },
            });
            assert_eq!(5, id5);
            assert_eq!(already_existed, b5);

            if !already_existed {
                already_existed = true;
            } else {
                break;
            }
        }
    }

    #[test]
    fn test_serialization() {
        let mut map = ProfileProtoMap::new();
        // Values were pulled from a PHP profile of the symfony/demo app,
        // though mappings were dropped (all Mapping.id=1 which is an empty
        // mapping, which we should avoid and use location.mapping_id=0).
        let functions = [
            Function {
                id: 1,
                name: 21,
                ..Function::default()
            },
            Function {
                id: 2,
                name: 25,
                ..Function::default()
            },
            Function {
                id: 3,
                name: 26,
                ..Function::default()
            },
            Function {
                id: 4,
                name: 27,
                filename: 24,
                ..Function::default()
            },
            Function {
                id: 5,
                name: 48,
                filename: 39,
                ..Function::default()
            },
            Function {
                id: 6,
                name: 49,
                filename: 30,
                ..Function::default()
            },
            Function {
                id: 7,
                name: 27,
                filename: 29,
                ..Function::default()
            },
            Function {
                id: 8,
                name: 27,
                filename: 28,
                ..Function::default()
            },
            Function {
                id: 9,
                name: 63,
                ..Function::default()
            },
            Function {
                id: 10,
                name: 27,
                filename: 62,
                ..Function::default()
            },
        ];
        let locations = [
            Location {
                id: 1,
                line: Line {
                    function_id: 1,
                    line: 0,
                },
                ..Location::default()
            },
            Location {
                id: 2,
                line: Line {
                    function_id: 2,
                    line: 0,
                },
                ..Location::default()
            },
            Location {
                id: 3,
                line: Line {
                    function_id: 3,
                    line: 0,
                },
                ..Location::default()
            },
            Location {
                id: 4,
                line: Line {
                    function_id: 4,
                    line: 5,
                },
                ..Location::default()
            },
            Location {
                id: 5,
                line: Line {
                    function_id: 5,
                    line: 41,
                },
                ..Location::default()
            },
            Location {
                id: 6,
                line: Line {
                    function_id: 6,
                    line: 45,
                },
                ..Location::default()
            },
            Location {
                id: 7,
                line: Line {
                    function_id: 7,
                    line: 25,
                },
                ..Location::default()
            },
            Location {
                id: 8,
                line: Line {
                    function_id: 8,
                    line: 5,
                },
                ..Location::default()
            },
            Location {
                id: 9,
                line: Line {
                    function_id: 9,
                    line: 0,
                },
                ..Location::default()
            },
            Location {
                id: 10,
                line: Line {
                    function_id: 10,
                    line: 67,
                },
                ..Location::default()
            },
        ];

        // We don't _need_ to insert these in this order, but it practices a
        // capability that we have with this structure that we didn't before.
        for (function, location) in functions.iter().zip(locations.iter()) {
            _ = map.insert(function);
            _ = map.insert(location);
        }

        let buffer = map.clear();

        use prost::Message;
        let profile = crate::pprof::Profile::decode(buffer.as_slice()).unwrap();
        assert_eq!(profile.locations.len(), locations.len());
        assert_eq!(profile.functions.len(), functions.len());

        for (pprof, encodable) in profile.functions.iter().zip(&functions) {
            assert_eq!(pprof.id, encodable.id);
            assert_eq!(pprof.name, encodable.name);
            assert_eq!(pprof.system_name, encodable.system_name);
            assert_eq!(pprof.filename, encodable.filename);
            assert_eq!(pprof.start_line, encodable.start_line);
        }

        for (pprof, encodable) in profile.locations.iter().zip(&locations) {
            assert_eq!(pprof.id, encodable.id);
            assert_eq!(pprof.mapping_id, encodable.mapping_id);
            assert_eq!(pprof.address, encodable.address);

            assert_eq!(pprof.lines.len(), 1);
            assert_eq!(pprof.lines[0], encodable.line);
        }
    }
}
