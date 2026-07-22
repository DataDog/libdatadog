// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Platform-neutral mapping operations and Linux publication/discovery for the [OTEL process
//! context specification](https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md).
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

#[cfg(target_os = "linux")]
use std::io;

mod mapping;

pub use mapping::{
    decode, initialize, invalidate, read, threadlocal_attribute_key_map, update, validate,
    ProcessContextMapping,
};

#[cfg(all(target_os = "linux", feature = "process-context-reader"))]
mod reader;
#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
mod writer;
#[cfg(all(target_os = "linux", not(target_has_atomic = "64")))]
compile_error!("OTel process context requires 64-bit atomics on Linux");
#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(all(target_os = "linux", feature = "process-context-reader"))]
pub use reader::ProcessContextSelfReader;
#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
pub use writer::{publish, unpublish};

/// Current version of the process context format
pub const PROCESS_CTX_VERSION: u32 = 2;
/// Signature bytes for identifying process context mappings
pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
/// Sentinel timestamp indicating that the context is unpublished or being updated.
const UNPUBLISHED_OR_UPDATING: u64 = 0;

#[repr(C)]
#[cfg(all(target_os = "linux", feature = "process-context-reader"))]
struct MappingHeaderSnapshot {
    signature: [u8; 8],
    version: u32,
    payload_size: u32,
    monotonic_published_at_ns: u64,
    payload_ptr: *const u8,
}

/// Runs an operation until it succeeds or fails for a reason other than `EINTR`.
#[cfg(target_os = "linux")]
fn retry_on_eintr<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match operation() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[cfg(all(
    test,
    target_os = "linux",
    feature = "process-context-reader",
    feature = "process-context-writer"
))]
#[serial_test::serial]
mod tests {
    use core::time::Duration;
    #[cfg(target_os = "linux")]
    use std::io;

    use super::ProcessContextSelfReader;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
        any_value, AnyValue, KeyValue, ProcessContext,
    };
    use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;
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

    #[cfg(target_os = "linux")]
    use linux::{is_published, read_process_context};

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

    /// End-to-end test using `ProcessContextSelfReader`.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn publish_read_update_read_and_unpublish() {
        fn context(service_name: &str) -> ProcessContext {
            ProcessContext {
                resource: Some(Resource {
                    attributes: vec![
                        string_attribute("service.name", service_name),
                        string_attribute("telemetry.sdk.language", "rust"),
                    ],
                    dropped_attributes_count: 0,
                    entity_refs: vec![],
                }),
                extra_attributes: vec![string_attribute(
                    "datadog.process_tags",
                    "region:us-east-1",
                )],
            }
        }

        fn string_attribute(key: &str, value: &str) -> KeyValue {
            KeyValue {
                key: key.to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(value.to_string())),
                }),
                key_ref: 0,
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

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn child_can_republish_after_fork() {
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

        let parent_context = context("fork-parent");
        let child_context = context("fork-child");

        super::publish(&parent_context).expect("publication before fork should succeed");
        let reader = ProcessContextSelfReader::new().expect("reader creation should succeed");
        assert_eq!(
            reader
                .read()
                .expect("parent read before fork should succeed"),
            parent_context
        );

        // SAFETY: the process context lock is not held and the child immediately limits itself to
        // the operations under test before exiting with `_exit`.
        let child_pid = unsafe { libc::fork() };
        if child_pid < 0 {
            let fork_error = std::io::Error::last_os_error();
            drop(reader);
            super::unpublish().expect("cleanup after failed fork should succeed");
            panic!("fork failed: {fork_error}");
        }

        if child_pid == 0 {
            let exit_code = child_actions(&reader, &child_context);
            // SAFETY: `_exit` terminates the child without running parent-owned destructors.
            unsafe { libc::_exit(exit_code) };
        }

        fn child_actions(
            stale_reader: &ProcessContextSelfReader,
            child_context: &ProcessContext,
        ) -> libc::c_int {
            match stale_reader.read() {
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => {}
                Err(_) | Ok(_) => return 4,
            }
            match ProcessContextSelfReader::new() {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) | Ok(_) => return 4,
            }

            if super::publish(child_context).is_err() {
                return 5;
            }

            let reader = match ProcessContextSelfReader::new() {
                Ok(reader) => reader,
                Err(_) => return 6,
            };

            match reader.read() {
                Ok(read_context) if read_context == *child_context => 0,
                Ok(_) | Err(_) => 7,
            }
        }

        let mut status = 0;
        let wait_result = super::retry_on_eintr(|| {
            // SAFETY: child_pid identifies the child created above, and status is writable.
            let waited_pid = unsafe { libc::waitpid(child_pid, &mut status, 0) };
            if waited_pid < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(waited_pid)
            }
        });

        assert_eq!(
            reader
                .read()
                .expect("parent read after fork should succeed"),
            parent_context
        );

        drop(reader);
        super::unpublish().expect("parent cleanup should succeed");

        assert_eq!(
            wait_result.expect("waiting for child should succeed"),
            child_pid,
            "waitpid returned a different child"
        );
        assert!(
            libc::WIFEXITED(status),
            "child did not exit normally: status={status}"
        );
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "child process failed at step {}",
            libc::WEXITSTATUS(status)
        );
    }
}
