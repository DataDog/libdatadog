#[cfg(test)]
use crate::change_buffer::{ChangeBuffer, Result};

// ---------------------------------------------------------------------------
// Simple opcode encoding (u16 < COMPLEX_OP_BASE)
//
// Lower 3 bits = operation kind (what type of field and how to apply it).
// Upper 13 bits = field_idx (which field within that kind's offset table).
//
// Encoded value = (field_idx << 3) | kind
// ---------------------------------------------------------------------------

/// Lowest value reserved for complex (non-encoded) opcodes.
/// Any raw u16 < COMPLEX_OP_BASE is a simple op; >= is a complex op.
pub const COMPLEX_OP_BASE: u16 = 32;

// -- Op kinds (lower 3 bits) ------------------------------------------------

/// Set a `T::Text` field on the span directly.
/// Args: str_id (u16 → string table key).
pub const OP_KIND_SET_STR: u16 = 0;
/// Set an `i32` field on the span directly.
/// Args: i32.
pub const OP_KIND_SET_I32: u16 = 1;
/// Set an `i64` field on the span directly.
/// Args: i64.
pub const OP_KIND_SET_I64: u16 = 2;
/// Insert into a `HashMap<T::Text, T::Text>` field on the span.
/// Args: key_str_id (u16), val_str_id (u16).
pub const OP_KIND_MAP_STR: u16 = 3;
/// Insert into a `HashMap<T::Text, f64>` field on the span.
/// Args: key_str_id (u16), val (f64).
pub const OP_KIND_MAP_F64: u16 = 4;
/// Set an `Option<T::Text>` field on the trace for this span.
/// Args: str_id (u16).
pub const OP_KIND_TRACE_SET_STR: u16 = 5;
/// Insert into a `FxHashMap<T::Text, T::Text>` field on the trace for this span.
/// Args: key_str_id (u16), val_str_id (u16).
pub const OP_KIND_TRACE_MAP_STR: u16 = 6;
/// Insert into a `FxHashMap<T::Text, f64>` field on the trace for this span.
/// Args: key_str_id (u16), val (f64).
pub const OP_KIND_TRACE_MAP_F64: u16 = 7;

// -- Field indices for SET_STR (kind = 0) -----------------------------------
/// `span.service` (field_idx 0 in the str_fields table)
pub const SPAN_STR_SERVICE: u16 = 0;
/// `span.name` (field_idx 1)
pub const SPAN_STR_NAME: u16 = 1;
/// `span.resource` (field_idx 2)
pub const SPAN_STR_RESOURCE: u16 = 2;
/// `span.type` (field_idx 3)
pub const SPAN_STR_TYPE: u16 = 3;

// -- Field indices for SET_I32 (kind = 1) -----------------------------------
/// `span.error` (field_idx 0 in the i32_fields table)
pub const SPAN_I32_ERROR: u16 = 0;

// -- Field indices for SET_I64 (kind = 2) -----------------------------------
/// `span.start` (field_idx 0 in the i64_fields table)
pub const SPAN_I64_START: u16 = 0;
/// `span.duration` (field_idx 1)
pub const SPAN_I64_DURATION: u16 = 1;

// -- Field indices for MAP_STR (kind = 3) -----------------------------------
/// `span.meta` (field_idx 0 in the str_map_fields table)
pub const SPAN_MAP_META: u16 = 0;

// -- Field indices for MAP_F64 (kind = 4) -----------------------------------
/// `span.metrics` (field_idx 0 in the f64_map_fields table)
pub const SPAN_MAP_METRICS: u16 = 0;

// -- Field indices for TRACE_SET_STR (kind = 5) -----------------------------
/// `trace.origin` (field_idx 0 in the trace_opt_str_fields table)
pub const TRACE_STR_ORIGIN: u16 = 0;

// -- Field indices for TRACE_MAP_STR (kind = 6) -----------------------------
/// `trace.meta` (field_idx 0 in the trace_str_map_fields table)
pub const TRACE_MAP_META: u16 = 0;

// -- Field indices for TRACE_MAP_F64 (kind = 7) -----------------------------
/// `trace.metrics` (field_idx 0 in the trace_f64_map_fields table)
pub const TRACE_MAP_METRICS: u16 = 0;

// ---------------------------------------------------------------------------
// OpCode: named constants for all opcodes (simple + complex).
//
// Simple opcodes use (field_idx << 3) | kind as their value.
// Complex opcodes occupy values >= COMPLEX_OP_BASE (32).
//
// These names are the public API used by the JS layer to refer to opcodes by
// name. The u16 values are what actually go into the change buffer.
// ---------------------------------------------------------------------------

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    // -- Simple ops --
    SetServiceName      = (SPAN_STR_SERVICE   << 3) | OP_KIND_SET_STR,   //  0
    SetError            = (SPAN_I32_ERROR     << 3) | OP_KIND_SET_I32,   //  1
    SetStart            = (SPAN_I64_START     << 3) | OP_KIND_SET_I64,   //  2
    SetMetaAttr         = (SPAN_MAP_META      << 3) | OP_KIND_MAP_STR,   //  3
    SetMetricAttr       = (SPAN_MAP_METRICS   << 3) | OP_KIND_MAP_F64,   //  4
    SetTraceOrigin      = (TRACE_STR_ORIGIN   << 3) | OP_KIND_TRACE_SET_STR, //  5
    SetTraceMetaAttr    = (TRACE_MAP_META     << 3) | OP_KIND_TRACE_MAP_STR, //  6
    SetTraceMetricsAttr = (TRACE_MAP_METRICS  << 3) | OP_KIND_TRACE_MAP_F64, //  7
    SetName             = (SPAN_STR_NAME      << 3) | OP_KIND_SET_STR,   //  8
    SetDuration         = (SPAN_I64_DURATION  << 3) | OP_KIND_SET_I64,   // 10
    SetResourceName     = (SPAN_STR_RESOURCE  << 3) | OP_KIND_SET_STR,   // 16
    SetType             = (SPAN_STR_TYPE      << 3) | OP_KIND_SET_STR,   // 24
    // -- Complex ops --
    Create              = 32,
    CreateSpan          = 33,
    CreateSpanFull      = 34,
    BatchSetMeta        = 35,
    BatchSetMetric      = 36,
}

/// Convenience: read the next opcode (u16) and span_id (u64) from the buffer,
/// advancing `index`. Used in tests.
#[cfg(test)]
pub struct BufferedOpHeader {
    pub raw_op: u16,
    pub span_id: u64,
}

#[cfg(test)]
impl BufferedOpHeader {
    pub fn from_buf(buf: &ChangeBuffer, index: &mut usize) -> Result<Self> {
        let raw_op: u16 = buf.read(index)?;
        let span_id: u64 = buf.read(index)?;
        Ok(BufferedOpHeader { raw_op, span_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that each simple opcode encodes to the expected u16 value and
    /// that decoding the lower 3 bits / upper bits recovers the right kind and
    /// field_idx.
    #[test]
    fn simple_opcode_encoding() {
        let cases: &[(OpCode, u16, u16)] = &[
            // (opcode, expected_kind, expected_field_idx)
            (OpCode::SetServiceName,       OP_KIND_SET_STR,        SPAN_STR_SERVICE),
            (OpCode::SetName,              OP_KIND_SET_STR,        SPAN_STR_NAME),
            (OpCode::SetResourceName,      OP_KIND_SET_STR,        SPAN_STR_RESOURCE),
            (OpCode::SetType,              OP_KIND_SET_STR,        SPAN_STR_TYPE),
            (OpCode::SetError,             OP_KIND_SET_I32,        SPAN_I32_ERROR),
            (OpCode::SetStart,             OP_KIND_SET_I64,        SPAN_I64_START),
            (OpCode::SetDuration,          OP_KIND_SET_I64,        SPAN_I64_DURATION),
            (OpCode::SetMetaAttr,          OP_KIND_MAP_STR,        SPAN_MAP_META),
            (OpCode::SetMetricAttr,        OP_KIND_MAP_F64,        SPAN_MAP_METRICS),
            (OpCode::SetTraceOrigin,       OP_KIND_TRACE_SET_STR,  TRACE_STR_ORIGIN),
            (OpCode::SetTraceMetaAttr,     OP_KIND_TRACE_MAP_STR,  TRACE_MAP_META),
            (OpCode::SetTraceMetricsAttr,  OP_KIND_TRACE_MAP_F64,  TRACE_MAP_METRICS),
        ];

        for &(op, expected_kind, expected_field_idx) in cases {
            let val = op as u16;
            assert!(val < COMPLEX_OP_BASE, "{op:?} should be < COMPLEX_OP_BASE");
            assert_eq!(val & 0x7, expected_kind, "{op:?}: wrong kind");
            assert_eq!(val >> 3, expected_field_idx, "{op:?}: wrong field_idx");
        }
    }

    #[test]
    fn complex_opcode_values() {
        assert_eq!(OpCode::Create as u16, 32);
        assert_eq!(OpCode::CreateSpan as u16, 33);
        assert_eq!(OpCode::CreateSpanFull as u16, 34);
        assert_eq!(OpCode::BatchSetMeta as u16, 35);
        assert_eq!(OpCode::BatchSetMetric as u16, 36);

        for op in [OpCode::Create, OpCode::CreateSpan, OpCode::CreateSpanFull,
                   OpCode::BatchSetMeta, OpCode::BatchSetMetric] {
            assert!(op as u16 >= COMPLEX_OP_BASE, "{op:?} should be >= COMPLEX_OP_BASE");
        }
    }

    #[test]
    fn buffered_op_header_from_buf() -> Result<()> {
        let raw_op: u16 = OpCode::SetServiceName as u16;
        let span_id: u64 = 0xDEADBEEF;

        let mut buffer = vec![0u8; 10];
        buffer[0..2].copy_from_slice(&raw_op.to_le_bytes());
        buffer[2..10].copy_from_slice(&span_id.to_le_bytes());

        let buf = unsafe { ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len()) };
        let mut index = 0;
        let header = BufferedOpHeader::from_buf(&buf, &mut index)?;

        assert_eq!(header.raw_op, raw_op);
        assert_eq!(header.span_id, 0xDEADBEEF);
        assert_eq!(index, 10);
        Ok(())
    }

    #[test]
    fn buffered_op_header_advances_index_sequentially() -> Result<()> {
        // Two operations packed after a u32 count header
        let mut buffer = vec![0u8; 24];
        // count header (u32)
        buffer[0..4].copy_from_slice(&0u32.to_le_bytes());
        // first op at offset 4: opcode(u16) + span_id(u64)
        buffer[4..6].copy_from_slice(&(OpCode::Create as u16).to_le_bytes());
        buffer[6..14].copy_from_slice(&1u64.to_le_bytes());
        // second op at offset 14
        buffer[14..16].copy_from_slice(&(OpCode::SetError as u16).to_le_bytes());
        buffer[16..24].copy_from_slice(&2u64.to_le_bytes());

        let buf = unsafe { ChangeBuffer::from_raw_parts(buffer.as_mut_ptr(), buffer.len()) };
        let mut index = 4;

        let h1 = BufferedOpHeader::from_buf(&buf, &mut index)?;
        assert_eq!(h1.raw_op, OpCode::Create as u16);
        assert_eq!(h1.span_id, 1);
        assert_eq!(index, 14);

        let h2 = BufferedOpHeader::from_buf(&buf, &mut index)?;
        assert_eq!(h2.raw_op, OpCode::SetError as u16);
        assert_eq!(h2.span_id, 2);
        assert_eq!(index, 24);
        Ok(())
    }
}
