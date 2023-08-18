use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use crate::truncate::TruncateUtf8;

/// Span representation with msgpack encode and decode
// TODO: for 0.5 all Strings should be u32
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Span {
  #[serde(rename = "type")]
  span_type: Option<String>,
  trace_id: u64,
  span_id: u64,
  parent_id: u64,
  name: String,
  resource: String,
  service: String,
  error: u64,
  start: u64,
  duration: u64,
  meta: HashMap<String, String>,
  metrics: HashMap<String, f64>,
}

// Traces are just arrays of spans
type Trace = Vec<Span>;

/// List of traces in the chunk
type Chunk = Vec<Trace>;

/// Filter engine
/// 
/// The filter engine should be constructed when the app starts and then be
/// reused for the lifetime of the process. Each chunk that would be written
/// to the network would first be given to the filter, processed, and returned.
pub struct Filter {
  max_resource_length: usize,
  max_meta_key_length: usize,
  max_meta_value_length: usize,
  max_metrics_key_length: usize
}

impl Filter {
  /// Construct an instance of the filter engine
  pub fn new() -> Self {
    Self {
      max_resource_length: 5000,
      max_meta_key_length: 200,
      max_meta_value_length: 25000,
      max_metrics_key_length: 200
    }
  }

  pub fn filter_meta_key(&self, key: String) -> String {
    key.truncate_utf8(self.max_meta_key_length)
  }

  pub fn filter_meta_value(&self, value: String) -> String {
    value.truncate_utf8(self.max_meta_value_length)
  }

  pub fn filter_meta(&self, meta: HashMap<String, String>) -> HashMap<String, String> {
    meta.into_iter()
      .map(|(key, value)| {
        (self.filter_meta_key(key), self.filter_meta_value(value))
      })
      .collect::<HashMap<String, String>>()
  }

  pub fn filter_metrics_key(&self, key: String) -> String {
    key.truncate_utf8(self.max_metrics_key_length)
  }

  pub fn filter_metrics(&self, meta: HashMap<String, f64>) -> HashMap<String, f64> {
    meta.into_iter()
      .map(|(key, value)| {
        (self.filter_metrics_key(key), value)
      })
      .collect::<HashMap<String, f64>>()
  }

  pub fn filter_span(&self, span: Span) -> Span {
    println!("filtering span: {:#?}", span);

    Span {
      span_type: span.span_type.clone(),
      trace_id: span.trace_id.clone(),
      span_id: span.span_id.clone(),
      parent_id: span.parent_id.clone(),
      name: span.name.clone(),
      resource: span.resource.truncate_utf8(self.max_resource_length),
      service: span.service.clone(),
      error: span.error.clone(),
      start: span.start.clone(),
      duration: span.duration.clone(),
      meta: self.filter_meta(span.meta),
      metrics: self.filter_metrics(span.metrics),
    }
  }

  /// Filter a msgpack encoded Trace to a new msgpack encoded Chunk
  pub fn filter_span_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    let span: Span = rmp_serde::from_slice(&data).unwrap();
    Ok(rmp_serde::to_vec(&self.filter_span(span)).unwrap())
  }

  pub fn filter_trace(&self, trace: Trace) -> Trace {
    trace.into_iter()
      .map(|span| self.filter_span(span))
      .collect()
  }

  /// Filter a msgpack encoded Trace to a new msgpack encoded Chunk
  pub fn filter_trace_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    let trace: Trace = rmp_serde::from_slice(&data).unwrap();
    Ok(rmp_serde::to_vec(&self.filter_trace(trace)).unwrap())
  }

  /// Filter a input Chunk to a new output Chunk
  pub fn filter_chunk(&self, chunk: Chunk) -> Chunk {
    chunk.into_iter()
      .map(|trace| self.filter_trace(trace))
      .collect()
  }

  /// Filter a msgpack encoded Chunk to a new msgpack encoded Chunk
  pub fn filter_chunk_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    let chunk: Chunk = rmp_serde::from_slice(&data).unwrap();
    Ok(rmp_serde::to_vec(&self.filter_chunk(chunk)).unwrap())
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn test_filter() {
    let filter = Filter::new();

    // Dumped from dd-trace-js
    let bytes = include_bytes!("./out.data");

    let expected = rmp_serde::to_vec(&vec![
      vec![
        Span {
          span_type: None,
          trace_id: 8425192693734688651,
          span_id: 3562883422441930249,
          parent_id: 727453526424589197,
          name: "name".into(),
          resource: "resource".into(),
          service: "unnamed-service".into(),
          error: 0,
          start: 1234567890,
          duration: 1234,
          meta: HashMap::from([
            ("foo".into(), "bar".into())
          ]),
          metrics: HashMap::from([
            ("a".into(), 1.0)
          ]),
        }
      ]
    ]).unwrap();

    assert_eq!(filter.filter_chunk_data(bytes.to_vec()).unwrap(), expected);

    let input = rmp_serde::to_vec(&vec![
      vec![
        Span {
          service: "service".into(),
          name: "name".into(),
          resource: "resource".into(),
          trace_id: 123,
          span_id: 456,
          parent_id: 789,
          start: 1234,
          duration: 5678,
          error: 1,
          meta: HashMap::from([
            ("key".into(), "value".into())
          ]),
          metrics: HashMap::from([
            ("key".into(), 0.12345f64)
          ]),
          span_type: Some("span type".into())
        }
      ]
    ]).unwrap();

    let expected = rmp_serde::to_vec(&vec![
      vec![
        Span {
          service: "service".into(),
          name: "name".into(),
          resource: "resource".into(),
          trace_id: 123,
          span_id: 456,
          parent_id: 789,
          start: 1234,
          duration: 5678,
          error: 1,
          meta: HashMap::from([
            ("key".into(), "value".into())
          ]),
          metrics: HashMap::from([
            ("key".into(), 0.12345f64)
          ]),
          span_type: Some("span type".into())
        }
      ]
    ]).unwrap();

    assert_eq!(filter.filter_chunk_data(input).unwrap(), expected);
  }
}
