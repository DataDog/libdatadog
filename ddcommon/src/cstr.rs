// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#[doc(hidden)]
pub const fn validate_cstr_contents(bytes: &[u8]) {
    // `str_len` is the length excluding the null terminator.
    let str_len = bytes.len() - 1usize;
    if bytes[str_len] != b'\0' {
        panic!("cstr must be null terminated");
    }

    // Search for a null byte, safe due to above guard.
    let mut i = 0;
    while bytes[i] != b'\0' {
        i += 1;
    }

    // The only null byte should have been the last byte of the slice.
    if i != str_len {
        panic!("cstr string cannot contain null character outside of last element");
    }
}

#[macro_export]
macro_rules! cstr {
    ($s:literal) => {{
        let mut bytes = $s.as_bytes();
        if bytes[bytes.len() - 1usize] != b'\0' {
            bytes = concat!($s, "\0").as_bytes();
        }

        $crate::cstr::validate_cstr_contents(bytes);
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(bytes) }
    }};
}

#[macro_export]
macro_rules! cstr_u8 {
    ($s:literal) => {{
        $crate::cstr::validate_cstr_contents($s);
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked($s as &[u8]) }
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cstr() {
        assert_eq!(b"/dev/null", cstr!("/dev/null").to_bytes());
        assert_eq!(b"/dev/null", cstr!("/dev/null\0").to_bytes());
        assert_eq!(b"/dev/null", cstr_u8!(b"/dev/null\0").to_bytes());
    }

    #[test]
    #[should_panic]
    fn test_invalid_cstr_with_extra_null_character() {
        cstr!("/dev/null\0\0");
    }

    #[test]
    #[should_panic]
    fn test_invalid_cstr_u8_without_terminatid_nul() {
        cstr_u8!(b"/dev/null");
    }

    #[test]
    #[should_panic]
    fn test_invalid_cstr_with_nul_character() {
        cstr!("/dev/\0null");
    }
}
