// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::change_buffer::utils::*;
use crate::change_buffer::{ChangeBufferError, Result};
use std::ptr::NonNull;

/// A handle to a fixed-size change buffer shared with another runtime. The memory is shared and
/// owned/managed by the external runtime. The size of the buffer must not change once instantiated.
pub struct ChangeBuffer {
    ptr: NonNull<u8>,
    len: usize,
}

impl ChangeBuffer {
    /// # Safety
    ///
    /// The underlying raw memory must be valid for reads and writes.
    ///
    /// The underlying raw memory must not be freed until after this struct's
    /// lifetime. Having the calling code manage the memory makes it simpler to
    /// integrate with managed runtimes.
    pub unsafe fn from_raw_parts(ptr: NonNull<u8>, len: usize) -> Self {
        Self { ptr, len }
    }

    /// # Safety
    ///
    /// Same safety conditions as [std::slice::from_raw_parts].
    unsafe fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// # Safety
    ///
    /// Same safety conditions as [std::slice::from_raw_parts_mut].
    unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Read a value of type `T` starting at offset `index`.
    pub fn read<T: FromBytes>(&self, index: &mut usize) -> Result<T> {
        let size = std::mem::size_of::<T>();
        // Safety: the allocation of `self.ptr` is required to be valid for read and writes at
        // construction time, and to remain alive for the lifetime of `self`. We do not materialize
        // other references during the lifetime of `slice`.
        let slice = unsafe { self.as_slice() };
        let out_of_bounds_err = ChangeBufferError::ReadOutOfBounds {
            offset: *index,
            value_len: size,
            buffer_len: self.len,
        };
        let Some(end) = index.checked_add(size) else {
            return Err(out_of_bounds_err);
        };

        let bytes = slice.get(*index..end).ok_or(out_of_bounds_err)?;
        *index += size;
        Ok(T::from_bytes(bytes))
    }

    /// Write a raw `u32` in the buffer.
    pub fn write_u32(&mut self, offset: usize, value: u32) -> Result<()> {
        let len = self.len;
        // Safety: the allocation of `self.ptr` is guaranteed to be valid for read and writes at
        // construction time. We do not materialize other references during the lifetime of `slice`.
        let slice = unsafe { self.as_mut_slice() };
        let out_of_bounds_err = || ChangeBufferError::WriteOutOfBounds {
            offset,
            value_len: offset + 4,
            buffer_len: len,
        };
        let Some(end) = offset.checked_add(4) else {
            return Err(out_of_bounds_err());
        };

        let target = slice.get_mut(offset..end).ok_or_else(out_of_bounds_err)?;
        let bytes = value.to_le_bytes();
        target.copy_from_slice(&bytes);
        Ok(())
    }

    pub(crate) fn read_arg<T: Clone>(
        &self,
        string_table: &super::StringTable<T>,
        index: &mut usize,
    ) -> Result<T> {
        let num: u32 = self.read(index)?;
        string_table
            .get(num)
            .ok_or(ChangeBufferError::StringNotFound(num))
    }

    /// Clear the op count, which is stored in the first 4 bytes of the buffer. This effectively
    /// reset the buffer (semantically), but without actually zeroing the rest.
    pub fn clear_count(&mut self) -> Result<()> {
        self.write_u32(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::ChangeBuffer;
    use super::NonNull;
    use crate::change_buffer::Result;

    #[test]
    fn buffer_creation_and_slices() {
        let mut buf = unsafe {
            let mut buf: [u8; 256] = std::mem::zeroed();
            ChangeBuffer::from_raw_parts(NonNull::new(buf.as_mut_ptr()).unwrap(), 256)
        };
        {
            // Safety: slice is the only reference to the buffer in its scope.
            let slice = unsafe { buf.as_mut_slice() };
            assert_eq!(256, slice.len());
            slice[1] = 42;
        }
        // Safety: slice is the only reference to the buffer in its scope.
        let slice = unsafe { buf.as_slice() };
        assert_eq!(256, slice.len());
        assert_eq!(42, slice[1]);
    }

    #[test]
    fn read_and_write() -> Result<()> {
        let example =
            b"This is an example string, long enough to get 16 bytes out of it, without issue.";
        let mut ex_buf = example.to_vec();
        let mut buf = unsafe {
            ChangeBuffer::from_raw_parts(NonNull::new(ex_buf.as_mut_ptr()).unwrap(), ex_buf.len())
        };
        let mut index = 8;
        assert_eq!(8101238474429984353, buf.read::<u64>(&mut index)?);
        assert_eq!(7956016061199967596, buf.read::<u64>(&mut index)?);
        index = 8;
        buf.write_u32(index, 8675309)?;
        index = 8;
        assert_eq!(8675309, buf.read::<u32>(&mut index)?);

        Ok(())
    }

    #[test]
    fn clear_count() -> Result<()> {
        let mut buffer = vec![0xFFu8; 64];
        let mut buf = unsafe {
            ChangeBuffer::from_raw_parts(NonNull::new(buffer.as_mut_ptr()).unwrap(), buffer.len())
        };
        buf.clear_count()?;
        let mut index = 0;
        assert_eq!(0, buf.read::<u32>(&mut index)?);
        Ok(())
    }

    #[test]
    fn read_different_types() -> Result<()> {
        let mut buffer = vec![0u8; 64];
        buffer[0..4].copy_from_slice(&42u32.to_le_bytes());
        buffer[8..24].copy_from_slice(&123456789u128.to_le_bytes());
        buffer[24..32].copy_from_slice(&1.5f64.to_le_bytes());

        let buf = unsafe {
            ChangeBuffer::from_raw_parts(NonNull::new(buffer.as_mut_ptr()).unwrap(), buffer.len())
        };

        let mut index = 0;
        assert_eq!(42, buf.read::<u32>(&mut index)?);
        assert_eq!(4, index);

        index = 8;
        assert_eq!(123456789u128, buf.read::<u128>(&mut index)?);

        index = 24;
        assert_eq!(1.5, buf.read::<f64>(&mut index)?);
        Ok(())
    }

    #[test]
    fn read_out_of_bounds() {
        let mut buffer = vec![0u8; 8];
        let buf = unsafe {
            ChangeBuffer::from_raw_parts(NonNull::new(buffer.as_mut_ptr()).unwrap(), buffer.len())
        };
        let mut index = 4;
        assert!(buf.read::<u64>(&mut index).is_err());
    }

    #[test]
    fn write_out_of_bounds() {
        let mut buffer = vec![0u8; 8];
        let mut buf = unsafe {
            ChangeBuffer::from_raw_parts(NonNull::new(buffer.as_mut_ptr()).unwrap(), buffer.len())
        };
        // 8-byte buffer, u32 at offset 5 needs bytes 5..9 — out of bounds
        assert!(buf.write_u32(5, 123).is_err());
    }
}
