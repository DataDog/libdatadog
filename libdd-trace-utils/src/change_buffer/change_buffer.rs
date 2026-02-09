use crate::change_buffer::utils::*;
use crate::change_buffer::{ChangeBufferError, Result};

#[derive(Clone, Copy)]
pub struct ChangeBuffer {                                                                                                                                                                                                       
    ptr: *mut u8,                                                                                                                                                                                                               
    len: usize,                                                                                                                                                                                                                 
}

impl ChangeBuffer {                                                                                                                                                                                                                                                                                                                                                                      
    pub unsafe fn from_raw_parts(ptr: *const u8, len: usize) -> Self {                                                                                                                                                          
        Self { ptr: ptr as *mut u8, len }                                                                                                                                                                                       
    }                                                                                                                                                                                                                           

    fn as_slice(&self) -> &[u8] {                                                                                                                                                                                               
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }                                                                                                                                                               
    }                                                                                                                                                                                                                           

    fn as_mut_slice(&mut self) -> &mut [u8] {                                                                                                                                                                                   
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }                                                                                                                                                           
    }                                                                                                                                                                                                                           

    pub fn read<T: Copy + FromBytes>(&self, index: &mut usize) -> Result<T> {
        let size = std::mem::size_of::<T>();
        let slice = self.as_slice();
        let bytes = slice.get(*index..*index + size)
            .ok_or(ChangeBufferError::ReadOutOfBounds { offset: *index, len: self.len })?;
        *index += size;
        Ok(T::from_bytes(bytes))
    }

    pub fn write_u64(&mut self, offset: usize, value: u64) -> Result<()> {                                                                                                                                                      
        let len = self.len;
        let slice = self.as_mut_slice();                                                                                                                                                                                        
        let target = slice.get_mut(offset..offset + 8)
            .ok_or(ChangeBufferError::WriteOutOfBounds { offset, len })?;                                                                                                                                 
        let bytes = value.to_le_bytes();
        target.copy_from_slice(&bytes);                                                                                                                                                                           
        Ok(())                                                                                                                                                                                                                  
    }                                                                                                                                                                                                                           

    pub fn clear_count(&mut self) -> Result<()> {
        self.write_u64(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::ChangeBuffer;
    use crate::change_buffer::Result;

    #[test]
    fn buffer_creation_and_slices() {
        let mut buf = unsafe {
            let buf: [u8; 256] = std::mem::zeroed();
            ChangeBuffer::from_raw_parts(buf.as_ptr(), 256)
        };
        let slice = buf.as_mut_slice();
        assert_eq!(256, slice.len());
        slice[1] = 42;
        let slice = buf.as_slice();
        assert_eq!(256, slice.len());
        assert_eq!(42, slice[1]);
    }

    #[test]
    fn read_and_write() -> Result<()> {
        let example = b"This is an example string, long enough to get 16 bytes out of it, without issue.";
        let mut ex_buf = example.to_vec();
        let mut buf = unsafe {
            ChangeBuffer::from_raw_parts(ex_buf.as_mut_ptr(), ex_buf.len())
        };
        let mut index = 8;
        assert_eq!(8101238474429984353, buf.read::<u64>(&mut index)?);
        assert_eq!(7956016061199967596, buf.read::<u64>(&mut index)?);
        index = 8;
        buf.write_u64(index, 8675309)?;
        index = 8;
        assert_eq!(8675309, buf.read::<u64>(&mut index)?);

        Ok(())
    }

    #[test]
    fn clear_count() -> Result<()> {
        let mut buffer = vec![0xFFu8; 64];
        let mut buf = unsafe {
            ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len())
        };
        buf.clear_count()?;
        let mut index = 0;
        assert_eq!(0, buf.read::<u64>(&mut index)?);
        Ok(())
    }

    #[test]
    fn read_different_types() -> Result<()> {
        let mut buffer = vec![0u8; 64];
        buffer[0..4].copy_from_slice(&42u32.to_le_bytes());
        buffer[8..24].copy_from_slice(&123456789u128.to_le_bytes());
        buffer[24..32].copy_from_slice(&3.14f64.to_le_bytes());

        let buf = unsafe {
            ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len())
        };

        let mut index = 0;
        assert_eq!(42, buf.read::<u32>(&mut index)?);
        assert_eq!(4, index);

        index = 8;
        assert_eq!(123456789u128, buf.read::<u128>(&mut index)?);

        index = 24;
        assert_eq!(3.14, buf.read::<f64>(&mut index)?);
        Ok(())
    }

    #[test]
    fn read_out_of_bounds() {
        let mut buffer = vec![0u8; 8];
        let buf = unsafe { ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len()) };
        let mut index = 4;
        assert!(buf.read::<u64>(&mut index).is_err());
    }

    #[test]
    fn write_out_of_bounds() {
        let mut buffer = vec![0u8; 8];
        let mut buf = unsafe { ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len()) };
        assert!(buf.write_u64(4, 123).is_err());
    }
}
