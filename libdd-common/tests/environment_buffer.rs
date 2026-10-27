// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_has_atomic = "ptr")]

use libdd_common::environment::EnvCapability;
use libdd_common::{environment, ffi::OsStr};
#[cfg(all(feature = "std", unix))]
use std::os::unix::ffi::OsStrExt;

// A separate process tests the public one-time initialization API in every feature mode.
#[test]
#[cfg(any(not(feature = "std"), unix))]
fn buffer_initialization_copies_values_and_returns_failed_storage() {
    static mut SMALL: [u8; 2] = [0; 2];
    static mut STORAGE: [u8; 128] = [0; 128];

    assert!(!environment::is_initialized());
    assert!(environment::var_os_ref("PATH").is_none());
    let capability = environment::ShadowEnvCapability::new();
    assert_eq!(capability.get("PATH"), Ok(None));
    // SAFETY: this test runs once and creates the only references to these statics.
    let (small, storage) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(SMALL),
            &mut *core::ptr::addr_of_mut!(STORAGE),
        )
    };
    let failure = environment::with_buffer(small)
        .initialize_from([("K", "value")])
        .unwrap_err();
    assert_eq!(failure.error, environment::InitError::BufferTooSmall);
    assert!(!environment::is_initialized());
    // The failed initialization retained no access to this buffer.
    let returned = failure.buffer;
    returned.fill(0xaa);

    let storage = environment::with_buffer(storage).into_buffer();
    storage.fill(0xaa);
    assert!(!environment::is_initialized());

    let mut service = *b"SERVICE=startup\0";
    let mut raw_entry = *b"K\xff=\xff=v\0";
    let raw_key = OsStr::from_bytes(b"K\xff");
    let envp = [
        service.as_ptr().cast(),
        raw_entry.as_ptr().cast(),
        c"SERVICE=ignored".as_ptr(),
        c"EMPTY=".as_ptr(),
        c"RAW=\xff".as_ptr(),
        c"malformed".as_ptr(),
        c"=invalid".as_ptr(),
        core::ptr::null(),
    ];
    // SAFETY: the terminated pointer array and its C strings remain valid during the copy.
    unsafe { environment::with_buffer(storage).initialize_from_envp(envp.as_ptr()) }.unwrap();
    service.fill(b'x');
    raw_entry.fill(b'x');

    let saved = environment::var_os_ref("SERVICE").unwrap();
    assert_eq!(saved.as_encoded_bytes(), b"startup");
    // The result borrows the capability's snapshot, independently of the lookup key.
    let borrowed = {
        let name = *b"SERVICE";
        capability
            .get(core::str::from_utf8(&name).unwrap())
            .unwrap()
            .unwrap()
    };
    assert_eq!(borrowed, "startup");
    assert_eq!(borrowed.as_ptr(), saved.as_encoded_bytes().as_ptr());
    assert_eq!(capability.get("EMPTY"), Ok(Some("")));
    assert_eq!(capability.get("MISSING"), Ok(None));
    assert_eq!(capability.get("BAD=NAME"), Ok(None));
    assert_eq!(
        capability.get("RAW"),
        Err(environment::EnvError::NotUnicode)
    );
    assert_eq!(
        environment::var_os_ref(raw_key).unwrap().as_encoded_bytes(),
        &[0xff, b'=', b'v']
    );
    assert_eq!(
        environment::var_os_ref("EMPTY").unwrap().as_encoded_bytes(),
        b""
    );
    assert!(environment::var_os_ref("MISSING").is_none());
    assert!(environment::var_os_ref("BAD=NAME").is_none());

    let failure = environment::with_buffer(returned)
        .initialize_from([("K", "replacement")])
        .unwrap_err();
    assert_eq!(failure.error, environment::InitError::AlreadyInitialized);
    assert_eq!(failure.buffer, &[0xaa; 2]);
    assert_eq!(saved.as_encoded_bytes(), b"startup");

    #[cfg(feature = "alloc")]
    {
        assert_eq!(environment::var("SERVICE").unwrap(), "startup");
        let raw = environment::var_os(raw_key).unwrap();
        assert_eq!(
            environment::var(raw_key),
            Err(environment::VarError::NotUnicode(raw))
        );
    }
}

#[cfg(all(feature = "std", windows))]
#[test]
fn buffer_preserves_native_windows_surrogates() {
    use libdd_common::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    static mut STORAGE: [u8; 128] = [0; 128];
    let key = OsString::from_wide(&[u16::from(b'K'), 0xd800]);
    let value = OsString::from_wide(&[0xdc00]);
    // SAFETY: this test consumes its unique static storage exactly once.
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(STORAGE) };
    let capability = environment::ShadowEnvCapability::new();
    assert_eq!(capability.get("PATH"), Ok(None));
    environment::with_buffer(storage)
        .initialize_from([(&key, &value), (&OsString::from("RAW"), &value)])
        .unwrap();
    let saved: &OsStr = environment::var_os_ref(&key).unwrap();
    assert_eq!(saved.as_encoded_bytes(), value.as_encoded_bytes());
    assert_eq!(
        capability.get("RAW"),
        Err(environment::EnvError::NotUnicode)
    );
    assert_eq!(
        environment::var(&key),
        Err(environment::VarError::NotUnicode(value))
    );
}
