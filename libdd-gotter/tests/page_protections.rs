// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(target_os = "linux", target_pointer_width = "64", not(miri)))]

use libc::{PROT_READ, PROT_WRITE};
use libdd_gotter::{PageProtGuard, read_proc_maps};

struct Page {
    ptr: *mut libc::c_void,
    size: usize,
}

impl Page {
    fn new() -> Self {
        unsafe {
            let size = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap();
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(ptr, libc::MAP_FAILED);
            ptr.cast::<usize>().write(42);
            Self { ptr, size }
        }
    }

    fn protect(&self, prot: i32) {
        assert_eq!(unsafe { libc::mprotect(self.ptr, self.size, prot) }, 0);
    }

    fn protection(&self) -> i32 {
        let addr = self.ptr as usize;
        read_proc_maps()
            .iter()
            .find(|entry| addr >= entry.start && addr < entry.end)
            .expect("page is mapped")
            .prot
    }

    fn value(&self) -> usize {
        unsafe { self.ptr.cast::<usize>().read() }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.size) };
    }
}

#[test]
fn skips_pages_missing_from_the_snapshot() {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    let mut guard = PageProtGuard::new();

    // Find an address verified absent from the snapshot. A plain mmap after
    // the snapshot can land on an address that was mapped at snapshot time
    // Scanning the snapshot for a gap and using MAP_FIXED avoids this.
    let mut gap = 0x10000; // above typical vm.mmap_min_addr
    while guard.original_prot(gap).is_some() {
        gap += page_size;
    }
    let ptr = unsafe {
        libc::mmap(
            gap as *mut libc::c_void,
            page_size,
            PROT_READ | PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED);
    unsafe { ptr.cast::<usize>().write(42) };

    assert_eq!(guard.original_prot(ptr as usize), None);
    let patched = unsafe { guard.override_entry(ptr as usize, 99) };
    drop(guard);

    assert!(!patched, "unknown page protections must not be guessed");
    assert_eq!(unsafe { ptr.cast::<usize>().read() }, 42);
    let prot = read_proc_maps()
        .iter()
        .find(|e| ptr as usize >= e.start && (ptr as usize) < e.end)
        .expect("page is mapped")
        .prot;
    assert_eq!(prot, PROT_READ | PROT_WRITE);
    unsafe { libc::munmap(ptr, page_size) };
}

#[test]
fn restores_read_only_and_writable_pages() {
    for prot in [PROT_READ, PROT_READ | PROT_WRITE] {
        let page = Page::new();
        page.protect(prot);
        let mut guard = PageProtGuard::new();

        assert!(unsafe { guard.override_entry(page.ptr as usize, 99) });
        assert!(unsafe { guard.override_entry(page.ptr as usize, 100) });
        drop(guard);

        assert_eq!(page.value(), 100);
        assert_eq!(page.protection(), prot);
    }
}

#[test]
fn restores_protection_when_unwinding() {
    let page = Page::new();
    page.protect(PROT_READ);

    let result = std::panic::catch_unwind(|| {
        let mut guard = PageProtGuard::new();
        assert!(unsafe { guard.override_entry(page.ptr as usize, 99) });
        panic!("interrupted patch pass");
    });

    assert!(result.is_err());
    assert_eq!(page.value(), 99);
    assert_eq!(page.protection(), PROT_READ);
}
