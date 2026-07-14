// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};
use std::io;
#[cfg(not(feature = "process-context-writer"))]
use std::sync::OnceLock;

use super::ReaderPlatform;

pub(super) struct HeaderDiscovery;

impl ReaderPlatform for HeaderDiscovery {
    fn discover_header() -> io::Result<NonNull<u8>> {
        let header = process_context_global()?.load(Ordering::Acquire);
        NonNull::new(header).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no process context is published")
        })
    }
}

#[cfg(feature = "process-context-writer")]
fn process_context_global() -> io::Result<&'static AtomicPtr<u8>> {
    Ok(&crate::otel_process_ctx::writer::macos::otel_process_ctx_v2)
}

#[cfg(not(feature = "process-context-writer"))]
fn process_context_global() -> io::Result<&'static AtomicPtr<u8>> {
    static SYMBOL_ADDRESS: OnceLock<usize> = OnceLock::new();

    let address = match SYMBOL_ADDRESS.get() {
        Some(address) => *address,
        None => {
            // SAFETY: RTLD_DEFAULT searches globally visible symbols in the current process and
            // the symbol name is NUL-terminated.
            let symbol =
                unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"otel_process_ctx_v2".as_ptr().cast()) };
            let address = NonNull::new(symbol)
                .map(|symbol| symbol.as_ptr() as usize)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "couldn't resolve otel_process_ctx_v2 in the current process",
                    )
                })?;

            // may fail, another thread may have already set the address
            let _ = SYMBOL_ADDRESS.set(address);
            SYMBOL_ADDRESS.get().copied().unwrap_or(address)
        }
    };

    // SAFETY: dlsym returned the address of the exported AtomicPtr<u8>. Successful addresses are
    // cached only for the lifetime of the process.
    Ok(unsafe { &*(address as *const AtomicPtr<u8>) })
}

#[cfg(all(test, not(feature = "process-context-writer")))]
mod tests {
    use core::{
        ptr,
        sync::atomic::{AtomicPtr, Ordering},
    };

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{KeyValue, ProcessContext};

    use crate::otel_process_ctx::{
        MappingHeaderSnapshot, ProcessContextSelfReader, PROCESS_CTX_VERSION, SIGNATURE,
    };

    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static otel_process_ctx_v2: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

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
        otel_process_ctx_v2.store(ptr::from_ref(&header).cast_mut().cast(), Ordering::Release);

        let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
        assert_eq!(reader.read().expect("read should succeed"), expected);

        otel_process_ctx_v2.store(ptr::null_mut(), Ordering::Relaxed);
    }
}
