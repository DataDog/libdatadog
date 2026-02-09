use crate::change_buffer::{ChangeBuffer, ChangeBufferError, Result};

#[repr(u64)]
#[derive(Debug, Clone)]
pub enum OpCode {
    Create = 0,
    SetMetaAttr = 1,
    SetMetricAttr = 2,
    SetServiceName = 3,
    SetResourceName = 4,
    SetError = 5,
    SetStart = 6,
    SetDuration = 7,
    SetType = 8,
    SetName = 9,
    SetTraceMetaAttr = 10,
    SetTraceMetricsAttr = 11,
    SetTraceOrigin = 12,
    // TODO: SpanLinks, SpanEvents, StructAttr
}

impl TryFrom<u64> for OpCode {
    type Error = ChangeBufferError;

    fn try_from(val: u64) -> Result<Self> {
        match val {
            0 => Ok(OpCode::Create),
            1 => Ok(OpCode::SetMetaAttr),
            2 => Ok(OpCode::SetMetricAttr),
            3 => Ok(OpCode::SetServiceName),
            4 => Ok(OpCode::SetResourceName),
            5 => Ok(OpCode::SetError),
            6 => Ok(OpCode::SetStart),
            7 => Ok(OpCode::SetDuration),
            8 => Ok(OpCode::SetType),
            9 => Ok(OpCode::SetName),
            10 => Ok(OpCode::SetTraceMetaAttr),
            11 => Ok(OpCode::SetTraceMetricsAttr),
            12 => Ok(OpCode::SetTraceOrigin),
            _ => Err(ChangeBufferError::UnknownOpcode(val)),
        }
    }
}

pub struct BufferedOperation {
    pub opcode: OpCode,
    pub span_id: u64,
}

impl BufferedOperation {
    pub fn from_buf(buf: &ChangeBuffer, index: &mut usize) -> Result<Self> {
        let opcode = buf.read::<u64>(index)?.try_into()?;
        let span_id = buf.read(index)?;
        Ok(BufferedOperation {
            opcode,
            span_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_buffer::Result;

    fn change_buffer_from_vec(buffer: &mut Vec<u8>) -> ChangeBuffer {
        unsafe {
            ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len())
        }
    }

    #[test]
    fn opcode_try_from_valid_values() -> Result<()>{
        let expected = [
            (0, "Create"),
            (1, "SetMetaAttr"),
            (2, "SetMetricAttr"),
            (3, "SetServiceName"),
            (4, "SetResourceName"),
            (5, "SetError"),
            (6, "SetStart"),
            (7, "SetDuration"),
            (8, "SetType"),
            (9, "SetName"),
            (10, "SetTraceMetaAttr"),
            (11, "SetTraceMetricsAttr"),
            (12, "SetTraceOrigin"),
        ];

        for (val, name) in expected {
            let opcode = OpCode::try_from(val)?;
            assert_eq!(opcode.clone() as u64, val);
            assert_eq!(name, format!("{:?}", opcode))
        }

        Ok(())
    }

    #[test]
    fn opcode_try_from_invalid_value() {
        assert!(OpCode::try_from(13).is_err());
        assert!(OpCode::try_from(100).is_err());
        assert!(OpCode::try_from(u64::MAX).is_err());
    }

    #[test]
    fn buffered_operation_from_buf() -> Result<()> {
        // Layout: opcode (u64 LE) + span_id (u64 LE)
        let opcode: u64 = 3; // SetServiceName
        let span_id: u64 = 0xDEADBEEF;

        let mut buffer = vec![0u8; 16];
        buffer[0..8].copy_from_slice(&opcode.to_le_bytes());
        buffer[8..16].copy_from_slice(&span_id.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        let op = BufferedOperation::from_buf(&buf, &mut index)?;

        assert_eq!(op.opcode as u64, 3);
        assert_eq!(op.span_id, 0xDEADBEEF);
        assert_eq!(index, 16);
        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_advances_index() -> Result<()> {
        // Two operations packed sequentially, starting at a non-zero offset
        let mut buffer = vec![0u8; 40];
        // padding
        buffer[0..8].copy_from_slice(&0u64.to_le_bytes());
        // first op at offset 8
        buffer[8..16].copy_from_slice(&(OpCode::Create as u64).to_le_bytes());
        buffer[16..24].copy_from_slice(&1u64.to_le_bytes());
        // second op at offset 24
        buffer[24..32].copy_from_slice(&(OpCode::SetError as u64).to_le_bytes());
        buffer[32..40].copy_from_slice(&2u64.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);

        let mut index = 8;
        let op1 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op1.opcode as u64, OpCode::Create as u64);
        assert_eq!(op1.span_id, 1);
        assert_eq!(index, 24);

        let op2 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op2.opcode as u64, OpCode::SetError as u64);
        assert_eq!(op2.span_id, 2);
        assert_eq!(index, 40);

        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_invalid_opcode() {
        let mut buffer = vec![0u8; 16];
        buffer[0..8].copy_from_slice(&999u64.to_le_bytes());
        buffer[8..16].copy_from_slice(&1u64.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }

    #[test]
    fn buffered_operation_from_buf_too_small() {
        // Only 8 bytes — enough for the opcode but not the span_id
        let mut buffer = vec![0u8; 8];
        buffer[0..8].copy_from_slice(&0u64.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }

    #[test]
    fn buffered_operation_from_buf_empty() {
        let mut buffer = vec![0u8; 0];
        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }
}
