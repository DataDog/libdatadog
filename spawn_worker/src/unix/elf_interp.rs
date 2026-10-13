// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Locating and rewriting the program interpreter (PT_INTERP) of the embedded
//! trampoline executable.
//!
//! The linker bakes the interpreter of the libc the trampoline was built
//! against into it, e.g. `/lib/ld-musl-x86_64.so.1`. A trampoline built
//! against musl but linked with glibc-compatible library names can run on
//! glibc too, but only through glibc's loader, whose path differs. The build
//! script therefore links the trampoline with the default interpreter padded
//! with leading slashes (an equivalent path) to [`INTERP_CAPACITY`] bytes, and
//! the spawner overwrites it with the loader of the running process.
//!
//! Shared between build.rs and the crate, so it has no dependencies.

#![allow(dead_code)] // each includer uses a different subset

use std::ops::Range;

/// Size of the padded PT_INTERP segment, including the terminating NUL.
pub const INTERP_CAPACITY: usize = 256;

/// Returns the file byte range of the PT_INTERP segment of a 64-bit
/// little-endian ELF image, NUL included.
pub fn interp_range(elf: &[u8]) -> Option<Range<usize>> {
    const PT_INTERP: u32 = 3;

    if elf.get(..4)? != b"\x7fELF" || *elf.get(4)? != 2 || *elf.get(5)? != 1 {
        return None;
    }
    let phoff = usize::try_from(read_u64(elf, 0x20)?).ok()?;
    let phentsize = usize::from(read_u16(elf, 0x36)?);
    let phnum = usize::from(read_u16(elf, 0x38)?);

    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if read_u32(elf, ph)? != PT_INTERP {
            continue;
        }
        let offset = usize::try_from(read_u64(elf, ph.checked_add(0x08)?)?).ok()?;
        let filesz = usize::try_from(read_u64(elf, ph.checked_add(0x20)?)?).ok()?;
        let end = offset.checked_add(filesz)?;
        return (end <= elf.len()).then_some(offset..end);
    }
    None
}

/// Returns the interpreter path, without its terminating NUL.
pub fn interp_path(elf: &[u8]) -> Option<&[u8]> {
    let segment = &elf[interp_range(elf)?];
    let len = segment.iter().position(|&b| b == 0)?;
    Some(&segment[..len])
}

/// Replaces the interpreter path in place. Returns false, leaving the image
/// untouched, when the new path does not fit in the existing segment.
pub fn set_interp_path(elf: &mut [u8], path: &[u8]) -> bool {
    let Some(range) = interp_range(elf) else {
        return false;
    };
    let segment = &mut elf[range];
    if path.is_empty() || path.len() >= segment.len() || path.contains(&0) {
        return false;
    }
    segment[..path.len()].copy_from_slice(path);
    segment[path.len()..].fill(0);
    true
}

/// Returns `path` preceded by as many slashes as needed for it, plus its NUL,
/// to fill [`INTERP_CAPACITY`] bytes. `None` when it is already too long.
pub fn padded_interp_path(path: &[u8]) -> Option<Vec<u8>> {
    let pad = INTERP_CAPACITY.checked_sub(path.len().checked_add(1)?)?;
    let mut padded = vec![b'/'; pad];
    padded.extend_from_slice(path);
    Some(padded)
}

fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        buf.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buf.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        buf.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}
