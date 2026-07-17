// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(feature = "process-context-writer"))]
use core::{ffi::c_void, mem::size_of, ptr};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};
use std::io;
#[cfg(not(feature = "process-context-writer"))]
use std::sync::OnceLock;

#[cfg(not(feature = "process-context-writer"))]
use crate::otel_process_ctx::last_error;
use crate::otel_process_ctx::reader::ReaderBackend;

#[cfg(not(feature = "process-context-writer"))]
type Handle = *mut c_void;

#[cfg(not(feature = "process-context-writer"))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn GetProcAddress(module: Handle, proc_name: *const u8) -> *mut c_void;
    fn K32EnumProcessModules(
        process: Handle,
        modules: *mut Handle,
        size: u32,
        needed: *mut u32,
    ) -> i32;
}

mod sealed {
    pub struct WindowsReaderBackend;
}

pub(crate) use sealed::WindowsReaderBackend;

impl ReaderBackend for WindowsReaderBackend {
    type MemoryCopy = super::copy_pipe::CopyPipe;

    fn discover_header() -> io::Result<NonNull<u8>> {
        let header = process_context_global()?.load(Ordering::Acquire);
        NonNull::new(header).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no process context is published")
        })
    }
}

#[cfg(feature = "process-context-writer")]
fn process_context_global() -> io::Result<&'static AtomicPtr<u8>> {
    Ok(&super::writer::datadog_process_ctx_v1)
}

#[cfg(not(feature = "process-context-writer"))]
fn process_context_global() -> io::Result<&'static AtomicPtr<u8>> {
    static SYMBOL_ADDRESS: OnceLock<usize> = OnceLock::new();

    let address = match SYMBOL_ADDRESS.get() {
        Some(address) => *address,
        None => {
            let address = find_process_context_global()?;
            // may fail, another thread may have already set the address
            let _ = SYMBOL_ADDRESS.set(address);
            SYMBOL_ADDRESS.get().copied().unwrap_or(address)
        }
    };

    // SAFETY: GetProcAddress returned the address of the exported AtomicPtr<u8>. Successful
    // addresses are cached only for the lifetime of the process.
    Ok(unsafe { &*(address as *const AtomicPtr<u8>) })
}

#[cfg(not(feature = "process-context-writer"))]
fn find_process_context_global() -> io::Result<usize> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle valid in the current process.
    let process = unsafe { GetCurrentProcess() };
    let mut bytes_needed = 0;
    // SAFETY: a zero-sized first call queries the required module-array size.
    if unsafe { K32EnumProcessModules(process, ptr::null_mut(), 0, &mut bytes_needed) } == 0 {
        return Err(last_error("failed to enumerate loaded process modules"));
    }

    loop {
        let module_count = bytes_needed as usize / size_of::<Handle>();
        let mut modules = vec![ptr::null_mut(); module_count];
        let buffer_size = u32::try_from(modules.len() * size_of::<Handle>())
            .map_err(|_| io::Error::other("loaded process module list was too large"))?;
        let mut actual_bytes_needed = 0;
        // SAFETY: modules provides buffer_size writable bytes and the out-parameter is valid.
        if unsafe {
            K32EnumProcessModules(
                process,
                modules.as_mut_ptr(),
                buffer_size,
                &mut actual_bytes_needed,
            )
        } == 0
        {
            return Err(last_error("failed to enumerate loaded process modules"));
        }

        if actual_bytes_needed > buffer_size {
            bytes_needed = actual_bytes_needed;
            continue;
        }

        let actual_module_count = actual_bytes_needed as usize / size_of::<Handle>();
        for module in modules.into_iter().take(actual_module_count) {
            // SAFETY: module was returned by K32EnumProcessModules and the symbol name is
            // NUL-terminated.
            let symbol =
                unsafe { GetProcAddress(module, c"datadog_process_ctx_v1".as_ptr().cast()) };
            if let Some(symbol) = NonNull::new(symbol) {
                return Ok(symbol.as_ptr() as usize);
            }
        }

        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "couldn't resolve datadog_process_ctx_v1 in the current process",
        ));
    }
}

#[cfg(all(test, not(feature = "process-context-writer")))]
mod tests {
    use core::{
        ptr,
        sync::atomic::{AtomicPtr, Ordering},
    };

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{KeyValue, ProcessContext};

    use crate::{
        datadog_process_ctx::ProcessContextSelfReader,
        otel_process_ctx::{reader::MappingHeaderSnapshot, PROCESS_CTX_VERSION, SIGNATURE},
    };

    #[cfg(target_env = "msvc")]
    #[used]
    #[link_section = ".drectve"]
    static EXPORT_DATADOG_PROCESS_CTX_V1: [u8; 31] = *b" /EXPORT:datadog_process_ctx_v1";

    #[cfg(target_env = "gnu")]
    #[used]
    #[link_section = ".drectve"]
    static EXPORT_DATADOG_PROCESS_CTX_V1: [u8; 31] = *b" -export:datadog_process_ctx_v1";

    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static datadog_process_ctx_v1: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

    #[test]
    fn reads_context_from_exported_global() {
        let payload = b"\x12\x05\x0a\x03key";
        let expected = ProcessContext {
            resource: None,
            extra_attributes: vec![KeyValue {
                key: "key".to_string(),
                value: None,
                key_ref: 0,
            }],
        };
        let header = MappingHeaderSnapshot {
            signature: *SIGNATURE,
            version: PROCESS_CTX_VERSION,
            payload_size: payload.len() as u32,
            monotonic_published_at_ns: 1,
            payload_ptr: payload.as_ptr(),
        };
        datadog_process_ctx_v1.store(ptr::from_ref(&header).cast_mut().cast(), Ordering::Release);

        let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
        assert_eq!(reader.read().expect("read should succeed"), expected);

        datadog_process_ctx_v1.store(ptr::null_mut(), Ordering::Relaxed);
    }
}
