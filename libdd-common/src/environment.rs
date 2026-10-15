// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Process-wide, owned shadow environment, available with `alloc` and without `std`.
//!
//! Initialize explicitly during startup, before concurrent environment mutation is possible.
//! The snapshot is immutable and published once; subsequent reads never access the process
//! environment, including for missing keys. Before initialization, `std` builds fall back to
//! `std::env::var`, and `no_std` builds return [`VarError::NotPresent`].
//!
//! ```
//! use libdd_common::environment;
//!
//! environment::initialize_from([("DD_EXTERNAL_ENV", "startup-value")])?;
//! assert_eq!(environment::var("DD_EXTERNAL_ENV")?, "startup-value");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use alloc::{boxed::Box, collections::BTreeMap, string::String};
use core::{
    fmt, ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

#[cfg(feature = "std")]
use std::ffi::{OsStr, OsString};

static ENVIRONMENT: ShadowEnvironment = ShadowEnvironment::new();

/// Failure to read an environment variable as UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarError {
    NotPresent,
    NotUnicode,
}

impl fmt::Display for VarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotPresent => "environment variable not found",
            Self::NotUnicode => "environment variable is not valid UTF-8",
        })
    }
}

impl core::error::Error for VarError {}

/// Failure to initialize the shadow environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    AlreadyInitialized,
    InvalidName,
    InvalidValue,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyInitialized => "shadow environment is already initialized",
            Self::InvalidName => "environment variable name is empty or contains '=' or NUL",
            Self::InvalidValue => "environment variable value contains NUL",
        })
    }
}

impl core::error::Error for InitError {}

/// Read a variable from the shadow environment, or from the process environment in `std`
/// builds when no shadow has been installed. Missing snapshot entries never fall back.
pub fn var(name: &str) -> Result<String, VarError> {
    ENVIRONMENT.var(name)
}

/// Whether the process-wide shadow environment has been initialized.
pub fn is_initialized() -> bool {
    ENVIRONMENT.is_initialized()
}

/// Copy caller-supplied UTF-8 variables into the shadow environment.
///
/// Copies every supplied pair into owned storage. Duplicate names retain their first value,
/// matching `getenv` lookup order. An empty iterator installs an authoritative empty snapshot.
/// Initialization happens once; invalid entries leave the environment uninitialized.
pub fn initialize_from<I, K, V>(variables: I) -> Result<(), InitError>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    if is_initialized() {
        return Err(InitError::AlreadyInitialized);
    }
    ENVIRONMENT.install(Snapshot::from_variables(variables)?)
}

/// Copy all process variables into owned storage during startup.
///
/// Preserves non-UTF-8 values; [`var`] reports them as [`VarError::NotUnicode`]. Later changes
/// to the process environment are not reflected in the shadow.
///
/// # Safety
/// Call before other threads or native libraries can mutate the process environment. No
/// environment mutation may occur for the duration of the copy, including through C APIs.
#[cfg(any(feature = "std", target_os = "linux"))]
pub unsafe fn initialize() -> Result<(), InitError> {
    if is_initialized() {
        return Err(InitError::AlreadyInitialized);
    }
    #[cfg(feature = "std")]
    {
        let mut values = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            values.entry(name).or_insert(value);
        }
        ENVIRONMENT.install(Snapshot { values })
    }
    #[cfg(not(feature = "std"))]
    {
        unsafe extern "C" {
            static environ: *const *const core::ffi::c_char;
        }
        // SAFETY: the caller ensures the process environment stays valid and immutable.
        unsafe { initialize_from_envp(environ) }
    }
}

/// Copy a null-terminated C environment array into owned storage.
///
/// Accepts a null `envp` as an empty environment. Entries are split at their first `=`;
/// malformed entries are skipped and duplicate names retain their first value.
///
/// # Safety
/// The array and all its NUL-terminated strings must be valid and immutable throughout this
/// call. The array must end in a null pointer. No pointers into it are retained afterward.
#[cfg(any(not(feature = "std"), unix))]
pub unsafe fn initialize_from_envp(envp: *const *const core::ffi::c_char) -> Result<(), InitError> {
    if is_initialized() {
        return Err(InitError::AlreadyInitialized);
    }
    // SAFETY: forwarded from the caller's contract.
    ENVIRONMENT.install(unsafe { Snapshot::from_envp(envp) })
}

struct Snapshot {
    #[cfg(feature = "std")]
    values: BTreeMap<OsString, OsString>,
    #[cfg(not(feature = "std"))]
    values: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Snapshot {
    fn from_variables<I, K, V>(variables: I) -> Result<Self, InitError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let (name, value) = (name.as_ref(), value.as_ref());
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(InitError::InvalidName);
            }
            if value.contains('\0') {
                return Err(InitError::InvalidValue);
            }
            #[cfg(feature = "std")]
            values
                .entry(OsString::from(name))
                .or_insert_with(|| OsString::from(value));
            #[cfg(not(feature = "std"))]
            values
                .entry(name.as_bytes().to_vec())
                .or_insert_with(|| value.as_bytes().to_vec());
        }
        Ok(Self { values })
    }

    #[cfg(any(not(feature = "std"), unix))]
    unsafe fn from_envp(mut envp: *const *const core::ffi::c_char) -> Self {
        let mut values = BTreeMap::new();
        if !envp.is_null() {
            loop {
                // SAFETY: the caller guarantees a readable, NUL-terminated pointer array.
                let entry = unsafe { *envp };
                if entry.is_null() {
                    break;
                }
                // SAFETY: each entry is a valid immutable C string for this call.
                let bytes = unsafe { core::ffi::CStr::from_ptr(entry) }.to_bytes();
                if let Some(index) = bytes.iter().position(|byte| *byte == b'=') {
                    if index != 0 {
                        let name = bytes[..index].to_vec();
                        let value = bytes[index + 1..].to_vec();
                        #[cfg(feature = "std")]
                        {
                            use std::os::unix::ffi::OsStringExt;
                            values
                                .entry(OsString::from_vec(name))
                                .or_insert_with(|| OsString::from_vec(value));
                        }
                        #[cfg(not(feature = "std"))]
                        values.entry(name).or_insert(value);
                    }
                }
                // SAFETY: the caller guarantees the next pointer exists up to the terminator.
                envp = unsafe { envp.add(1) };
            }
        }
        Self { values }
    }

    fn var(&self, name: &str) -> Result<String, VarError> {
        #[cfg(feature = "std")]
        {
            self.values
                .get(OsStr::new(name))
                .ok_or(VarError::NotPresent)?
                .to_str()
                .map(String::from)
                .ok_or(VarError::NotUnicode)
        }
        #[cfg(not(feature = "std"))]
        {
            let value = self
                .values
                .get(name.as_bytes())
                .ok_or(VarError::NotPresent)?;
            core::str::from_utf8(value)
                .map(String::from)
                .map_err(|_| VarError::NotUnicode)
        }
    }
}

struct ShadowEnvironment {
    snapshot: AtomicPtr<Snapshot>,
}

impl ShadowEnvironment {
    const fn new() -> Self {
        Self {
            snapshot: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn is_initialized(&self) -> bool {
        !self.snapshot.load(Ordering::Acquire).is_null()
    }

    fn install(&self, snapshot: Snapshot) -> Result<(), InitError> {
        let pointer = Box::into_raw(Box::new(snapshot));
        match self.snapshot.compare_exchange(
            ptr::null_mut(),
            pointer,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(_) => {
                // SAFETY: this unpublished allocation is still exclusively owned by this call.
                unsafe {
                    drop(Box::from_raw(pointer));
                }
                Err(InitError::AlreadyInitialized)
            }
        }
    }

    fn var(&self, name: &str) -> Result<String, VarError> {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(VarError::NotPresent);
        }
        let pointer = self.snapshot.load(Ordering::Acquire);
        if !pointer.is_null() {
            // SAFETY: release/acquire publication exposes a fully initialized immutable snapshot.
            // It is never replaced, and cannot be dropped while this method borrows `self`.
            return unsafe { &*pointer }.var(name);
        }
        #[cfg(feature = "std")]
        {
            std::env::var(name).map_err(|error| match error {
                std::env::VarError::NotPresent => VarError::NotPresent,
                std::env::VarError::NotUnicode(_) => VarError::NotUnicode,
            })
        }
        #[cfg(not(feature = "std"))]
        Err(VarError::NotPresent)
    }
}

impl Drop for ShadowEnvironment {
    fn drop(&mut self) {
        let pointer = *self.snapshot.get_mut();
        if !pointer.is_null() {
            // SAFETY: exclusive access ensures no readers remain; this owns the published box.
            unsafe {
                drop(Box::from_raw(pointer));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "std")]
    use alloc::string::ToString;
    use alloc::{vec, vec::Vec};

    #[test]
    fn snapshot_owns_all_supplied_values_and_keeps_first_duplicates() {
        let environment = ShadowEnvironment::new();
        let mut variables = vec![
            (String::from("SERVICE"), String::from("checkout")),
            (String::from("EMPTY"), String::new()),
            (String::from("EQUALS"), String::from("a=b=c")),
            (String::from("SERVICE"), String::from("ignored")),
        ];
        let snapshot =
            Snapshot::from_variables(variables.iter().map(|(key, value)| (key, value))).unwrap();
        environment.install(snapshot).unwrap();
        variables[0].1.clear();
        drop(variables);

        assert!(environment.is_initialized());
        assert_eq!(environment.var("SERVICE").unwrap(), "checkout");
        assert_eq!(environment.var("EMPTY").unwrap(), "");
        assert_eq!(environment.var("EQUALS").unwrap(), "a=b=c");
        assert_eq!(environment.var("MISSING"), Err(VarError::NotPresent));
    }

    #[test]
    fn empty_snapshot_is_authoritative_and_cannot_be_replaced() {
        let environment = ShadowEnvironment::new();
        environment
            .install(Snapshot::from_variables(core::iter::empty::<(&str, &str)>()).unwrap())
            .unwrap();
        assert_eq!(environment.var("PATH"), Err(VarError::NotPresent));
        assert_eq!(
            environment.install(Snapshot::from_variables([("PATH", "replacement")]).unwrap()),
            Err(InitError::AlreadyInitialized)
        );
        assert_eq!(environment.var("PATH"), Err(VarError::NotPresent));
    }

    #[test]
    fn invalid_supplied_variables_do_not_create_a_snapshot() {
        for name in ["", "INVALID=NAME", "INVALID\0NAME"] {
            assert!(matches!(
                Snapshot::from_variables([(name, "value")]),
                Err(InitError::InvalidName)
            ));
        }
        assert!(matches!(
            Snapshot::from_variables([("NAME", "invalid\0value")]),
            Err(InitError::InvalidValue)
        ));
        let environment = ShadowEnvironment::new();
        assert!(!environment.is_initialized());
        for name in ["", "INVALID=NAME", "INVALID\0NAME"] {
            assert_eq!(environment.var(name), Err(VarError::NotPresent));
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn uninitialized_std_environment_uses_system_values() {
        let environment = ShadowEnvironment::new();
        let expected = std::env::var("PATH").map_err(|error| match error {
            std::env::VarError::NotPresent => VarError::NotPresent,
            std::env::VarError::NotUnicode(_) => VarError::NotUnicode,
        });
        assert_eq!(environment.var("PATH"), expected);
        assert!(!environment.is_initialized());
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn uninitialized_no_std_environment_has_no_system_fallback() {
        let environment = ShadowEnvironment::new();
        assert_eq!(environment.var("PATH"), Err(VarError::NotPresent));
        assert!(!environment.is_initialized());
    }

    #[cfg(any(not(feature = "std"), unix))]
    #[test]
    fn c_environment_is_copied_and_preserves_non_unicode_values() {
        use alloc::ffi::CString;

        let entries = [
            CString::new("NAME=original=with=equals").unwrap(),
            CString::new("NAME=ignored").unwrap(),
            CString::new("EMPTY=").unwrap(),
            CString::new("malformed").unwrap(),
            CString::new("=invalid").unwrap(),
            CString::new(b"RAW=\xff".as_slice()).unwrap(),
        ];
        let pointers: Vec<_> = entries
            .iter()
            .map(|value| value.as_ptr())
            .chain(core::iter::once(ptr::null()))
            .collect();
        // SAFETY: the owned strings and terminated pointer array stay alive during the copy.
        let snapshot = unsafe { Snapshot::from_envp(pointers.as_ptr()) };
        drop(entries);
        drop(pointers);
        let environment = ShadowEnvironment::new();
        environment.install(snapshot).unwrap();
        assert_eq!(environment.var("NAME").unwrap(), "original=with=equals");
        assert_eq!(environment.var("EMPTY").unwrap(), "");
        assert_eq!(environment.var("RAW"), Err(VarError::NotUnicode));
    }

    #[cfg(any(not(feature = "std"), unix))]
    #[test]
    fn null_c_environment_installs_an_empty_snapshot() {
        let environment = ShadowEnvironment::new();
        // SAFETY: null is explicitly accepted as an empty environment.
        environment
            .install(unsafe { Snapshot::from_envp(ptr::null()) })
            .unwrap();
        assert!(environment.is_initialized());
        assert_eq!(environment.var("PATH"), Err(VarError::NotPresent));
    }

    #[cfg(feature = "std")]
    #[test]
    fn concurrent_initialization_publishes_one_complete_snapshot() {
        use core::sync::atomic::AtomicUsize;
        let environment = ShadowEnvironment::new();
        let successes = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for value in 0..16 {
                let environment = &environment;
                let successes = &successes;
                scope.spawn(move || {
                    let snapshot =
                        Snapshot::from_variables([("VALUE", value.to_string())]).unwrap();
                    if environment.install(snapshot).is_ok() {
                        successes.fetch_add(1, Ordering::Relaxed);
                    }
                    assert!(environment.var("VALUE").unwrap().parse::<usize>().unwrap() < 16);
                });
            }
        });
        assert_eq!(successes.load(Ordering::Relaxed), 1);
    }
}
