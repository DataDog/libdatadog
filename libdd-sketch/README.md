# libdd-sketch

DDSketch implementation for distributed quantile estimation.

## Overview

`libdd-sketch` provides a Rust implementation of DDSketch, a distributed quantile sketch algorithm that provides accurate quantile estimates with guaranteed relative error bounds.

## Features

- **Relative Error Guarantee**: Configurable relative accuracy
- **Mergeable**: Sketches can be combined
- **Memory Efficient**: Logarithmic space complexity
- **Fast Queries**: O(1) quantile queries
- **Protobuf Serialization**: Serialize for transmission to Datadog

## What is DDSketch?

DDSketch is a data structure for tracking value distributions and computing quantiles (percentiles) with:
- Guaranteed relative error (e.g., ±2%)
- Low memory footprint
- Ability to merge sketches from multiple sources
- No need to know value range in advance

Perfect for tracking latencies, sizes, and other metrics in distributed systems.

## Example Usage

```rust
use datadog_ddsketch::DDSketch;

// Create a sketch with 2% relative error
let mut sketch = DDSketch::new(0.02);

// Add values
sketch.add(42.0);
sketch.add(100.0);
sketch.add(250.0);

// Query quantiles
let p50 = sketch.quantile(0.5)?; // median
let p99 = sketch.quantile(0.99)?;
let p999 = sketch.quantile(0.999)?;

// Merge sketches
let mut combined = sketch1.merge(&sketch2);
```

## Use Cases

- Latency monitoring (p50, p95, p99)
- Request size distributions
- Memory usage tracking
- Any metric where quantiles are important

## Accuracy

The sketch guarantees that for any quantile q:
```
|actual_value - estimated_value| / actual_value ≤ α
```
where α is the configured relative accuracy.

## References

- [DDSketch Paper](https://arxiv.org/abs/1908.10693)
- [Datadog Engineering Blog](https://www.datadoghq.com/blog/engineering/computing-accurate-percentiles-with-ddsketch/)

