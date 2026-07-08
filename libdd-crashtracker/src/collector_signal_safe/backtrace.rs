// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;

use super::sys;
use crate::shared::ucontext::ucontext_registers;

const MMAP_MIN_ADDR: usize = 0x10000;
const MAX_FRAME_SIZE: usize = 0x100000;
const WORD: usize = core::mem::size_of::<usize>();

#[inline]
fn aligned_8(p: usize) -> bool {
    p & 0b111 == 0
}

#[inline]
fn word_at(rec: &[u8; 2 * WORD], i: usize) -> usize {
    let mut b = [0u8; WORD];
    b.copy_from_slice(&rec[i * WORD..(i + 1) * WORD]);
    usize::from_ne_bytes(b)
}

fn walk_fp(out: &mut [usize], mut n: usize, self_pid: i32, mut fp: usize) -> usize {
    while n < out.len() && fp != 0 {
        if !aligned_8(fp) || fp < MMAP_MIN_ADDR {
            break;
        }

        let mut rec = [0u8; 2 * WORD];
        if !sys::read_own_mem(self_pid, fp, &mut rec) {
            break;
        }

        let prev_fp = word_at(&rec, 0);
        let ret = word_at(&rec, 1);
        if ret <= MMAP_MIN_ADDR {
            break;
        }

        out[n] = ret;
        n += 1;

        if prev_fp <= fp || prev_fp - fp > MAX_FRAME_SIZE {
            break;
        }
        fp = prev_fp;
    }
    n
}

fn arch_seed(uc: &libc::ucontext_t, out: &mut [usize]) -> (usize, usize) {
    let Some(registers) = ucontext_registers(uc) else {
        return (0, 0);
    };
    let mut n = 0;
    if registers.ip != 0 {
        out[0] = registers.ip;
        n = 1;
    }

    let fp_walkable = registers.fp != 0 && aligned_8(registers.fp) && registers.fp >= MMAP_MIN_ADDR;
    if !fp_walkable && registers.link != 0 && n < out.len() {
        out[n] = registers.link;
        n += 1;
    }
    (n, registers.fp)
}

pub fn backtrace_from_ucontext(
    out: &mut [usize],
    ucontext: *const c_void,
    self_pid: i32,
    allow_memory_read: bool,
) -> usize {
    if out.is_empty() || ucontext.is_null() {
        return 0;
    }

    let uc = unsafe { &*(ucontext as *const libc::ucontext_t) };
    let (n, fp) = arch_seed(uc, out);
    if allow_memory_read {
        walk_fp(out, n, self_pid, fp)
    } else {
        n
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn pid() -> i32 {
        unsafe { libc::getpid() }
    }

    #[test]
    fn walks_a_synthetic_chain() {
        let mut frames = [[0usize; 2]; 3];
        let base = frames.as_ptr() as usize;
        let stride = core::mem::size_of::<[usize; 2]>();
        frames[0] = [base + stride, 0x401000];
        frames[1] = [base + 2 * stride, 0x402000];
        frames[2] = [0, 0x403000];
        // The frame array is read through `process_vm_readv`, which is opaque to lints.
        core::hint::black_box(&frames);

        let mut out = [0usize; 8];
        let n = walk_fp(&mut out, 0, pid(), base);
        assert_eq!(n, 3);
        assert_eq!(&out[..3], &[0x401000, 0x402000, 0x403000]);
    }

    #[test]
    fn forked_child_walks_inherited_snapshot_with_self_pid() {
        let mut frames = [[0usize; 2]; 3];
        let base = frames.as_ptr() as usize;
        let stride = core::mem::size_of::<[usize; 2]>();
        frames[0] = [base + stride, 0x401000];
        frames[1] = [base + 2 * stride, 0x402000];
        frames[2] = [0, 0x403000];
        // The child reads this array through `process_vm_readv`, which is opaque to lints.
        core::hint::black_box(&frames);

        let child = unsafe { sys::fork_raw() };
        if child == 0 {
            let mut out = [0usize; 8];
            let n = walk_fp(&mut out, 0, sys::getpid(), base);
            let ok = n == 3 && out[..3] == [0x401000, 0x402000, 0x403000];
            sys::exit_process(if ok { 0 } else { 1 });
        }

        assert!(child > 0, "fork failed: {child}");
        match sys::reap_child(child as i32, 1_000, 10, 100) {
            sys::ChildReap::Reaped(status) => {
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 0);
            }
            sys::ChildReap::NoChild => panic!("child was already reaped"),
            sys::ChildReap::WaitFailed(errno) => panic!("waitpid failed: {errno}"),
            sys::ChildReap::TimedOut => panic!("child timed out"),
        }
    }

    #[test]
    fn stops_on_unmapped_fp() {
        let mut out = [0usize; 8];
        assert_eq!(walk_fp(&mut out, 0, pid(), 0x10000), 0);
    }

    #[test]
    fn honors_out_capacity() {
        let mut frames = [[0usize; 2]; 3];
        let base = frames.as_ptr() as usize;
        let stride = core::mem::size_of::<[usize; 2]>();
        frames[0] = [base + stride, 0x401000];
        frames[1] = [base + 2 * stride, 0x402000];
        frames[2] = [0, 0x403000];
        // The frame array is read through `process_vm_readv`, which is opaque to lints.
        core::hint::black_box(&frames);

        let mut out = [0usize; 2];
        let n = walk_fp(&mut out, 0, pid(), base);
        assert_eq!(n, 2);
        assert_eq!(&out[..2], &[0x401000, 0x402000]);
    }

    #[test]
    fn read_own_mem_roundtrips_and_rejects_unmapped() {
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut dst = [0u8; 8];
        assert!(sys::read_own_mem(pid(), src.as_ptr() as usize, &mut dst));
        assert_eq!(dst, src);
        assert!(!sys::read_own_mem(pid(), 0x10000, &mut dst));
    }
}
