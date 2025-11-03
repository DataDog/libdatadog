# libdd-trace-obfuscation

Sensitive data obfuscation for distributed tracing.

## Overview

`libdd-trace-obfuscation` provides utilities for obfuscating sensitive information in trace data such as SQL queries, Redis commands, MongoDB queries, and other potentially sensitive strings.

## Features

- **SQL Obfuscation**: Obfuscate SQL queries while preserving structure
- **Redis Obfuscation**: Remove sensitive data from Redis commands
- **MongoDB Obfuscation**: Obfuscate MongoDB queries and commands
- **Elasticsearch Obfuscation**: Clean Elasticsearch queries
- **HTTP Obfuscation**: Remove sensitive HTTP query parameters
- **Configurable Rules**: Customize obfuscation rules per data type
- **Performance**: Optimized for low overhead

## Obfuscation Types

### SQL Obfuscation
Replaces literal values in SQL queries while preserving query structure:
- String literals → `?`
- Numeric literals → `?`
- Lists of values → `?`

### Redis Obfuscation
Removes sensitive arguments from Redis commands while keeping command structure:
- `SET key value` → `SET key ?`
- `HMSET key field value` → `HMSET key field ?`

### MongoDB Obfuscation
Obfuscates MongoDB query filters and updates:
- `{username: "admin"}` → `{username: ?}`
- Preserves query structure

## Example Usage

```rust
use libdd_trace_obfuscation::obfuscate;

// Obfuscate SQL
let sql = "SELECT * FROM users WHERE id = 123 AND name = 'admin'";
// let obfuscated = obfuscate::sql(sql);
// Result: "SELECT * FROM users WHERE id = ? AND name = ?"
```

## Configuration

Obfuscation can be configured per span type with options for:
- Enable/disable specific obfuscators
- Custom replacement tokens
- Obfuscation levels (partial vs full)

