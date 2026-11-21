// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use rust_alloc::ffi::CString;
use rust_alloc::format;
use rust_alloc::string::String;
use rust_alloc::vec::Vec;

extern crate std;

/// The process lock's directory: the OS temp directory on Unix, none on
/// Windows.
fn lock_dir() -> Option<CString> {
    if cfg!(unix) {
        let dir = std::env::temp_dir();
        Some(CString::new(dir.into_os_string().as_encoded_bytes()).unwrap())
    } else {
        None
    }
}

/// The strings every new string table holds after `""`.
fn reserved() -> impl Iterator<Item = &'static str> {
    (0..).map_while(<Str as Record>::reserved)
}

/// The id the first string a test interns gets.
fn first_free_id() -> u32 {
    1 + u32::try_from(reserved().count()).unwrap()
}

/// The arena bytes the reserved strings use.
fn reserved_arena_bytes() -> usize {
    reserved().map(|s| (2 + s.len()).next_multiple_of(2)).sum()
}

fn strings(regions: u8, arena_bytes: usize) -> StringTable {
    let config = Config {
        regions,
        arena_bytes,
    };
    StringTable::new(config, lock_dir().as_deref()).unwrap()
}

fn functions(regions: u8) -> FunctionTable {
    let config = Config {
        regions,
        arena_bytes: 0,
    };
    FunctionTable::new(config, lock_dir().as_deref()).unwrap()
}

/// Ids per 16 KiB block: 14 × (16,384 / bytes per group).
const STRINGS_PER_BLOCK: usize = 14 * (BLOCK_BYTES / 144);
const FUNCTIONS_PER_BLOCK: usize = 14 * (BLOCK_BYTES / 80);

#[test]
fn strings_intern_dedup_and_resolve() {
    let table = strings(2, 1 << 16);
    let first = first_free_id();
    assert_eq!(table.len(), usize_from(first));
    assert_eq!(table.capacity(), 2 * STRINGS_PER_BLOCK);
    assert_eq!(table.intern(""), Ok(StringId::ZERO));
    for (index, name) in (1..).zip(reserved()) {
        assert_eq!(table.intern(name), Ok(Id::new(index)));
    }
    let a = table.intern("service").unwrap();
    let b = table.intern("env").unwrap();
    assert_eq!((a.index(), b.index()), (first, first + 1));
    assert_eq!(table.intern("service"), Ok(a));
    assert_eq!(table.resolve(StringId::ZERO), Some(""));
    assert_eq!(table.resolve(a), Some("service"));
    assert_eq!(table.resolve(b), Some("env"));
    assert_eq!(table.resolve(Id::new(first + 2)), None);
    assert_eq!(table.len(), usize_from(first) + 2);
}

#[test]
fn strings_fill_every_region_then_report_full() {
    // Blocks 1 + 1 + 2; one region under Miri, which is slow.
    let regions = if cfg!(miri) { 1 } else { 3 };
    let table = strings(regions, 1 << 20);
    let capacity = table.capacity();
    let first = usize_from(first_free_id());
    let keys: Vec<String> = (first..capacity).map(|i| format!("s{i}")).collect();
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(
            table.intern(key).map(|id| usize_from(id.index())),
            Ok(first + i)
        );
    }
    assert_eq!(table.len(), capacity);
    let sealed = table
        .local()
        .header()
        .publication
        .sealed
        .load(Ordering::Relaxed);
    assert_eq!(usize_from(sealed), usize::from(regions));
    // Every region is sealed, so these are found without a lock.
    for (i, key) in keys.iter().enumerate() {
        let id = table.intern(key).unwrap();
        assert_eq!(usize_from(id.index()), first + i);
        assert_eq!(table.resolve(id), Some(key.as_str()));
    }
    assert_eq!(table.intern("one more"), Err(InternError::Full));
    assert_eq!(table.len(), capacity);
}

#[test]
fn arena_errors_leave_the_table_unchanged() {
    let table = strings(1, 16 + reserved_arena_bytes());
    // "" uses 2 bytes; "abcdefg" uses 2 + 7 rounded up to 10.
    let id = table.intern("abcdefg").unwrap();
    assert_eq!(table.intern("abcde"), Err(InternError::ArenaFull));
    assert_eq!(table.len(), usize::try_from(first_free_id()).unwrap() + 1);
    // A record that still fits: 2 + 1 rounded up = 4 bytes, ending at 16.
    let small = table.intern("x").unwrap();
    assert_eq!(table.resolve(id), Some("abcdefg"));
    assert_eq!(table.resolve(small), Some("x"));
    assert_eq!(table.intern("y"), Err(InternError::ArenaFull));

    // The longest record, 2 + 65,535 bytes rounded up, needs a larger arena.
    let table = strings(1, 1 << 17);
    let long = "x".repeat(usize::from(u16::MAX) + 1);
    assert_eq!(table.intern(&long), Err(InternError::TooLong));
    let longest = &long[1..];
    let id = table.intern(longest).unwrap();
    assert_eq!(table.resolve(id), Some(longest));
}

#[test]
fn functions_intern_inline_records() {
    let strings = strings(1, 1 << 12);
    let functions = functions(1);
    assert_eq!(functions.capacity(), FUNCTIONS_PER_BLOCK);
    assert_eq!(functions.intern(Function::default()), Ok(FunctionId::ZERO));
    let main = Function {
        name: strings.intern("main").unwrap(),
        system_name: strings.intern("_main").unwrap(),
        filename: strings.intern("main.c").unwrap(),
        start_line: 10,
    };
    let other = Function {
        start_line: 11,
        ..main
    };
    let id = functions.intern(main).unwrap();
    assert_eq!(id.index(), 1);
    assert_eq!(functions.intern(other).map(Id::index), Ok(2));
    assert_eq!(functions.intern(main), Ok(id));
    let resolved = functions.resolve(id).unwrap();
    assert_eq!(resolved, main);
    assert_eq!(strings.resolve(resolved.filename), Some("main.c"));

    // Fill the region: inline records need no arena.
    for line in 100.. {
        let function = Function {
            start_line: line,
            ..main
        };
        if let Err(error) = functions.intern(function) {
            assert_eq!(error, InternError::Full);
            break;
        }
    }
    assert_eq!(functions.len(), FUNCTIONS_PER_BLOCK);
    assert_eq!(functions.resolve(id), Some(main));
}

#[test]
fn config_errors() {
    let dir = lock_dir();
    let invalid = [
        Config {
            regions: 0,
            arena_bytes: 2,
        },
        Config {
            regions: 33,
            arena_bytes: 2,
        },
        Config {
            regions: 1,
            arena_bytes: 1,
        },
        Config {
            regions: 1,
            arena_bytes: MAX_ARENA_BYTES + 1,
        },
        // More ids than a u32 can name.
        Config {
            regions: 32,
            arena_bytes: 2,
        },
    ];
    for config in invalid {
        assert_eq!(
            StringTable::new(config, dir.as_deref()).unwrap_err(),
            InitError::InvalidConfig,
            "{config:?}"
        );
    }
    let with_arena = Config {
        regions: 1,
        arena_bytes: 2,
    };
    assert_eq!(
        FunctionTable::new(with_arena, dir.as_deref()).unwrap_err(),
        InitError::InvalidConfig
    );
    // Unix needs a lock directory; Windows refuses one.
    let wrong_dir = if cfg!(unix) { None } else { Some(c"C:\\") };
    assert_eq!(
        StringTable::new(with_arena, wrong_dir).unwrap_err(),
        InitError::InvalidConfig
    );
}

#[cfg(all(unix, not(miri)))]
#[test]
fn lock_directory_errors() {
    let config = Config {
        regions: 1,
        arena_bytes: 2,
    };
    let missing = c"/nonexistent-ddprof-lock-dir";
    assert_eq!(
        StringTable::new(config, Some(missing)).unwrap_err(),
        InitError::LockFailed
    );
    let long = CString::new("/".repeat(1000)).unwrap();
    assert_eq!(
        StringTable::new(config, Some(&long)).unwrap_err(),
        InitError::InvalidConfig
    );
}

#[test]
fn handles_are_counted_per_process() {
    let table = strings(1, 1 << 12);
    let id = table.intern("kept").unwrap();
    let second = table.try_clone().unwrap();
    assert_eq!(table.local().handles.load(Ordering::Relaxed), 2);
    // Dropping one handle keeps the table mapped for the other; Miri would
    // report a use after unmap otherwise.
    drop(table);
    assert_eq!(second.resolve(id), Some("kept"));
    assert_eq!(
        second.intern("more").map(Id::index),
        Ok(first_free_id() + 1)
    );

    // A raw handle owns its count until it is taken back.
    let raw = second.try_clone().unwrap().into_raw();
    assert_eq!(second.local().handles.load(Ordering::Relaxed), 2);
    // SAFETY: `raw` came from `into_raw` on a `StringTable`, once.
    let back = unsafe { StringTable::from_raw(raw) };
    assert_eq!(back.resolve(id), Some("kept"));
    drop(back);
    assert_eq!(second.local().handles.load(Ordering::Relaxed), 1);
}

#[test]
fn too_many_handles_is_an_error() {
    let table = strings(1, 1 << 12);
    let handles = &table.local().handles;
    handles.store(MAX_HANDLES, Ordering::Relaxed);
    assert_eq!(table.try_clone().unwrap_err(), TooManyHandles);
    assert_eq!(handles.load(Ordering::Relaxed), MAX_HANDLES);
    handles.store(1, Ordering::Relaxed);
}

#[test]
fn destroy_ignores_the_count() {
    let table = strings(1, 1 << 12);
    let other = table.try_clone().unwrap();
    // `other` must never be used or dropped again.
    core::mem::forget(other);
    // SAFETY: the only other handle is forgotten.
    unsafe { table.destroy() };
}

#[test]
fn an_unguarded_fork_lock_is_held_until_finished() {
    let table = strings(1, 1 << 12);
    table.prepare_fork_unguarded();
    assert_eq!(table.intern("x"), Err(InternError::Busy));
    // SAFETY: held by the call above, released once.
    unsafe { table.finish_fork() };
    assert_eq!(table.intern("x").map(Id::index), Ok(first_free_id()));
}

#[test]
fn a_held_thread_lock_reports_busy() {
    let table = strings(1, 1 << 12);
    let id = table.intern("known").unwrap();
    let guard = table.prepare_fork();
    assert_eq!(table.intern("new"), Err(InternError::Busy));
    guard.finish();
    assert_eq!(table.intern("new").map(Id::index), Ok(first_free_id() + 1));
    assert_eq!(table.intern("known"), Ok(id));
}

#[test]
fn threads_agree_on_ids() {
    use std::thread;

    let threads = 4;
    let count = if cfg!(miri) { 40 } else { 2000 };
    let table = strings(2, 1 << 16);
    let keys: Vec<String> = (0..count).map(|i| format!("key-{i}")).collect();

    let intern = |key: &str| loop {
        match table.intern(key) {
            Err(InternError::Busy) => thread::yield_now(),
            other => break other.unwrap(),
        }
    };
    let results: Vec<Vec<StringId>> = thread::scope(|scope| {
        #[allow(
            clippy::needless_collect,
            reason = "every thread must start before any is joined"
        )]
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (keys, intern, table) = (&keys, &intern, &table);
                scope.spawn(move || {
                    // Each thread walks the keys from a different start.
                    let mut ids = Vec::new();
                    ids.resize(count, StringId::ZERO);
                    for i in 0..count {
                        let k = (i + t * count / threads) % count;
                        ids[k] = intern(&keys[k]);
                        assert_eq!(table.resolve(ids[k]), Some(keys[k].as_str()));
                    }
                    ids
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for ids in &results[1..] {
        assert_eq!(ids, &results[0]);
    }
    let first = first_free_id();
    assert_eq!(table.len(), usize_from(first) + count);
    let mut seen: Vec<u32> = results[0].iter().map(|id| id.index()).collect();
    seen.sort_unstable();
    assert!(
        seen.iter()
            .copied()
            .eq(first..first + u32::try_from(count).unwrap())
    );
}

/// Makes the next writer recover, as if the last one died mid-write.
fn mark_dirty<R: Record>(table: &InternTable<R>) {
    table.local().header().dirty.0.store(1, Ordering::Release);
}

#[test]
fn recovery_rebuilds_lost_buckets_and_the_arena_cursor() {
    let table = strings(2, 1 << 12);
    let keys = ["a", "bb", "ccc", "dddd"];
    let ids: Vec<StringId> = keys.iter().map(|k| table.intern(k).unwrap()).collect();

    // A writer died after publishing but before writing buckets and the
    // cursor: here, every bucket of the current region is lost.
    // SAFETY: no other thread uses the table.
    let bytes = unsafe { table.local().region_mut(0) };
    HashTable::<[u32; 2]>::new(bytes).unwrap();
    table
        .local()
        .header()
        .arena_used
        .0
        .store(0, Ordering::Relaxed);
    mark_dirty(&table);

    // Without the rebuilt buckets this would add a duplicate.
    assert_eq!(table.intern("ccc"), Ok(ids[2]));
    assert_eq!(table.local().header().dirty.0.load(Ordering::Relaxed), 0);
    // Without the restored cursor this would overwrite "" and "a".
    let new = table.intern("eeeee").unwrap();
    assert_eq!(new.index(), first_free_id() + 4);
    for (key, id) in keys.iter().zip(&ids) {
        assert_eq!(table.resolve(*id), Some(*key));
    }
    assert_eq!(table.resolve(StringId::ZERO), Some(""));
    assert_eq!(table.resolve(new), Some("eeeee"));
}

#[test]
fn recovery_seals_a_full_region() {
    let table = functions(2);
    let first = Function {
        start_line: 1,
        ..Function::default()
    };
    let first_id = table.intern(first).unwrap();
    for line in 2..u32::try_from(FUNCTIONS_PER_BLOCK).unwrap() {
        table
            .intern(Function {
                start_line: line,
                ..Function::default()
            })
            .unwrap();
    }
    let publication = &table.local().header().publication;
    assert_eq!(publication.sealed.load(Ordering::Relaxed), 1);

    // A writer died after filling region 0 but before sealing it.
    publication.sealed.store(0, Ordering::Release);
    mark_dirty(&table);

    let next = Function {
        start_line: 0,
        filename: StringId::new(7),
        ..Function::default()
    };
    let next_id = table.intern(next).unwrap();
    assert_eq!(usize_from(next_id.index()), FUNCTIONS_PER_BLOCK);
    assert_eq!(publication.sealed.load(Ordering::Relaxed), 1);
    assert_eq!(table.intern(first), Ok(first_id));
}

#[test]
fn recovery_refuses_inconsistent_counts() {
    let table = strings(1, 1 << 12);
    let a = table.intern("a").unwrap();
    let publication = &table.local().header().publication;
    publication.count.store(u32::MAX, Ordering::Release);
    mark_dirty(&table);
    assert_eq!(table.intern("b"), Err(InternError::Corrupt));
    // The mark stays set, and readers still see published ids.
    assert_eq!(table.local().header().dirty.0.load(Ordering::Relaxed), 1);
    publication.count.store(a.index() + 1, Ordering::Release);
    assert_eq!(table.resolve(a), Some("a"));
}

#[cfg(all(unix, not(miri)))]
mod fork {
    use super::*;

    /// Forks; the child runs `child` and exits with its result, without
    /// unwinding or running the test harness.
    fn in_child(table: &StringTable, child: impl FnOnce() -> i32) -> i32 {
        let guard = table.prepare_fork();
        // SAFETY: the child only uses the table, which does not allocate,
        // and then exits immediately.
        let pid = unsafe { libc::fork() };
        guard.finish();
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let code = child();
            // SAFETY: ends the child without running the parent's cleanup.
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: waits for the child just forked.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "child crashed: {status}");
        libc::WEXITSTATUS(status)
    }

    #[test]
    fn children_share_ids_with_the_parent() {
        let table = strings(1, 1 << 12);
        let before = table.intern("before fork").unwrap();
        let code = in_child(&table, || {
            let ok = table.intern("before fork") == Ok(before);
            match table.intern("from child") {
                Ok(id) if ok => i32::try_from(id.index()).unwrap_or(-1),
                _ => -1,
            }
        });
        let id = Id::new(u32::try_from(code).unwrap());
        assert_eq!(table.resolve(id), Some("from child"));
        assert_eq!(table.intern("from child"), Ok(id));
        assert_eq!(
            table.intern("from parent").map(Id::index),
            Ok(id.index() + 1)
        );
    }

    #[test]
    fn a_writer_that_dies_holding_the_locks_is_recovered() {
        let table = strings(1, 1 << 12);
        let id = table.intern("published").unwrap();
        in_child(&table, || {
            // Take both locks and die holding them, mid-write.
            match table.local().writer() {
                Ok(writer) => {
                    core::mem::forget(writer);
                    0
                }
                Err(_) => 1,
            }
        });
        // The kernel released the child's process lock; its mark remains.
        assert_eq!(table.local().header().dirty.0.load(Ordering::Relaxed), 1);
        assert_eq!(table.intern("published"), Ok(id));
        assert_eq!(table.local().header().dirty.0.load(Ordering::Relaxed), 0);
        assert_eq!(table.intern("after").map(Id::index), Ok(id.index() + 1));
    }
}

#[test]
fn strings_with_reserved_write_their_ids() {
    let config = Config {
        regions: 1,
        arena_bytes: 1 << 12,
    };
    let reserved = ["<python function>", "<unknown file>", "<python function>"];
    let mut ids = [StringId::ZERO; 3];
    let table =
        StringTable::with_reserved(config, lock_dir().as_deref(), &reserved, &mut ids).unwrap();
    for (id, value) in ids.iter().zip(reserved) {
        assert_eq!(table.resolve(*id), Some(value));
        assert_eq!(table.intern(value), Ok(*id), "already present");
    }
    assert_eq!(ids[0], ids[2], "a repeat gets the existing id");
    assert_ne!(ids[0], StringId::ZERO);

    // Owned strings work too.
    let owned: Vec<String> = reserved.iter().map(|s| String::from(*s)).collect();
    let mut owned_ids = [StringId::ZERO; 3];
    StringTable::with_reserved(config, lock_dir().as_deref(), &owned, &mut owned_ids).unwrap();
    assert_eq!(owned_ids, ids, "ids are the same in every table");
}

#[test]
fn reserved_errors_leave_zero_ids() {
    let config = Config {
        regions: 1,
        arena_bytes: 1 << 12,
    };
    let mut ids = [Id::new(7); 2];
    assert_eq!(
        StringTable::with_reserved(config, lock_dir().as_deref(), &["only one"], &mut ids).err(),
        Some(InitError::InvalidConfig),
        "lengths differ"
    );
    assert_eq!(ids, [StringId::ZERO; 2]);

    let long = "x".repeat(usize::from(u16::MAX) + 1);
    let mut ids = [Id::new(7); 2];
    assert_eq!(
        StringTable::with_reserved(config, lock_dir().as_deref(), &["fits", &long], &mut ids).err(),
        Some(InitError::InvalidConfig),
        "too long"
    );
    assert_eq!(ids, [StringId::ZERO; 2]);
}

#[test]
fn functions_with_reserved_write_their_ids() {
    let config = Config {
        regions: 1,
        arena_bytes: 0,
    };
    let python = Function {
        name: Id::new(1),
        system_name: Id::new(1),
        filename: Id::new(2),
        start_line: 0,
    };
    let native = Function {
        filename: Id::new(3),
        ..python
    };
    let mut ids = [FunctionId::ZERO; 2];
    let table =
        FunctionTable::with_reserved(config, lock_dir().as_deref(), &[python, native], &mut ids)
            .unwrap();
    assert_eq!(table.resolve(ids[0]), Some(python));
    assert_eq!(table.resolve(ids[1]), Some(native));
    assert_ne!(ids[0], ids[1]);
}
