// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implementation of the [OTEL process
//! context specification](https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md).
//!
//! Note: the Linux implementation follows the discovery method described in the OTEL process
//! specification linked above, that is, uses a memfd or a named mapping with the name OTEL_CTX.
//! This is a strategy only viable on Linux, since MacOS and Windows do not have those exact
//! features. Here, the MacOS and Windows implementations, on the other hand, use a global atomic
//! pointer to the mapping header that is published as a symbol named `otel_process_ctx_v2`.
//! SUCH MECHANISM IS NOT PART OF THE SPECIFICATION, which deals only with Linux.
//!
//! The update/read protocol is seqlock-style: the publisher marks the mapping as unavailable,
//! writes the payload metadata, publishes a non-zero version, and readers accept a copy only if
//! the version they observed before copying still matches afterward. The general algorithm and
//! the C++ memory-model constraints are described in Boehm's
//! [Can Seqlocks Get Along With Programming Language Memory Models?](https://web.archive.org/web/20211106170334/https://www.hpl.hp.com/techreports/2012/HPL-2012-68.pdf).
//! Linux has its own [seqlock/seqcount implementation](https://github.com/torvalds/linux/blob/master/include/linux/seqlock.h),
//! but its barriers are specified by the Linux kernel memory model, not by the C++/Rust models.
//!
//! This implementation differs from the usual odd/even counter form in two ways: `0` is the
//! in-progress sentinel, and each non-zero `monotonic_published_at_ns` value is the
//! reader-visible version. Updates force that timestamp to advance so readers can detect torn
//! reads even when the clock returns the same value twice. Concurrent writers are rejected, and
//! retry policy is left to the reader's caller.

#[cfg(feature = "process-context-reader")]
mod reader;
#[cfg(feature = "process-context-writer")]
mod writer;
#[cfg(all(target_os = "linux", not(target_has_atomic = "64")))]
compile_error!("OTel process context requires 64-bit atomics on Linux");
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(feature = "process-context-reader")]
pub use reader::ProcessContextSelfReader;
#[cfg(feature = "process-context-writer")]
pub use writer::{publish, unpublish};

/// Current version of the process context format
pub const PROCESS_CTX_VERSION: u32 = 2;
/// Signature bytes for identifying process context mappings
pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
/// Sentinel timestamp indicating that the context is unpublished or being updated.
const UNPUBLISHED_OR_UPDATING: u64 = 0;

#[repr(C)]
#[cfg(feature = "process-context-reader")]
struct MappingHeaderSnapshot {
    signature: [u8; 8],
    version: u32,
    payload_size: u32,
    monotonic_published_at_ns: u64,
    payload_ptr: *const u8,
}

#[cfg(all(
    test,
    feature = "process-context-reader",
    feature = "process-context-writer"
))]
#[serial_test::serial]
mod tests {
    use core::time::Duration;

    use super::ProcessContextSelfReader;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
        any_value, AnyValue, KeyValue, ProcessContext,
    };
    use prost::Message;

    #[cfg(target_os = "linux")]
    mod linux {
        use core::ptr;
        use std::io;

        use super::super::{reader, MappingHeaderSnapshot};

        pub(super) fn read_process_context() -> io::Result<MappingHeaderSnapshot> {
            let mapping_addr = reader::linux::find_otel_mapping()?;
            let header_ptr: *const MappingHeaderSnapshot =
                ptr::with_exposed_provenance(mapping_addr);
            // SAFETY: the mapping was published by this test before being read; the tests are
            // serial and don't update the mapping while this header is copied.
            Ok(unsafe { ptr::read(header_ptr) })
        }

        pub(super) fn is_published() -> bool {
            reader::linux::find_otel_mapping().is_ok()
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use core::{ptr, sync::atomic::Ordering};
        use std::io;

        use super::super::{
            macos::{HEADER_ADDRESS_MASK, PUBLISHER_PID_SHIFT},
            writer::macos::otel_process_ctx_v2,
            MappingHeaderSnapshot,
        };

        fn published_header() -> *mut u8 {
            let value = otel_process_ctx_v2.load(Ordering::Acquire);
            let publisher_pid = (value >> PUBLISHER_PID_SHIFT) as u32;
            if publisher_pid != std::process::id() {
                return ptr::null_mut();
            }

            let header_address = (value & HEADER_ADDRESS_MASK) as usize;
            ptr::with_exposed_provenance_mut(header_address)
        }

        pub(super) fn read_process_context() -> io::Result<MappingHeaderSnapshot> {
            let header_ptr: *const MappingHeaderSnapshot = published_header().cast();
            if header_ptr.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no process context is published",
                ));
            }
            Ok(unsafe { ptr::read(header_ptr) })
        }

        pub(super) fn is_published() -> bool {
            !published_header().is_null()
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use core::{ptr, sync::atomic::Ordering};
        use std::io;

        use super::super::{writer::windows::otel_process_ctx_v2, MappingHeaderSnapshot};

        pub(super) fn read_process_context() -> io::Result<MappingHeaderSnapshot> {
            let header_ptr: *const MappingHeaderSnapshot =
                otel_process_ctx_v2.load(Ordering::Acquire).cast();
            if header_ptr.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no process context is published",
                ));
            }
            Ok(unsafe { ptr::read(header_ptr) })
        }

        pub(super) fn is_published() -> bool {
            !otel_process_ctx_v2.load(Ordering::Acquire).is_null()
        }
    }

    #[cfg(target_os = "linux")]
    use linux::{is_published, read_process_context};
    #[cfg(target_os = "macos")]
    use macos::{is_published, read_process_context};
    #[cfg(target_os = "windows")]
    use windows::{is_published, read_process_context};

    #[test]
    #[cfg_attr(miri, ignore)]
    fn publish_then_read_process_context() {
        let context = ProcessContext {
            resource: None,
            extra_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("checkout".to_string())),
                }),
                key_ref: 0,
            }],
        };

        super::publish(&context).expect("couldn't publish the process context");
        let header = read_process_context().expect("couldn't read back the process context");
        // SAFETY: the published context must have put valid bytes of size payload_size in the
        // context if the signature check succeded.
        let read_payload = unsafe {
            core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
        };
        let read_context =
            ProcessContext::decode(read_payload).expect("couldn't decode the process context");
        super::unpublish().expect("couldn't unpublish the context");

        assert!(header.signature == *super::SIGNATURE, "wrong signature");
        assert!(
            header.version == super::PROCESS_CTX_VERSION,
            "wrong context version"
        );
        assert!(
            header.monotonic_published_at_ns > 0,
            "monotonic_published_at_ns is zero"
        );
        assert!(read_context == context, "read back a different context");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn publish_then_update_process_context() {
        let payload_v1 = "example process context payload";
        let payload_v2 = "another example process context payload of different size";

        super::writer::publish_raw_payload(payload_v1.as_bytes().to_vec())
            .expect("couldn't publish the process context");

        let header = read_process_context().expect("couldn't read back the process context");
        // SAFETY: the published context must have put valid bytes of size payload_size in the
        // context if the signature check succeded.
        let read_payload = unsafe {
            core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
        };

        assert!(header.signature == *super::SIGNATURE, "wrong signature");
        assert!(
            header.version == super::PROCESS_CTX_VERSION,
            "wrong context version"
        );
        assert!(
            header.payload_size == payload_v1.len() as u32,
            "wrong payload size"
        );
        assert!(
            header.monotonic_published_at_ns > 0,
            "monotonic_published_at_ns is zero"
        );
        assert!(read_payload == payload_v1.as_bytes(), "payload mismatch");

        let published_at_ns_v1 = header.monotonic_published_at_ns;
        // Ensure the clock advances so the updated timestamp is strictly greater
        std::thread::sleep(Duration::from_nanos(10));

        super::writer::publish_raw_payload(payload_v2.as_bytes().to_vec())
            .expect("couldn't update the process context");

        let header = read_process_context().expect("couldn't read back the process context");
        // SAFETY: the published context must have put valid bytes of size payload_size in the
        // context if the signature check succeded.
        let read_payload = unsafe {
            core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
        };

        assert!(header.signature == *super::SIGNATURE, "wrong signature");
        assert!(
            header.version == super::PROCESS_CTX_VERSION,
            "wrong context version"
        );
        assert!(
            header.payload_size == payload_v2.len() as u32,
            "wrong payload size"
        );
        assert!(
            header.monotonic_published_at_ns > published_at_ns_v1,
            "published_at_ns should be strictly greater after update"
        );
        assert!(read_payload == payload_v2.as_bytes(), "payload mismatch");

        super::unpublish().expect("couldn't unpublish the context");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn unpublish_process_context() {
        let payload = "example process context payload";

        super::writer::publish_raw_payload(payload.as_bytes().to_vec())
            .expect("couldn't publish the process context");

        assert!(
            is_published(),
            "process context should be visible after publishing"
        );

        super::unpublish().expect("couldn't unpublish the context");

        assert!(
            !is_published(),
            "process context should not be visible after unpublishing"
        );
    }

    /// The only end-to-end test with `ProcessContextSelfReader`
    #[test]
    #[cfg_attr(miri, ignore)]
    fn publish_read_update_read_and_unpublish() {
        fn context(service_name: &str) -> ProcessContext {
            ProcessContext {
                resource: None,
                extra_attributes: vec![KeyValue {
                    key: "service.name".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(service_name.to_string())),
                    }),
                    key_ref: 0,
                }],
            }
        }

        let first = context("checkout");
        super::publish(&first).expect("publication should succeed");

        let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
        assert_eq!(reader.read().expect("read should succeed"), first);

        let second = context("payments");
        super::publish(&second).expect("context update should succeed");
        assert_eq!(reader.read().expect("updated read should succeed"), second);

        super::unpublish().expect("unpublish should succeed");
        assert!(reader.read().is_err());
        assert!(ProcessContextSelfReader::new().is_err());
    }
}
