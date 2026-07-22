// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Platform-neutral process-context operations over caller-owned storage.

use core::{
    mem::{align_of, offset_of, size_of},
    ptr::{self, NonNull},
    sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering},
};
use std::{io, sync::OnceLock, time::Instant};

use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value, AnyValue, KeyValue, ProcessContext,
};
use prost::Message;

use super::{PROCESS_CTX_VERSION, SIGNATURE, UNPUBLISHED_OR_UPDATING};

#[repr(C)]
struct MappingHeader {
    signature: [u8; 8],
    version: u32,
    payload_size: AtomicU32,
    monotonic_published_at_ns: AtomicU64,
    payload_ptr: AtomicPtr<u8>,
}

const _: () = {
    assert!(offset_of!(MappingHeader, signature) == 0);
    assert!(offset_of!(MappingHeader, version) == 8);
    assert!(offset_of!(MappingHeader, payload_size) == 12);
    assert!(offset_of!(MappingHeader, monotonic_published_at_ns) == 16);
    assert!(offset_of!(MappingHeader, payload_ptr) == 24);
    assert!(size_of::<MappingHeader>() == 32);
    assert!(align_of::<MappingHeader>() == 8);
};

/// A caller-owned memory region containing a process-context header and inline payload.
#[derive(Clone, Copy)]
pub struct ProcessContextMapping {
    base: NonNull<u8>,
    len: usize,
}

// SAFETY: this type does not own or dereference the region implicitly. Synchronization is supplied
// by the process-context protocol, and callers retain responsibility for the region lifetime.
unsafe impl Send for ProcessContextMapping {}
unsafe impl Sync for ProcessContextMapping {}

impl ProcessContextMapping {
    /// Constructs a view over caller-owned storage.
    ///
    /// # Safety
    ///
    /// `base..base + len` must remain mapped and readable for every operation on this value, and
    /// writable for initialization, update, and invalidation. The base must be 8-byte aligned.
    pub unsafe fn from_raw_parts(base: *mut u8, len: usize) -> io::Result<Self> {
        let base = NonNull::new(base).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "null process context mapping")
        })?;
        if len < size_of::<MappingHeader>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context mapping is smaller than its header",
            ));
        }
        if base.as_ptr().align_offset(align_of::<MappingHeader>()) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context mapping is not 8-byte aligned",
            ));
        }
        Ok(Self { base, len })
    }

    fn header(self) -> *mut MappingHeader {
        self.base.as_ptr().cast()
    }

    fn payload(self) -> *mut u8 {
        self.base.as_ptr().wrapping_add(size_of::<MappingHeader>())
    }

    fn capacity(self) -> usize {
        self.len - size_of::<MappingHeader>()
    }
}

pub fn initialize(mapping: ProcessContextMapping, context: &ProcessContext) -> io::Result<()> {
    let payload = context.encode_to_vec();
    let header = mapping.header();

    // Construct the typed header before using its atomic fields. The unpublished value makes the
    // mapping fail closed if validation or payload sizing fails below.
    unsafe {
        header.write(MappingHeader {
            signature: *SIGNATURE,
            version: PROCESS_CTX_VERSION,
            payload_size: AtomicU32::new(0),
            monotonic_published_at_ns: AtomicU64::new(UNPUBLISHED_OR_UPDATING),
            payload_ptr: AtomicPtr::new(mapping.payload()),
        });
    }
    let payload_size = payload_size(mapping, payload.len())?;

    // SAFETY: construction validates size and alignment; the caller guarantees writable lifetime.
    unsafe {
        copy_payload_to_mapping(mapping, &payload);
        (*header)
            .payload_size
            .store(payload_size, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        (*header)
            .monotonic_published_at_ns
            .store(next_published_at(0), Ordering::Relaxed);
    }
    Ok(())
}

pub fn update(mapping: ProcessContextMapping, context: &ProcessContext) -> io::Result<()> {
    let payload = context.encode_to_vec();
    let payload_size = match payload_size(mapping, payload.len()) {
        Ok(size) => size,
        Err(error) => {
            invalidate(mapping);
            return Err(error);
        }
    };
    let header = mapping.header();

    // SAFETY: construction validates size and alignment; the caller guarantees writable lifetime.
    let previous = unsafe {
        (*header)
            .monotonic_published_at_ns
            .swap(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed)
    };
    if previous == UNPUBLISHED_OR_UPDATING {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "process context is already being updated or invalidated",
        ));
    }

    unsafe {
        fence(Ordering::SeqCst);
        copy_payload_to_mapping(mapping, &payload);
        (*header)
            .payload_ptr
            .store(mapping.payload(), Ordering::Relaxed);
        (*header)
            .payload_size
            .store(payload_size, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        (*header)
            .monotonic_published_at_ns
            .store(next_published_at(previous), Ordering::Relaxed);
    }
    Ok(())
}

pub fn invalidate(mapping: ProcessContextMapping) {
    // SAFETY: the caller guarantees writable lifetime.
    unsafe {
        (*mapping.header())
            .monotonic_published_at_ns
            .store(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed);
    }
    fence(Ordering::SeqCst);
}

pub fn validate(mapping: ProcessContextMapping) -> io::Result<()> {
    let header = mapping.header();
    // SAFETY: the caller guarantees readable lifetime.
    let published_at = unsafe { (*header).monotonic_published_at_ns.load(Ordering::Relaxed) };
    if published_at == UNPUBLISHED_OR_UPDATING {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "process context is unavailable or being updated",
        ));
    }
    fence(Ordering::SeqCst);

    // SAFETY: the validated region contains a complete header.
    let (signature, version, size, payload) = unsafe {
        (
            ptr::addr_of!((*header).signature).read(),
            ptr::addr_of!((*header).version).read(),
            (*header).payload_size.load(Ordering::Relaxed) as usize,
            (*header).payload_ptr.load(Ordering::Relaxed),
        )
    };
    if signature != *SIGNATURE || version != PROCESS_CTX_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid process context header",
        ));
    }
    if payload != mapping.payload() || size > mapping.capacity() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process context payload is not inline or exceeds the mapping",
        ));
    }
    Ok(())
}

pub fn decode(mapping: ProcessContextMapping) -> io::Result<ProcessContext> {
    let (context, _) = read(mapping)?;
    Ok(context)
}

pub fn read(mapping: ProcessContextMapping) -> io::Result<(ProcessContext, u64)> {
    validate(mapping)?;
    let header = mapping.header();
    // SAFETY: validate checked that the inline range is in bounds.
    let (published_at, size) = unsafe {
        (
            (*header).monotonic_published_at_ns.load(Ordering::Relaxed),
            (*header).payload_size.load(Ordering::Relaxed) as usize,
        )
    };
    let mut payload = vec![0; size];
    copy_payload_from_mapping(mapping, &mut payload);
    fence(Ordering::SeqCst);
    let published_after = unsafe { (*header).monotonic_published_at_ns.load(Ordering::Relaxed) };
    if published_at == 0 || published_at != published_after {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "process context changed while being read",
        ));
    }
    let context = ProcessContext::decode(payload.as_slice())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok((context, published_at))
}

pub fn threadlocal_attribute_key_map(context: &ProcessContext) -> Option<Vec<String>> {
    find_attr(&context.extra_attributes, "threadlocal.attribute_key_map")
        .or_else(|| {
            context.resource.as_ref().and_then(|resource| {
                find_attr(&resource.attributes, "threadlocal.attribute_key_map")
            })
        })
        .and_then(string_array)
}

fn payload_size(mapping: ProcessContextMapping, len: usize) -> io::Result<u32> {
    if len > mapping.capacity() {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            format!(
                "encoded process context needs {len} bytes but mapping capacity is {}",
                mapping.capacity()
            ),
        ));
    }
    u32::try_from(len).map_err(|_| io::Error::other("process context payload size overflowed"))
}

fn next_published_at(previous: u64) -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let elapsed = ORIGIN.get_or_init(Instant::now).elapsed().as_nanos();
    let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
    elapsed.max(previous.saturating_add(1)).max(1)
}

fn copy_payload_to_mapping(mapping: ProcessContextMapping, payload: &[u8]) {
    for (offset, byte) in payload.iter().copied().enumerate() {
        // SAFETY: payload_size checked that offset is in the caller-provided mapping.
        unsafe { AtomicU8::from_ptr(mapping.payload().add(offset)).store(byte, Ordering::Relaxed) };
    }
}

fn copy_payload_from_mapping(mapping: ProcessContextMapping, payload: &mut [u8]) {
    for (offset, byte) in payload.iter_mut().enumerate() {
        // SAFETY: validate checked that offset is in the caller-provided mapping.
        *byte =
            unsafe { AtomicU8::from_ptr(mapping.payload().add(offset)).load(Ordering::Relaxed) };
    }
}

fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
    attrs
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
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

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext;

    fn storage() -> Box<[u64; 2048]> {
        Box::new([0; 2048])
    }

    fn mapping(storage: &mut [u64; 2048]) -> ProcessContextMapping {
        unsafe {
            ProcessContextMapping::from_raw_parts(storage.as_mut_ptr().cast(), size_of_val(storage))
                .expect("valid storage")
        }
    }

    #[test]
    fn caller_storage_lifecycle() {
        let mut storage = storage();
        let mapping = mapping(&mut storage);
        let first = ProcessContext::default();
        initialize(mapping, &first).expect("initialize");
        assert_eq!(decode(mapping).expect("decode"), first);
        let first_published_at = read(mapping).expect("read").1;
        update(mapping, &first).expect("update");
        assert!(read(mapping).expect("read").1 > first_published_at);
        invalidate(mapping);
        assert_eq!(
            validate(mapping).expect_err("invalidated").kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn oversized_payload_invalidates_mapping() {
        let mut words = [0u64; 8];
        let mapping = unsafe {
            ProcessContextMapping::from_raw_parts(words.as_mut_ptr().cast(), size_of_val(&words))
                .expect("valid storage")
        };
        initialize(mapping, &ProcessContext::default()).expect("initialize");
        let oversized = ProcessContext {
            extra_attributes: vec![KeyValue {
                key: "large".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("x".repeat(256))),
                }),
                key_ref: 0,
            }],
            ..Default::default()
        };
        assert_eq!(
            update(mapping, &oversized).expect_err("too large").kind(),
            io::ErrorKind::StorageFull
        );
        assert_eq!(
            validate(mapping).expect_err("invalidated").kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn update_detects_an_update_in_progress() {
        let mut storage = storage();
        let mapping = mapping(&mut storage);
        let context = ProcessContext::default();
        initialize(mapping, &context).expect("initialize");
        invalidate(mapping);

        assert_eq!(
            update(mapping, &context)
                .expect_err("concurrent update must be rejected")
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
