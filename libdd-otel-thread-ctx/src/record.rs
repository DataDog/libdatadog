// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    mem,
    sync::atomic::{compiler_fence, AtomicU8, Ordering},
};

pub const MAX_ATTRS_DATA_SIZE: usize = 612;
const ROOT_SPAN_KEY_INDEX: u8 = 0;

/// Platform-neutral in-memory layout of an OTel thread context record.
#[repr(C)]
pub struct ThreadContextRecord {
    pub(crate) trace_id: [u8; 16],
    pub(crate) span_id: [u8; 8],
    pub(crate) valid: AtomicU8,
    pub(crate) reserved: u8,
    pub(crate) attrs_data_size: u16,
    pub(crate) attrs_data: [u8; MAX_ATTRS_DATA_SIZE],
}

const _: () = {
    assert!(mem::size_of::<ThreadContextRecord>() == 640);
    assert!(mem::align_of::<ThreadContextRecord>() == 2);
    assert!(mem::offset_of!(ThreadContextRecord, trace_id) == 0);
    assert!(mem::offset_of!(ThreadContextRecord, span_id) == 16);
    assert!(mem::offset_of!(ThreadContextRecord, valid) == 24);
    assert!(mem::offset_of!(ThreadContextRecord, reserved) == 25);
    assert!(mem::offset_of!(ThreadContextRecord, attrs_data_size) == 26);
    assert!(mem::offset_of!(ThreadContextRecord, attrs_data) == 28);
};

impl ThreadContextRecord {
    pub fn initialize<I, S>(
        &mut self,
        trace_id: [u8; 16],
        span_id: [u8; 8],
        local_root_span_id: [u8; 8],
        attributes: I,
    ) -> bool
    where
        I: IntoIterator<Item = (u8, S)>,
        S: AsRef<str>,
    {
        self.update(trace_id, span_id, local_root_span_id, attributes)
    }

    pub fn update<I, S>(
        &mut self,
        trace_id: [u8; 16],
        span_id: [u8; 8],
        local_root_span_id: [u8; 8],
        attributes: I,
    ) -> bool
    where
        I: IntoIterator<Item = (u8, S)>,
        S: AsRef<str>,
    {
        self.valid.store(0, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        self.trace_id = trace_id;
        self.span_id = span_id;
        let complete = self.set_attrs(local_root_span_id, attributes);
        compiler_fence(Ordering::SeqCst);
        self.valid.store(1, Ordering::Relaxed);
        complete
    }

    pub fn update_span_id(&mut self, span_id: [u8; 8]) {
        self.valid.store(0, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        self.span_id = span_id;
        compiler_fence(Ordering::SeqCst);
        self.valid.store(1, Ordering::Relaxed);
    }

    fn set_attrs<I, S>(&mut self, local_root_span_id: [u8; 8], attributes: I) -> bool
    where
        I: IntoIterator<Item = (u8, S)>,
        S: AsRef<str>,
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.attrs_data[0] = ROOT_SPAN_KEY_INDEX;
        self.attrs_data[1] = 16;
        for (index, byte) in local_root_span_id.iter().copied().enumerate() {
            self.attrs_data[2 + index * 2] = HEX[(byte >> 4) as usize];
            self.attrs_data[3 + index * 2] = HEX[(byte & 0xf) as usize];
        }

        let mut offset = 18;
        let mut complete = true;
        for (key_index, value) in attributes {
            let bytes = value.as_ref().as_bytes();
            let len = match u8::try_from(bytes.len()) {
                Ok(len) => len,
                Err(_) => {
                    complete = false;
                    u8::MAX
                }
            };
            let end = offset + 2 + len as usize;
            if end > MAX_ATTRS_DATA_SIZE {
                complete = false;
                break;
            }
            self.attrs_data[offset] = key_index;
            self.attrs_data[offset + 1] = len;
            self.attrs_data[offset + 2..end].copy_from_slice(&bytes[..len as usize]);
            offset = end;
        }
        self.attrs_data_size = offset as u16;
        complete
    }
}

impl Default for ThreadContextRecord {
    fn default() -> Self {
        Self {
            trace_id: [0; 16],
            span_id: [0; 8],
            valid: AtomicU8::new(1),
            reserved: 0,
            attrs_data_size: 0,
            attrs_data: [0; MAX_ATTRS_DATA_SIZE],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_record_lifecycle() {
        let mut record = ThreadContextRecord::default();
        assert!(record.initialize([1; 16], [2; 8], [3; 8], [(1, "service")]));
        assert_eq!(record.valid.load(Ordering::Relaxed), 1);
        assert_eq!(record.span_id, [2; 8]);
        assert_eq!(&record.attrs_data[20..27], b"service");
        record.update_span_id([4; 8]);
        assert_eq!(record.span_id, [4; 8]);
    }
}
