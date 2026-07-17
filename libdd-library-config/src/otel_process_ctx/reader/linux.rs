// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{ptr, ptr::NonNull};
use std::{
    fs::File,
    io::{self, BufRead, BufReader},
};

use super::ReaderBackend;

mod sealed {
    pub struct LinuxReaderBackend;
}

pub(crate) use sealed::LinuxReaderBackend;

impl ReaderBackend for LinuxReaderBackend {
    type MemoryCopy = super::copy_pipe_unix::CopyPipe;

    fn discover_header() -> io::Result<NonNull<u8>> {
        let address = find_otel_mapping()?;
        NonNull::new(ptr::with_exposed_provenance::<u8>(address).cast_mut()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "null process context header")
        })
    }
}

/// Finds the OTEL_CTX mapping in `/proc/self/maps`.
pub(crate) fn find_otel_mapping() -> io::Result<usize> {
    let file = File::open("/proc/self/maps")?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if is_named_otel_mapping(&line) {
            if let Some(address) = parse_mapping_start(&line) {
                return Ok(address);
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "couldn't find the mapping of the process context",
    ))
}

fn parse_mapping_start(line: &str) -> Option<usize> {
    usize::from_str_radix(line.split('-').next()?, 16).ok()
}

fn is_named_otel_mapping(line: &str) -> bool {
    let Some(name) = line.split_whitespace().nth(5) else {
        return false;
    };

    matches!(
        name,
        "/memfd:OTEL_CTX" | "[anon_shmem:OTEL_CTX]" | "[anon:OTEL_CTX]"
    )
}

#[cfg(test)]
mod tests {
    use super::is_named_otel_mapping;

    #[test]
    fn recognizes_exact_mapping_names() {
        assert!(is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX"
        ));
        assert!(is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX (deleted)"
        ));
        assert!(is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 [anon_shmem:OTEL_CTX]"
        ));
        assert!(is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 [anon:OTEL_CTX]"
        ));
        assert!(!is_named_otel_mapping(
            "7f000000-7f001000 rw-p 00000000 00:00 0 /memfd:OTEL_CTX_BACKUP"
        ));
    }

    #[cfg(feature = "process-context-writer")]
    mod with_writer {
        use core::sync::atomic::Ordering;

        use super::super::io;
        use crate::otel_process_ctx::{
            reader::{linux::LinuxReaderBackend, ProcessContextReader},
            tests::publish_raw_payload,
            unpublish,
            writer::MappingHeader,
            UNPUBLISHED_OR_UPDATING,
        };

        #[test]
        #[cfg_attr(miri, ignore)]
        #[serial_test::serial]
        fn read_returns_would_block_while_context_is_being_updated() {
            publish_raw_payload(b"published payload".to_vec()).expect("publish should succeed");
            let reader = ProcessContextReader::<LinuxReaderBackend>::new()
                .expect("reader creation should succeed");
            // SAFETY: the mapping is live and this serial test excludes concurrent publishers.
            let header = reader.header_ptr.as_ptr().cast::<MappingHeader>();
            let published_at = unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .swap(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed)
            };

            let error = reader
                .read()
                .expect_err("read should report writer in progress");
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);

            // SAFETY: the mapping remains live and this restores the field changed above.
            unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .store(published_at, Ordering::Relaxed);
            }
            unpublish().expect("unpublish should succeed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        #[serial_test::serial]
        fn discovers_and_reads_published_context() {
            publish_raw_payload(b"published payload".to_vec()).expect("publish should succeed");

            let reader = ProcessContextReader::<LinuxReaderBackend>::new()
                .expect("reader creation should succeed");
            let error = reader
                .read()
                .expect_err("raw test payload is not a protobuf context");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);

            unpublish().expect("unpublish should succeed");
            assert!(ProcessContextReader::<LinuxReaderBackend>::new().is_err());
        }
    }
}
