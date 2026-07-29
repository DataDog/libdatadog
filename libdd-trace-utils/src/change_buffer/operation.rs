// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Change buffer operations.
//!
//! Operations are encoded in the change buffer, and provides an API on spans and their parts.
//! Instead of calling this API as normal functions, the operations are batched in the change
//! buffer, similarly to a bytecode.
use super::{ChangeBuffer, ChangeBufferError, Result};

#[repr(u16)]
#[derive(Debug, Clone)]
/// The code of an operation that can be encoded in [ChangeBuffer].
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

impl TryFrom<u16> for OpCode {
    type Error = ChangeBufferError;

    fn try_from(val: u16) -> Result<Self> {
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
            _ => Err(ChangeBufferError::UnknownOpcode(val as u32)),
        }
    }
}

pub struct BufferedOperation {
    pub opcode: OpCode,
    /// The span this operation targets, identified by its span_id.
    /// For Create operations this is the span being created; for all other
    /// operations it identifies the already-live span to modify.
    pub span_id: u64,
}

impl BufferedOperation {
    pub fn from_buf(buf: &ChangeBuffer, index: &mut usize) -> Result<Self> {
        // Layout: opcode (u16 LE) + span_id (u64 LE) = 10 bytes per op header.
        let opcode: u16 = buf.read(index)?;
        let opcode = opcode.try_into()?;
        let span_id: u64 = buf.read(index)?;
        Ok(BufferedOperation { opcode, span_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_buffer::Result;

    /// # Safety
    ///
    /// The original mutable borrowed vec must survive for the lifetime of the change buffer.
    unsafe fn change_buffer_from_vec(buffer: &mut Vec<u8>) -> ChangeBuffer {
        unsafe {
            ChangeBuffer::from_raw_parts(
                std::ptr::NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
            )
        }
    }

    #[test]
    fn opcode_try_from_valid_values() -> Result<()> {
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
            (13, "CreateSpan"),
            (14, "CreateSpanFull"),
            (15, "BatchSetMeta"),
            (16, "BatchSetMetric"),
        ];

        for (val, name) in expected {
            let opcode = OpCode::try_from(val as u16)?;
            assert_eq!(opcode.clone() as u16, val as u16);
            assert_eq!(name, format!("{:?}", opcode))
        }

        Ok(())
    }

    #[test]
    fn opcode_try_from_invalid_value() {
        assert!(OpCode::try_from(17u16).is_err());
        assert!(OpCode::try_from(100u16).is_err());
        assert!(OpCode::try_from(u16::MAX).is_err());
    }

    #[test]
    fn buffered_operation_from_buf() -> Result<()> {
        // Layout: opcode (u16 LE) + span_id (u64 LE) = 10 bytes per op header.
        let opcode: u16 = 3; // SetServiceName
        let span_id: u64 = 42;

        let mut buffer = vec![0u8; 10];
        buffer[0..2].copy_from_slice(&opcode.to_le_bytes());
        buffer[2..10].copy_from_slice(&span_id.to_le_bytes());

        let buf = unsafe { change_buffer_from_vec(&mut buffer) };
        let mut index = 0;
        let op = BufferedOperation::from_buf(&buf, &mut index)?;

        assert_eq!(op.opcode as u32, 3);
        assert_eq!(op.span_id, 42);
        assert_eq!(index, 10);
        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_advances_index() -> Result<()> {
        // Two operations packed sequentially, starting after a u64 count header.
        // Each op is 10 bytes: opcode(u16) + span_id(u64)
        let mut buffer = vec![0u8; 28];
        // count header (u64)
        buffer[0..8].copy_from_slice(&0u64.to_le_bytes());
        // first op at offset 8: opcode(u16) + span_id(u64)
        buffer[8..10].copy_from_slice(&(OpCode::Create as u16).to_le_bytes());
        buffer[10..18].copy_from_slice(&1u64.to_le_bytes());
        // second op at offset 18: opcode(u16) + span_id(u64)
        buffer[18..20].copy_from_slice(&(OpCode::SetError as u16).to_le_bytes());
        buffer[20..28].copy_from_slice(&2u64.to_le_bytes());

        let buf = unsafe { change_buffer_from_vec(&mut buffer) };

        let mut index = 8;
        let op1 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op1.opcode as u32, OpCode::Create as u32);
        assert_eq!(op1.span_id, 1);
        assert_eq!(index, 18);

        let op2 = BufferedOperation::from_buf(&buf, &mut index)?;
        assert_eq!(op2.opcode as u32, OpCode::SetError as u32);
        assert_eq!(op2.span_id, 2);
        assert_eq!(index, 28);

        Ok(())
    }

    #[test]
    fn buffered_operation_from_buf_invalid_opcode() {
        let mut buffer = vec![0u8; 10];
        buffer[0..2].copy_from_slice(&999u16.to_le_bytes());
        buffer[2..10].copy_from_slice(&1u64.to_le_bytes());

        let buf = unsafe { change_buffer_from_vec(&mut buffer) };
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }

    #[test]
    fn buffered_operation_from_buf_too_small() {
        // Only 2 bytes — enough for the opcode but not the span_id
        let mut buffer = vec![0u8; 2];
        buffer[0..2].copy_from_slice(&0u16.to_le_bytes());

        let buf = unsafe { change_buffer_from_vec(&mut buffer) };
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }

    #[test]
    fn buffered_operation_from_buf_empty() {
        let mut buffer = vec![0u8; 0];
        let buf = unsafe { change_buffer_from_vec(&mut buffer) };
        let mut index = 0;
        assert!(BufferedOperation::from_buf(&buf, &mut index).is_err());
    }
}
