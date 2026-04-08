- ~~use a hasher that is u32-friendly~~ (already done in latest branch)
- slap #inline on small functions if they're used from another crate (might be useless with LTO)
- why don't we use auto-vect in libdatadog-nodejs?
- ~~use smaller opcode? u64 sounds like way too much~~ (done: u16)
- question: what is the usual length / lifetime of the string table?
- ~~use a branch-less op-encoding for setting directly the memory bytes~~ (done: kind-encoded simple ops)

## Op optimization

### New bit-encoded opcode layout (u16)

Simple ops (raw_op < 32): `(field_idx << 3) | kind`

| Kind (lower 3 bits) | Meaning                                        | Args                       |
|---------------------|------------------------------------------------|----------------------------|
| `0` SET_STR         | set `T::Text` field on span                    | str_id (u16)               |
| `1` SET_I32         | set `i32` field on span                        | i32                        |
| `2` SET_I64         | set `i64` field on span                        | i64                        |
| `3` MAP_STR         | insert str→str into span HashMap               | key_str_id (u16), val (u16)|
| `4` MAP_F64         | insert str→f64 into span HashMap               | key_str_id (u16), val (f64)|
| `5` TRACE_SET_STR   | set `Option<T::Text>` field on trace           | str_id (u16)               |
| `6` TRACE_MAP_STR   | insert str→str into trace FxHashMap            | key_str_id (u16), val (u16)|
| `7` TRACE_MAP_F64   | insert str→f64 into trace FxHashMap            | key_str_id (u16), val (f64)|

Field indices (upper 13 bits):

| Kind       | field_idx | Field            |
|------------|-----------|------------------|
| SET_STR    | 0         | span.service     |
| SET_STR    | 1         | span.name        |
| SET_STR    | 2         | span.resource    |
| SET_STR    | 3         | span.type        |
| SET_I32    | 0         | span.error       |
| SET_I64    | 0         | span.start       |
| SET_I64    | 1         | span.duration    |
| MAP_STR    | 0         | span.meta        |
| MAP_F64    | 0         | span.metrics     |
| TRACE_SET_STR | 0      | trace.origin     |
| TRACE_MAP_STR | 0      | trace.meta       |
| TRACE_MAP_F64 | 0      | trace.metrics    |

### Encoded opcode values for simple ops

| OpCode              | Encoded u16 | = (field_idx << 3) \| kind |
|---------------------|-------------|---------------------------|
| SetServiceName      |  0          | (0 << 3) \| 0              |
| SetError            |  1          | (0 << 3) \| 1              |
| SetStart            |  2          | (0 << 3) \| 2              |
| SetMetaAttr         |  3          | (0 << 3) \| 3              |
| SetMetricAttr       |  4          | (0 << 3) \| 4              |
| SetTraceOrigin      |  5          | (0 << 3) \| 5              |
| SetTraceMetaAttr    |  6          | (0 << 3) \| 6              |
| SetTraceMetricsAttr |  7          | (0 << 3) \| 7              |
| SetName             |  8          | (1 << 3) \| 0              |
| SetDuration         | 10          | (1 << 3) \| 2              |
| SetResourceName     | 16          | (2 << 3) \| 0              |
| SetType             | 24          | (3 << 3) \| 0              |

### Complex opcodes (raw_op >= 32)

| OpCode         | Value |
|----------------|-------|
| Create         | 32    |
| CreateSpan     | 33    |
| CreateSpanFull | 34    |
| BatchSetMeta   | 35    |
| BatchSetMetric | 36    |

### Dispatch mechanism

During `flush_change_buffer`:
- If `raw_op < 32`: simple op → extract `kind = raw_op & 0x7`, `field_idx = raw_op >> 3`
  - Span pointer cached across consecutive ops on same span
  - `interpret_simple_op` uses `SpanFieldTable` (precomputed `offset_of!` values) to write to
    the target field via raw pointer for span ops; direct named-field access for trace ops
- If `raw_op >= 32`: complex op → `interpret_complex_op` dispatches by exact value
