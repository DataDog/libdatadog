// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Same-process reader for the OTel process context.
//!
//! Although it reads the current process, the reader does not coordinate with
//! the publisher: it neither acquires the publisher's lock nor retains a guard
//! that keeps the published mapping alive. It therefore faces the same mapping
//! lifetime and concurrent-update races as an external observer, while using a
//! platform-specific same-process memory-copy mechanism.
//!
//! Publication timestamps and fences can detect concurrent changes, but they
//! cannot keep the underlying memory mapped. Memory is therefore copied through
//! a [`ProcessMemoryCopy`] implementation that turns inaccessible or concurrently
//! unmapped memory into a recoverable error instead of dereferencing an invalid
//! pointer in Rust.

use core::{
    cell::Cell,
    mem::{offset_of, size_of},
    ptr::{self, NonNull},
    sync::atomic::{fence, Ordering},
};
use std::io;

use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value, AnyValue, KeyValue, ProcessContext,
};
use prost::Message;

use super::{MappingHeaderSnapshot, PROCESS_CTX_VERSION, SIGNATURE, UNPUBLISHED_OR_UPDATING};

#[cfg(target_os = "linux")]
mod copy_pipe_unix;
#[cfg(target_os = "linux")]
pub(super) mod linux;

#[cfg(target_os = "linux")]
use copy_pipe_unix::CopyPipe as PlatformCopyPipe;

#[cfg(target_os = "linux")]
type PlatformHeaderDiscovery = linux::HeaderDiscovery;

pub(super) trait ReaderPlatform {
    fn discover_header() -> io::Result<NonNull<u8>>;
}

pub(super) trait ProcessMemoryCopy: Sized + Send {
    fn new() -> io::Result<Self>;

    /// Copies exactly `len` bytes starting at `addr`.
    ///
    /// The caller is not required to establish that the source range is
    /// aligned, initialized, mapped, or remains mapped for the duration of the
    /// copy. Implementations must not dereference `addr` directly in Rust.
    ///
    /// If any part of the source range is inaccessible, the implementation must
    /// return an error with [`io::ErrorKind::WouldBlock`], rather than causing
    /// undefined behavior or terminating the process.
    ///
    /// On success, the returned vector contains exactly `len` bytes. The copy is
    /// not an atomic snapshot: the source memory may change while it is copied.
    ///
    /// The returned copier is present when its internal state remains reusable,
    /// regardless of whether the copy succeeded.
    fn copy(self, addr: *const u8, len: usize) -> (io::Result<Vec<u8>>, Option<Self>);
}

/// Reader for the current process's OTel process context.
///
/// The platform implementation locates the header at construction. Reads use a kernel-mediated
/// copy so unpublish and payload replacement races return errors instead of dereferencing freed
/// memory.
pub struct ProcessContextSelfReader {
    pid: u32,
    pub(in crate::otel_process_ctx) header_ptr: NonNull<u8>,
    pipe: Cell<Option<PlatformCopyPipe>>,
}

// SAFETY: ProcessMemoryCopy requires the cached platform pipe to be Send. Moving header_ptr between
// threads is safe because it is a process-global address that is never dereferenced directly; all
// access goes through ProcessMemoryCopy. Cell keeps the type !Sync, so the cached pipe cannot be
// used concurrently through shared references.
unsafe impl Send for ProcessContextSelfReader {}

impl ProcessContextSelfReader {
    /// Locates the current process's published OTel process context header.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            pid: std::process::id(),
            header_ptr: <PlatformHeaderDiscovery as ReaderPlatform>::discover_header()?,
            pipe: Cell::new(None),
        })
    }

    /// Reads and decodes the current process's OTel process context.
    ///
    /// Returns [`io::ErrorKind::WouldBlock`] if a writer is publishing or updating the context,
    /// or if the context changed while it was being read. Callers may retry later.
    pub fn read(&self) -> io::Result<ProcessContext> {
        if self.pid != std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context reader is stale after fork; construct a new reader",
            ));
        }

        let published_at = self.read_published_at()?;
        if published_at == UNPUBLISHED_OR_UPDATING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context is currently being updated",
            ));
        }

        fence(Ordering::SeqCst);

        let header = self.read_header()?;
        if header.signature != *SIGNATURE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid signature in process context header",
            ));
        }
        if header.version != PROCESS_CTX_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported process context version {}", header.version),
            ));
        }
        if header.payload_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context payload pointer is null",
            ));
        }

        let payload = self.read_process_memory(header.payload_ptr, header.payload_size as usize)?;

        fence(Ordering::SeqCst);
        if self.read_published_at()? != published_at {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context changed while being read",
            ));
        }

        ProcessContext::decode(payload.as_slice())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    /// Returns the thread-local attribute key map from a decoded process context.
    pub fn threadlocal_attribute_key_map(context: &ProcessContext) -> Option<Vec<String>> {
        Self::find_attr(&context.extra_attributes, "threadlocal.attribute_key_map")
            .and_then(Self::string_array)
    }

    fn string_array(value: &AnyValue) -> Option<Vec<String>> {
        let any_value::Value::ArrayValue(array) = value.value.as_ref()? else {
            return None;
        };

        array
            .values
            .iter()
            .map(|value| match value.value.as_ref()? {
                any_value::Value::StringValue(value) => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
        attrs
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| attribute.value.as_ref())
    }

    fn read_published_at(&self) -> io::Result<u64> {
        let timestamp_ptr = self
            .header_ptr
            .as_ptr()
            .wrapping_add(offset_of!(MappingHeaderSnapshot, monotonic_published_at_ns))
            .cast_const();
        let bytes = self.read_process_memory(timestamp_ptr, size_of::<u64>())?;
        Ok(u64::from_ne_bytes(
            Self::field_bytes::<{ size_of::<u64>() }>(&bytes, 0, "monotonic_published_at_ns")?,
        ))
    }

    fn read_header(&self) -> io::Result<MappingHeaderSnapshot> {
        let bytes = self.read_process_memory(
            self.header_ptr.as_ptr().cast_const(),
            size_of::<MappingHeaderSnapshot>(),
        )?;
        Self::mapping_header_from_bytes(&bytes)
    }

    fn mapping_header_from_bytes(bytes: &[u8]) -> io::Result<MappingHeaderSnapshot> {
        if bytes.len() != size_of::<MappingHeaderSnapshot>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "process context header copy had size {}, expected {}",
                    bytes.len(),
                    size_of::<MappingHeaderSnapshot>()
                ),
            ));
        }

        let signature = Self::field_bytes::<8>(
            bytes,
            offset_of!(MappingHeaderSnapshot, signature),
            "signature",
        )?;
        let version = u32::from_ne_bytes(Self::field_bytes::<{ size_of::<u32>() }>(
            bytes,
            offset_of!(MappingHeaderSnapshot, version),
            "version",
        )?);
        let payload_size = u32::from_ne_bytes(Self::field_bytes::<{ size_of::<u32>() }>(
            bytes,
            offset_of!(MappingHeaderSnapshot, payload_size),
            "payload_size",
        )?);
        let monotonic_published_at_ns =
            u64::from_ne_bytes(Self::field_bytes::<{ size_of::<u64>() }>(
                bytes,
                offset_of!(MappingHeaderSnapshot, monotonic_published_at_ns),
                "monotonic_published_at_ns",
            )?);
        let payload_addr = usize::from_ne_bytes(Self::field_bytes::<{ size_of::<usize>() }>(
            bytes,
            offset_of!(MappingHeaderSnapshot, payload_ptr),
            "payload_ptr",
        )?);

        Ok(MappingHeaderSnapshot {
            signature,
            version,
            payload_size,
            monotonic_published_at_ns,
            payload_ptr: ptr::with_exposed_provenance(payload_addr),
        })
    }

    fn field_bytes<const N: usize>(
        bytes: &[u8],
        offset: usize,
        field: &'static str,
    ) -> io::Result<[u8; N]> {
        let end = offset.checked_add(N).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("process context header field {field} offset overflowed"),
            )
        })?;
        let slice = bytes.get(offset..end).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("process context header field {field} is out of bounds"),
            )
        })?;
        let mut result = [0; N];
        result.copy_from_slice(slice);
        Ok(result)
    }

    fn read_process_memory(&self, addr: *const u8, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let pipe = match self.pipe.take() {
            Some(pipe) => pipe,
            None => PlatformCopyPipe::new()?,
        };

        let (result, pipe) = pipe.copy(addr, len);
        self.pipe.set(pipe);
        result
    }
}
