const PASS: u32 = 0;
const FAIL: u32 = 1;
const FAIL_NULL_POINTER: u32 = 2;

use crate::Filter;

#[no_mangle]
pub extern "C" fn dd_trace_filter_init(ptr: *mut *mut Filter) -> u32 {
  // Validate pointers
  if ptr.is_null() {
    return FAIL_NULL_POINTER;
  }

  // Construct Rust instance of the Filter
  let filter = Filter {};

  // Assign boxed instance to output pointer
  unsafe {
    *ptr = Box::into_raw(Box::new(filter));
  };

  PASS
}

#[no_mangle]
pub extern "C" fn dd_trace_filter_free(ptr: *mut Filter) -> u32 {
  // Validate pointers
  if ptr.is_null() {
    return FAIL_NULL_POINTER;
  }

  // Drop the boxed pointer
  unsafe {
    drop(Box::from_raw(ptr));
  }

  PASS
}

#[no_mangle]
pub extern "C" fn dd_trace_filter_apply(
  ptr: *mut Filter,
  input: *const u8,
  len: usize,
  output: *mut *mut Vec<u8>
) -> u32 {
  // Validate pointers
  if ptr.is_null() || input.is_null() || output.is_null() {
    return FAIL_NULL_POINTER;
  }

  // If there's nothing in the buffer there's no filtering needed
  if len == 0 {
    return PASS;
  }

  // Convert C objects to something useful to Rust
  let filter: &Filter = unsafe { &mut *ptr };
  let data = unsafe {
    std::slice::from_raw_parts(input, len)
  };

  // Attempt to apply the filter
  match filter.filter(data.to_vec()) {
    Ok(data) => {
      unsafe {
        *output = Box::into_raw(Box::new(data));
      };

      PASS
    },
    Err(_) => FAIL
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn test_init_filter() {
    let mut ret: u32;

    let mut filter: *mut Filter = std::ptr::null_mut();
    ret = dd_trace_filter_init(&mut filter);
    assert_eq!(0, ret);
    
    let input = vec![1, 2, 3];
    let mut output: *mut Vec<u8> = std::ptr::null_mut();
    ret = dd_trace_filter_apply(filter, input.as_ptr(), 3, &mut output);
    assert_eq!(0, ret);
    
    assert_eq!(*unsafe { &mut *output }, vec![
      0x91,
      0xa7,
      0x53,
      0x74,
      0x65,
      0x70,
      0x68,
      0x65,
      0x6e
    ]);

    ret = dd_trace_filter_free(filter);
    assert_eq!(0, ret);
  }
}
