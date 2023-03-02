// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use std::{
    ffi::{CStr, CString},
    ptr,
};

use nix::libc;
use smallvec::SmallVec;

pub mod raw_env {
    use nix::libc;

    use super::CListMutPtr;
    /// # Safety
    ///
    /// caller must ensure its safe to read `environ` global value
    #[inline]
    unsafe fn environ() -> *mut *const *const libc::c_char {
        extern "C" {
            static mut environ: *const *const libc::c_char;
        }
        std::ptr::addr_of_mut!(environ)
    }

    /// # Safety
    ///
    /// caller must ensure its safe to read `environ` global value
    pub unsafe fn as_clist<'a>() -> CListMutPtr<'a> {
        CListMutPtr::from_raw_parts(*environ() as *mut *const libc::c_char)
    }

    /// # Safety
    ///
    /// caller must ensure its safe to read and write to `environ` global value
    /// returned pointer validity depends on the data pointed to by environ global variable
    pub unsafe fn swap(new: *const *const libc::c_char) -> *const *const libc::c_char {
        let old = *environ();
        *environ() = new;
        old
    }
}

pub struct Symbol<T> {
    ptr: *mut libc::c_void,
    phantom: std::marker::PhantomData<T>,
}

/// # Safety
///
/// caller must ensure that the symbol name reflects will resolve to supplied type
pub unsafe fn dlsym<T>(handle: *mut libc::c_void, str: &CStr) -> Option<Symbol<T>> {
    let ptr = libc::dlsym(handle, str.as_ptr());
    if ptr.is_null() {
        return None;
    }
    Some(Symbol {
        ptr,
        phantom: std::marker::PhantomData,
    })
}

impl<T> ::std::ops::Deref for Symbol<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe {
            &*(&self.ptr as *const *mut _ as *const T)
        }
    }
}

/// returns the path of the library from which the symbol pointed to by *addr* was loaded from
///
/// # Safety
/// addr must be a valid address accepted by dladdr(2)
pub unsafe fn get_dl_path_raw(addr: *const libc::c_void) -> (Option<CString>, Option<CString>) {
    let mut info = libc::Dl_info {
        dli_fname: ptr::null(),
        dli_fbase: ptr::null_mut(),
        dli_sname: ptr::null(),
        dli_saddr: ptr::null_mut(),
    };
    let res = libc::dladdr(addr, &mut info as *mut libc::Dl_info);

    if res == 0 {
        return (None, None);
    }
    let path_name = if info.dli_fbase.is_null() || info.dli_fname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_fname).to_owned())
    };

    let symbol_name = if info.dli_saddr.is_null() || info.dli_sname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_sname).to_owned())
    };

    (path_name, symbol_name)
}

pub struct ExecVec<const N: usize> {
    heap_items: SmallVec<[CString; 0]>,
    // Always NULL ptr terminated
    ptrs: SmallVec<[*const libc::c_char; N]>,
}

impl<const N: usize> ExecVec<N> {
    pub fn as_ptr(&self) -> *const *const libc::c_char {
        self.ptrs.as_ptr()
    }

    pub fn empty() -> Self {
        let mut ptrs = SmallVec::new();
        ptrs.push(std::ptr::null());
        Self {
            heap_items: SmallVec::new(),
            ptrs: ptrs,
        }
    }

    pub fn push_cstr(&mut self, item: &CStr) {
        self.push_ptr(item.as_ptr());
    }

    pub fn push_cstring(&mut self, item: CString) {
        let ptr = item.as_ptr();
        self.heap_items.push(item);
        self.push_ptr(ptr);
    }

    pub fn push_ptr(&mut self, item: *const libc::c_char) {
        let l = self.ptrs.len();
        // replace previous trailing null with ptr to the item
        self.ptrs[l - 1] = item;
        self.ptrs.push(std::ptr::null());
    }
}

// pub struct CListMutPtr<'a

pub struct CListMutPtr<'a> {
    inner: &'a mut [*const libc::c_char],
    elements: usize,
}

impl<'a> CListMutPtr<'a> {
    /// # Safety
    ///
    /// pointers passed to this method must remain valid for the lifetime of CListMutPtr object
    pub unsafe fn from_raw_parts(ptr: *mut *const libc::c_char) -> Self {
        let mut len = 0;
        while !(*ptr.add(len)).is_null() {
            len += 1;
        }
        Self {
            inner: std::slice::from_raw_parts_mut(ptr, len + 1),
            elements: len,
        }
    }

    pub fn as_ptr(&self) -> *const *const libc::c_char {
        self.inner.as_ptr()
    }

    /// Copies data into owned Vec<CString> 
    /// 
    /// # Safety
    /// 
    /// caller must ensure the underlying pointer is safe to read and points to valid null teminated list 
    /// of c strings
    pub unsafe fn to_owned_vec(&self) -> Vec<CString> {
        let mut vec = Vec::with_capacity(self.elements);
        for i in 0..self.elements {
            let elem = CStr::from_ptr(self.inner[i]);
            vec.push(elem.to_owned());
        }

        vec
    }

    /// remove entry from a slice, shifting other entries in its place
    ///
    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn remove_entry<F: Fn(&[u8]) -> bool>(
        &mut self,
        predicate: F,
    ) -> Option<*const libc::c_char> {
        for i in (0..self.elements).rev() {
            let elem = CStr::from_ptr(self.inner[i]);
            if predicate(elem.to_bytes()) {
                for src in i + 1..self.elements {
                    self.inner[src - 1] = self.inner[src]
                }
                self.elements -= 1;
                return Some(elem.as_ptr());
            }
        }

        None
    }

    /// replace entry in a slice
    ///
    /// # Safety
    ///
    /// entries in self.inner must be valid null terminated c strings
    ///
    /// new_entry must live as long as CListMutPtr a
    pub unsafe fn replace_entry<F: Fn(&[u8]) -> bool>(
        &mut self,
        predicate: F,
        new_entry: &CStr,
    ) -> Option<*const libc::c_char> {
        for i in 0..self.elements {
            let elem = CStr::from_ptr(self.inner[i]);
            if predicate(elem.to_bytes()) {
                self.inner[i] = new_entry.as_ptr();
                return Some(elem.as_ptr());
            }
        }
        None
    }

    /// create exec vec with size N allocated on stack, and copy all pointer there,
    /// if there are more entries than N - ExecVec will allocate more space on heap
    ///
    /// # Safety
    ///
    /// entries in self.inner must be valid null terminated c strings valid for the lifetime of ExecVec
    ///
    /// if there are more elements in self.inner than N - caller must ensure the context allows safe heap allocations
    pub unsafe fn into_exec_vec<const N: usize>(self) -> ExecVec<N> {
        let mut vec: ExecVec<N> = ExecVec::empty();
        for i in 0..self.elements {
            vec.push_ptr(self.inner[i]);
        }
        vec
    }
}
