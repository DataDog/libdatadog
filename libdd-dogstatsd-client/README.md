# libdd-dogstatsd-client

DogStatsD client library for sending metrics to Datadog.

## Overview

`libdd-dogstatsd-client` provides a client for sending metrics to Datadog via the DogStatsD protocol, an extension of StatsD with additional features like tags and histograms.
This client provides rust methods to interact with a dogstatsd server. It is mainly used in the sidecar and data-pipeline
crates, but should be capable of being used elsewhere. See the crate docs for usage details.

## Features

- **Metric Types**: Count, gauge, histogram, distribution, set, timing
- **Tagging**: Attach tags to metrics for aggregation
- **Sampling**: Sample high-volume metrics
- **Buffering**: Efficient batching of metrics
- **UDP Transport**: Non-blocking UDP transmission
- **Unix Domain Sockets**: Lower overhead on same host
- **Container Support**: Automatic container ID tagging

## Supported Metric Types

### Counter
Tracks the number of occurrences of events:
```rust
// client.count("page.views", 1, &["page:home"])?;
```

### Gauge
Tracks the current value of something:
```rust
// client.gauge("queue.size", 42, &[])?;
```

### Histogram
Tracks statistical distribution of values:
```rust
// client.histogram("request.duration", 150.0, &["endpoint:/api"])?;
```

### Distribution
Similar to histogram but aggregated server-side:
```rust
// client.distribution("request.size", 1024, &[])?;
```

### Set
Counts unique occurrences:
```rust
// client.set("unique.users", "user123", &[])?;
```

### Timing
Shorthand for duration histograms:
```rust
// client.timing("request.time", 150, &[])?; // milliseconds
```

## Tags

Tags provide dimensions for aggregation:
```rust
let tags = vec!["env:prod", "service:web", "version:1.2.3"];
// client.count("requests", 1, &tags)?;
```

## Example Usage

```rust
use libdd_dogstatsd_client::Client;

// Create client
let client = Client::new("127.0.0.1:8125")?;

// Send metrics
// client.count("api.requests", 1, &["endpoint:/users"])?;
// client.gauge("cache.size", 1024, &[])?;
// client.histogram("request.duration", 150.0, &["endpoint:/api"])?;
```

## Transport Options

- **UDP**: Default, non-blocking
- **Unix Domain Socket**: Lower overhead for same-host communication
- **Named Pipe**: Windows support
