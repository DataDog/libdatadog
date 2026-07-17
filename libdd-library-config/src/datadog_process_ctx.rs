// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Unified process-context API for libdatadog consumers.
//!
//! Linux uses the discovery mechanism from the OTel process-context specification. macOS and
//! Windows use a Datadog-specific same-process convention based on the exported
//! `datadog_process_ctx_v1` symbol. All platforms share the OTel header layout and update/read
//! protocol.

#[cfg(all(
    feature = "process-context-writer",
    any(target_os = "macos", target_os = "windows")
))]
use std::io;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// For Linux, alias from otel_process_ctx
#[cfg(all(target_os = "linux", feature = "process-context-reader"))]
pub use crate::otel_process_ctx::ProcessContextSelfReader;
#[cfg(all(target_os = "linux", feature = "process-context-writer"))]
pub use crate::otel_process_ctx::{publish, unpublish};

#[cfg(all(
    feature = "process-context-reader",
    any(target_os = "macos", target_os = "windows")
))]
/// Reader for the current process's Datadog process context.
pub type ProcessContextSelfReader =
    crate::otel_process_ctx::reader::ProcessContextReader<PlatformReaderBackend>;

#[cfg(all(feature = "process-context-reader", target_os = "macos"))]
pub(crate) type PlatformReaderBackend = macos::reader::MacosReaderBackend;
#[cfg(all(feature = "process-context-reader", target_os = "windows"))]
pub(crate) type PlatformReaderBackend = windows::reader::WindowsReaderBackend;

#[cfg(all(feature = "process-context-writer", target_os = "macos"))]
type PlatformWriterBackend = macos::writer::MacosWriterBackend;
#[cfg(all(feature = "process-context-writer", target_os = "windows"))]
type PlatformWriterBackend = windows::writer::WindowsWriterBackend;

#[cfg(all(
    feature = "process-context-writer",
    any(target_os = "macos", target_os = "windows")
))]
static PROCESS_CONTEXT_WRITER: crate::otel_process_ctx::writer::ProcessContextWriter<
    PlatformWriterBackend,
> = crate::otel_process_ctx::writer::ProcessContextWriter::new();

#[cfg(all(
    feature = "process-context-writer",
    any(target_os = "macos", target_os = "windows")
))]
/// Publishes or updates the process context through the Datadog in-process convention.
pub fn publish(
    context: &libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext,
) -> io::Result<()> {
    PROCESS_CONTEXT_WRITER.publish(context)
}

#[cfg(all(
    feature = "process-context-writer",
    any(target_os = "macos", target_os = "windows")
))]
/// Removes the process-context publication and releases its header allocation.
pub fn unpublish() -> io::Result<()> {
    PROCESS_CONTEXT_WRITER.unpublish()
}

#[cfg(all(
    test,
    feature = "process-context-reader",
    feature = "process-context-writer"
))]
#[serial_test::serial]
mod tests {
    use core::time::Duration;
    #[cfg(unix)]
    use std::io;

    use super::ProcessContextSelfReader;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
        any_value, AnyValue, KeyValue, ProcessContext,
    };
    use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;
    use prost::Message;

    use crate::otel_process_ctx::{reader::MappingHeaderSnapshot, PROCESS_CTX_VERSION, SIGNATURE};

    fn publish_raw_payload(payload: Vec<u8>) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            crate::otel_process_ctx::tests::publish_raw_payload(payload)
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            super::PROCESS_CONTEXT_WRITER.publish_raw_payload(payload)
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use core::ptr;
        use std::io;

        use super::MappingHeaderSnapshot;
        use crate::otel_process_ctx::reader;

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

        use super::{
            super::macos::{
                writer::datadog_process_ctx_v1, HEADER_ADDRESS_MASK, PUBLISHER_PID_SHIFT,
            },
            MappingHeaderSnapshot,
        };

        fn published_header() -> *mut u8 {
            let value = datadog_process_ctx_v1.load(Ordering::Acquire);
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

        use super::{super::windows::writer::datadog_process_ctx_v1, MappingHeaderSnapshot};

        pub(super) fn read_process_context() -> io::Result<MappingHeaderSnapshot> {
            let header_ptr: *const MappingHeaderSnapshot =
                datadog_process_ctx_v1.load(Ordering::Acquire).cast();
            if header_ptr.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no process context is published",
                ));
            }
            Ok(unsafe { ptr::read(header_ptr) })
        }

        pub(super) fn is_published() -> bool {
            !datadog_process_ctx_v1.load(Ordering::Acquire).is_null()
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

        assert!(header.signature == *SIGNATURE, "wrong signature");
        assert!(
            header.version == PROCESS_CTX_VERSION,
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

        publish_raw_payload(payload_v1.as_bytes().to_vec())
            .expect("couldn't publish the process context");

        let header = read_process_context().expect("couldn't read back the process context");
        // SAFETY: the published context must have put valid bytes of size payload_size in the
        // context if the signature check succeded.
        let read_payload = unsafe {
            core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
        };

        assert!(header.signature == *SIGNATURE, "wrong signature");
        assert!(
            header.version == PROCESS_CTX_VERSION,
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

        publish_raw_payload(payload_v2.as_bytes().to_vec())
            .expect("couldn't update the process context");

        let header = read_process_context().expect("couldn't read back the process context");
        // SAFETY: the published context must have put valid bytes of size payload_size in the
        // context if the signature check succeded.
        let read_payload = unsafe {
            core::slice::from_raw_parts(header.payload_ptr, header.payload_size as usize)
        };

        assert!(header.signature == *SIGNATURE, "wrong signature");
        assert!(
            header.version == PROCESS_CTX_VERSION,
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

        publish_raw_payload(payload.as_bytes().to_vec())
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
    #[cfg(unix)]
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
        let wait_result = crate::otel_process_ctx::retry_on_eintr(|| {
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
