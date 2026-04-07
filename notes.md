- ~~use a hasher that is u32-friendly~~ (already done in latest branch)
- slap #inline on small functions if they're used from another crate (might be useless with LTO)
- why don't we use auto-vect in libdatadog-nodejs?
- use smaller opcode? u64 sounds like way too much
- question: what is the usual length / lifetime of the string table?
- use a branch-less op-encoding for setting directly the memory bytes (1 bit for: Vec or not? Do we need a second bit for String or not?)

## Op optimization

| OpCode               | ID | u32 args | Arg types (in order)                              | Effect                                  |
|----------------------|----|----------|---------------------------------------------------|-----------------------------------------|
| `Create`             |  0 | 6        | u128 (trace_id), u64 (parent_id)                  | _compound_ — creates span + trace entry |
| `SetMetaAttr`        |  1 | 2        | str (key), str (val)                              | insert into `span.meta` (map)           |
| `SetMetricAttr`      |  2 | 3        | str (key), f64 (val)                              | insert into `span.metrics` (map)        |
| `SetServiceName`     |  3 | 1        | str                                               | set field `span.service`                |
| `SetResourceName`    |  4 | 1        | str                                               | set field `span.resource`               |
| `SetError`           |  5 | 1        | num (i32)                                         | set field `span.error`                  |
| `SetStart`           |  6 | 2        | num (i64)                                         | set field `span.start`                  |
| `SetDuration`        |  7 | 2        | num (i64)                                         | set field `span.duration`               |
| `SetType`            |  8 | 1        | str                                               | set field `span.type`                   |
| `SetName`            |  9 | 1        | str                                               | set field `span.name`                   |
| `SetTraceMetaAttr`   | 10 | 2        | str (key), str (val)                              | insert into `trace.meta` (map)          |
| `SetTraceMetricsAttr`| 11 | 3        | str (key), f64 (val)                              | insert into `trace.metrics` (map)       |
| `SetTraceOrigin`     | 12 | 1        | str                                               | set field `trace.origin`                |
| `CreateSpan`         | 13 | 9        | u128 (trace_id), u64 (parent_id), str (name), i64 (start) | _compound_ — Create + SetName + SetStart |
| `CreateSpanFull`     | 14 | 12       | u128 (trace_id), u64 (parent_id), str (name), str (service), str (resource), str (type), i64 (start) | _compound_ — Create + SetName + SetService + SetResource + SetType + SetStart |
| `BatchSetMeta`       | 15 | 1 + N×2  | num (count), then N × (str key, str val)          | insert N entries into `span.meta` (map) |
| `BatchSetMetric`     | 16 | 1 + N×3  | num (count), then N × (str key, f64 val)          | insert N entries into `span.metrics` (map) |

> Note: "u32 args" counts the number of u32 words consumed after the fixed header (opcode u64 + span_id u64).
> A `str` arg is 1 u32 (string-table index). An `f64` is 2 u32. An `i64`/`u64` is 2 u32. A `u128` is 4 u32. An `i32`/`u32` is 1 u32.
> For `BatchSet*`, N is the runtime count value; the total is variable.

