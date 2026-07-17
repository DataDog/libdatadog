// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    ptr::{self, NonNull},
    sync::atomic::Ordering,
};
use std::io;
#[cfg(not(feature = "process-context-writer"))]
use std::sync::OnceLock;

use super::{AtomicPublishedHeader, HEADER_ADDRESS_MASK, PUBLISHER_PID_SHIFT};
use crate::otel_process_ctx::reader::ReaderBackend;

mod sealed {
    pub struct MacosReaderBackend;
}

pub(crate) use sealed::MacosReaderBackend;

impl ReaderBackend for MacosReaderBackend {
    type MemoryCopy = super::copy_pipe::CopyPipe;

    fn discover_header() -> io::Result<NonNull<u8>> {
        let global = process_context_global()?;
        let current_pid = std::process::id();

        let value = global.load(Ordering::Acquire);
        let (publisher_pid, header) = unpack_published_header(value);

        // After fork, the global retains the parent's publication even if the mapping itself was
        // excluded from inheritance. Treat that stale publication exactly like an unpublished one.
        if publisher_pid != current_pid {
            return Err(not_found());
        }

        NonNull::new(header).ok_or_else(not_found)
    }
}

fn not_found() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "no process context is published by the current process",
    )
}

#[cfg(feature = "process-context-writer")]
fn process_context_global() -> io::Result<&'static AtomicPublishedHeader> {
    Ok(&super::writer::datadog_process_ctx_v1)
}

#[cfg(not(feature = "process-context-writer"))]
fn process_context_global() -> io::Result<&'static AtomicPublishedHeader> {
    static SYMBOL_ADDRESS: OnceLock<usize> = OnceLock::new();

    let address = match SYMBOL_ADDRESS.get() {
        Some(address) => *address,
        None => {
            // SAFETY: RTLD_DEFAULT searches globally visible symbols in the current process and
            // the symbol name is NUL-terminated.
            let symbol = unsafe {
                libc::dlsym(
                    libc::RTLD_DEFAULT,
                    c"datadog_process_ctx_v1".as_ptr().cast(),
                )
            };
            let address = NonNull::new(symbol)
                .map(|symbol| symbol.as_ptr() as usize)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "couldn't resolve datadog_process_ctx_v1 in the current process",
                    )
                })?;

            // may fail, another thread may have already set the address
            let _ = SYMBOL_ADDRESS.set(address);
            SYMBOL_ADDRESS.get().copied().unwrap_or(address)
        }
    };

    // SAFETY: dlsym returned the address of the exported AtomicPublishedHeader. Successful
    // addresses are cached only for the lifetime of the process.
    Ok(unsafe { &*(address as *const AtomicPublishedHeader) })
}

fn unpack_published_header(value: u128) -> (u32, *mut u8) {
    let publisher_pid = (value >> PUBLISHER_PID_SHIFT) as u32;
    let header_address = (value & HEADER_ADDRESS_MASK) as usize;
    (
        publisher_pid,
        ptr::with_exposed_provenance_mut(header_address),
    )
}

#[cfg(all(test, not(feature = "process-context-writer")))]
mod tests {
    use core::{ptr, sync::atomic::Ordering};

    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{KeyValue, ProcessContext};

    use super::super::{AtomicPublishedHeader, PUBLISHER_PID_SHIFT};
    use crate::{
        datadog_process_ctx::ProcessContextSelfReader,
        otel_process_ctx::{reader::MappingHeaderSnapshot, PROCESS_CTX_VERSION, SIGNATURE},
    };

    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static datadog_process_ctx_v1: AtomicPublishedHeader = AtomicPublishedHeader::new(0);

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
        let published_header = (u128::from(std::process::id()) << PUBLISHER_PID_SHIFT)
            | ptr::from_ref(&header).expose_provenance() as u128;
        datadog_process_ctx_v1.store(published_header, Ordering::Release);

        let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
        assert_eq!(reader.read().expect("read should succeed"), expected);

        datadog_process_ctx_v1.store(0, Ordering::Relaxed);
    }
}
