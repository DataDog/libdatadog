use crate::change_buffer::{ChangeBuffer, ChangeBufferError, Result};

#[repr(u8)]
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
    /// Combined create + name + start. Avoids 3 separate ops per span.
    CreateSpan = 13,
    /// Combined create + name + service + resource + type + start.
    CreateSpanFull = 14,
    /// Batch N meta (string→string) tags for one span.
    BatchSetMeta = 15,
    /// Batch N metric (string→f64) tags for one span.
    BatchSetMetric = 16,
    // TODO: SpanLinks, SpanEvents, StructAttr
}

impl TryFrom<u8> for OpCode {
    type Error = ChangeBufferError;

    fn try_from(val: u8) -> Result<Self> {
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
            13 => Ok(OpCode::CreateSpan),
            14 => Ok(OpCode::CreateSpanFull),
            15 => Ok(OpCode::BatchSetMeta),
            16 => Ok(OpCode::BatchSetMetric),
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
        let opcode_u8: u8 = buf.read(index)?;
        let opcode = opcode_u8.try_into()?;
        let span_id = buf.read(index)?;
        Ok(BufferedOperation { opcode, span_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_buffer::Result;

    fn change_buffer_from_vec(buffer: &mut Vec<u8>) -> ChangeBuffer {
        unsafe { ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len()) }
    }

    #[test]
    fn opcode_try_from_valid_values() -> Result<()> {
        let expected = [
            (0u8, "Create"),
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
            (13, "CreateSpan"),
            (14, "CreateSpanFull"),
            (15, "BatchSetMeta"),
            (16, "BatchSetMetric"),
        ];

        for (val, name) in expected {
            let opcode = OpCode::try_from(val)?;
            assert_eq!(opcode.clone() as u8, val);
            assert_eq!(name, format!("{:?}", opcode))
        }

        Ok(())
    }

    #[test]
    fn opcode_try_from_invalid_value() {
        assert!(OpCode::try_from(17u8).is_err());
        assert!(OpCode::try_from(100u8).is_err());
        assert!(OpCode::try_from(u8::MAX).is_err());
    }

    #[test]
    fn buffered_operation_from_buf() -> Result<()> {
        // Layout: opcode (u8) + span_id (u64 LE)
        let opcode: u8 = 3; // SetServiceName
        let span_id: u64 = 0xDEADBEEF;

        let mut buffer = vec![0u8; 9];
        buffer[0] = opcode;
        buffer[1..9].copy_from_slice(&span_id.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        let op = BufferedOperation::from_buf(&buf, &mut index)?;

        assert_eq!(op.opcode as u8, 3);
        assert_eq!(op.span_id, 0xDEADBEEF);
        assert_eq!(index, 9);
        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_advances_index() -> Result<()> {
        // Two operations packed sequentially, starting after a u32 count header
        // Each op: opcode(u8=1B) + span_id(u64=8B) = 9B; two ops = 18B; + 4B header = 22B total
        let mut buffer = vec![0u8; 22];
        // count header (u32)
        buffer[0..4].copy_from_slice(&0u32.to_le_bytes());
        // first op at offset 4: opcode(u8) + span_id(u64)
        buffer[4] = OpCode::Create as u8;
        buffer[5..13].copy_from_slice(&1u64.to_le_bytes());
        // second op at offset 13: opcode(u8) + span_id(u64)
        buffer[13] = OpCode::SetError as u8;
        buffer[14..22].copy_from_slice(&2u64.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);

        let mut index = 4;
        let op1 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op1.opcode as u8, OpCode::Create as u8);
        assert_eq!(op1.span_id, 1);
        assert_eq!(index, 13);

        let op2 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op2.opcode as u8, OpCode::SetError as u8);
        assert_eq!(op2.span_id, 2);
        assert_eq!(index, 22);

        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_invalid_opcode() {
        // 17 is the first invalid opcode value
        let mut buffer = vec![0u8; 9];
        buffer[0] = 17u8;
        buffer[1..9].copy_from_slice(&1u64.to_le_bytes());

        let buf = change_buffer_from_vec(&mut buffer);
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }

    #[test]
    fn buffered_operation_from_buf_too_small() {
        // Only 1 byte — enough for the opcode but not the span_id
        let mut buffer = vec![0u8; 1];
        buffer[0] = 0u8;

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
