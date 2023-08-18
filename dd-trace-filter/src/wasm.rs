use crate::filter::Filter;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = "Filter")]
pub struct WasmFilter {
  filter: Filter
}

#[wasm_bindgen(js_class = "Filter")]
impl WasmFilter {
  /// Construct an instance of the filter engine
  #[wasm_bindgen(constructor)]
  pub fn new() -> Self {
    Self {
      filter: Filter::new()
    }
  }

  #[wasm_bindgen(js_name = filterSpan)]
  pub fn filter_span_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    self.filter.filter_span_data(data)
  }

  #[wasm_bindgen(js_name = filterTrace)]
  pub fn filter_trace_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    self.filter.filter_trace_data(data)
  }

  #[wasm_bindgen(js_name = filterChunk)]
  pub fn filter_chunk_data(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
    self.filter.filter_chunk_data(data)
  }
}

#[cfg(test)]
mod test {
  use super::*;

  // use std::collections::HashMap;
  use wasm_bindgen_test::wasm_bindgen_test;
  
  #[test]
  #[wasm_bindgen_test]
  fn test_filter() {
    let _filter = WasmFilter::new();

    // TODO: Write an actual test for this...
  }
}
