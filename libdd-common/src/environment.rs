// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Process-wide immutable shadow environment, with owned or caller-provided storage.
//! Available on targets with pointer-width atomics.
//!
//! Initialize explicitly during startup, before concurrent environment mutation is possible.
//! The snapshot is immutable and published once; subsequent reads never access the process
//! environment, including for missing keys. Before initialization, `std` builds fall back to
//! `std::env` for owned reads, and `no_std` builds report absent variables. With `alloc`,
//! `var_os` returns owned OS strings and `var` converts them to UTF-8. [`var_os_ref`] borrows
//! directly from an installed snapshot without allocating or falling back to the system.
//!
//! Caller-buffer initialization and borrowed reads require neither `alloc` nor `std`:
//!
//! ```
//! use libdd_common::environment;
//!
//! static mut BUFFER: [u8; 128] = [0; 128];
//! // SAFETY: startup calls this once and creates no other references to BUFFER.
//! let buffer = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };
//! environment::initialize_from_in([("DD_EXTERNAL_ENV", "startup-value")], buffer)?;
//! let value = environment::var_os_ref("DD_EXTERNAL_ENV").unwrap();
//! assert_eq!(value.as_encoded_bytes(), b"startup-value");
//! # Ok::<(), environment::BufferInitError>(())
//! ```

use crate::ffi::OsStr;
#[cfg(feature = "alloc")]
use crate::ffi::OsString;
#[cfg(feature = "alloc")]
use alloc::{boxed::Box, collections::BTreeMap, string::String};
use core::{
    fmt, ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

mod buffer;
#[cfg(any(not(feature = "std"), unix))]
pub use buffer::initialize_from_envp_in;
#[cfg(target_os = "linux")]
pub use buffer::initialize_in;
pub use buffer::{BufferInitError, initialize_from_in};

static ENVIRONMENT: ShadowEnvironment = ShadowEnvironment::new();

/// Failure to read an environment variable as UTF-8.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(feature = "alloc")]
pub enum VarError {
    /// No variable with this name is available.
    NotPresent,
    /// The value is not valid UTF-8; retains the original OS string.
    NotUnicode(OsString),
}

#[cfg(feature = "alloc")]
impl fmt::Display for VarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotPresent => "environment variable not found",
            Self::NotUnicode(_) => "environment variable is not valid UTF-8",
        })
    }
}

#[cfg(feature = "alloc")]
impl core::error::Error for VarError {}

/// Failure to initialize the shadow environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    AlreadyInitialized,
    InvalidName,
    InvalidValue,
    /// Caller-provided storage could not hold the snapshot.
    BufferTooSmall,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyInitialized => "shadow environment is already initialized",
            Self::InvalidName => "environment variable name is empty or contains '=' or NUL",
            Self::InvalidValue => "environment variable value contains NUL",
            Self::BufferTooSmall => "shadow environment buffer is too small",
        })
    }
}

impl core::error::Error for InitError {}

/// Read a variable from the shadow environment, or from the process environment in `std`
/// builds when no shadow has been installed. Missing snapshot entries never fall back.
#[cfg(feature = "alloc")]
pub fn var<K: AsRef<OsStr>>(name: K) -> Result<String, VarError> {
    ENVIRONMENT.var(name)
}

/// Read an owned OS-string value without requiring UTF-8, like `std::env::var_os`.
///
/// Uses the same snapshot and pre-initialization fallback rules as [`var`]. Invalid names
/// and absent variables return `None`; present empty values return an empty OS string.
#[cfg(feature = "alloc")]
pub fn var_os<K: AsRef<OsStr>>(name: K) -> Option<OsString> {
    ENVIRONMENT.var_os(name)
}

/// Borrow a value from the installed snapshot without allocating.
///
/// Returns `None` for invalid names, absent variables, or an uninitialized snapshot.
/// This never reads the system environment, including in `std` builds: live system values
/// cannot be safely borrowed. The returned value is immutable for the process lifetime.
pub fn var_os_ref<K: AsRef<OsStr>>(name: K) -> Option<&'static OsStr> {
    ENVIRONMENT.var_os_ref(name.as_ref())
}

/// Whether the process-wide shadow environment has been initialized.
pub fn is_initialized() -> bool {
    ENVIRONMENT.is_initialized()
}

/// Store caller-supplied OS-string variables in the shadow environment.
///
/// Takes ownership of owned strings and copies borrowed strings. Accepts UTF-8 strings and
/// arbitrary OS strings, including non-UTF-8 names and values. Duplicate names retain their
/// first value, matching `getenv` lookup order. An empty iterator installs an authoritative empty
/// snapshot. Initialization happens once; invalid entries leave the environment uninitialized.
#[cfg(feature = "alloc")]
pub fn initialize_from<I, K, V>(variables: I) -> Result<(), InitError>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    if is_initialized() {
        return Err(InitError::AlreadyInitialized);
    }
    ENVIRONMENT.install(Snapshot::from_variables(variables)?)
}

/// Copy all process variables into owned storage during startup.
///
/// Preserves non-UTF-8 values; [`var_os`] returns them and [`var`] reports
/// [`VarError::NotUnicode`]. Later changes to the process environment are not reflected in the
/// shadow.
///
/// # Safety
/// Call before other threads or native libraries can mutate the process environment. No
/// environment mutation may occur for the duration of the copy, including through C APIs.
#[cfg(all(feature = "alloc", any(feature = "std", target_os = "linux")))]
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
#[cfg(all(feature = "alloc", any(not(feature = "std"), unix)))]
pub unsafe fn initialize_from_envp(envp: *const *const core::ffi::c_char) -> Result<(), InitError> {
    if is_initialized() {
        return Err(InitError::AlreadyInitialized);
    }
    // SAFETY: forwarded from the caller's contract.
    ENVIRONMENT.install(unsafe { Snapshot::from_envp(envp) })
}

#[cfg(feature = "alloc")]
struct Snapshot {
    values: BTreeMap<OsString, OsString>,
}

#[cfg(feature = "alloc")]
impl Snapshot {
    fn from_variables<I, K, V>(variables: I) -> Result<Self, InitError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let (name, value) = (name.into(), value.into());
            if !valid_name(&name) {
                return Err(InitError::InvalidName);
            }
            if value.as_encoded_bytes().contains(&0) {
                return Err(InitError::InvalidValue);
            }
            values.entry(name).or_insert(value);
        }
        Ok(Self { values })
    }

    #[cfg(any(not(feature = "std"), unix))]
    unsafe fn from_envp(envp: *const *const core::ffi::c_char) -> Self {
        let mut values = BTreeMap::new();
        // SAFETY: the caller guarantees valid, immutable C strings during the copy.
        for (name, value) in unsafe { buffer::Envp::new(envp) } {
            values
                .entry(OsString::from(name))
                .or_insert_with(|| OsString::from(value));
        }
        Self { values }
    }
}

fn valid_name(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    !bytes.is_empty() && !bytes.contains(&b'=') && !bytes.contains(&0)
}

struct ShadowEnvironment {
    snapshot: AtomicPtr<()>,
    buffer_len: AtomicUsize,
    state: AtomicUsize,
}

const UNINITIALIZED: usize = 0;
const INITIALIZING: usize = 1;
const BUFFER: usize = 2;
#[cfg(feature = "alloc")]
const OWNED: usize = 3;

impl ShadowEnvironment {
    const fn new() -> Self {
        Self {
            snapshot: AtomicPtr::new(ptr::null_mut()),
            buffer_len: AtomicUsize::new(0),
            state: AtomicUsize::new(UNINITIALIZED),
        }
    }

    fn is_initialized(&self) -> bool {
        self.state.load(Ordering::Acquire) >= BUFFER
    }

    fn reserve(&self) -> Result<(), InitError> {
        match self.state.compare_exchange(
            UNINITIALIZED,
            INITIALIZING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(mut state) => {
                // A losing initializer must return only after the winning snapshot is ready.
                // Reservation covers only infallible atomic stores, never user code.
                while state == INITIALIZING {
                    core::hint::spin_loop();
                    state = self.state.load(Ordering::Acquire);
                }
                Err(InitError::AlreadyInitialized)
            }
        }
    }

    #[cfg(feature = "alloc")]
    fn install(&self, snapshot: Snapshot) -> Result<(), InitError> {
        let pointer = Box::into_raw(Box::new(snapshot));
        match self.reserve() {
            Ok(()) => {
                self.snapshot.store(pointer.cast(), Ordering::Relaxed);
                self.state.store(OWNED, Ordering::Release);
                Ok(())
            }
            Err(_) => {
                // SAFETY: this unpublished allocation is still exclusively owned by this call.
                unsafe {
                    drop(Box::from_raw(pointer));
                }
                Err(InitError::AlreadyInitialized)
            }
        }
    }

    fn install_buffer(
        &self,
        buffer: &'static mut [u8],
        used: usize,
    ) -> Result<(), BufferInitError> {
        if used > buffer.len() {
            return Err(BufferInitError {
                error: InitError::BufferTooSmall,
                buffer,
            });
        }
        if self.reserve().is_err() {
            return Err(BufferInitError {
                error: InitError::AlreadyInitialized,
                buffer,
            });
        }
        self.snapshot
            .store(buffer.as_mut_ptr().cast(), Ordering::Relaxed);
        self.buffer_len.store(used, Ordering::Relaxed);
        self.state.store(BUFFER, Ordering::Release);
        Ok(())
    }

    fn var_os_ref(&self, name: &OsStr) -> Option<&OsStr> {
        if !valid_name(name) {
            return None;
        }
        // The release/acquire state transition publishes both metadata and complete contents.
        match self.state.load(Ordering::Acquire) {
            BUFFER => {
                let pointer = self.snapshot.load(Ordering::Relaxed).cast::<u8>();
                let len = self.buffer_len.load(Ordering::Relaxed);
                // SAFETY: installation consumes exclusive process-lifetime storage, checks
                // its length, and never mutates or replaces it after publication.
                let bytes = unsafe { core::slice::from_raw_parts(pointer, len) };
                // SAFETY: buffer initialization copies complete native OS-string encodings.
                unsafe { buffer::find(bytes, name) }
            }
            #[cfg(feature = "alloc")]
            OWNED => {
                let pointer = self.snapshot.load(Ordering::Relaxed).cast::<Snapshot>();
                // SAFETY: the published owned snapshot is immutable and remains alive while
                // self is borrowed. Drop requires exclusive access to self.
                let snapshot = unsafe { &*pointer };
                snapshot.values.get(name).map(AsRef::<OsStr>::as_ref)
            }
            _ => None,
        }
    }

    #[cfg(feature = "alloc")]
    fn var(&self, name: impl AsRef<OsStr>) -> Result<String, VarError> {
        self.var_os(name)
            .ok_or(VarError::NotPresent)?
            .into_string()
            .map_err(VarError::NotUnicode)
    }

    #[cfg(feature = "alloc")]
    fn var_os(&self, name: impl AsRef<OsStr>) -> Option<OsString> {
        let name = name.as_ref();
        if !valid_name(name) {
            return None;
        }
        if self.is_initialized() {
            return self.var_os_ref(name).map(OsString::from);
        }
        #[cfg(feature = "std")]
        {
            std::env::var_os(name)
        }
        #[cfg(not(feature = "std"))]
        None
    }
}

#[cfg(feature = "alloc")]
impl Drop for ShadowEnvironment {
    fn drop(&mut self) {
        let pointer = *self.snapshot.get_mut();
        if *self.state.get_mut() == OWNED {
            // SAFETY: exclusive access ensures no readers remain; this owns the published box.
            unsafe {
                drop(Box::from_raw(pointer.cast::<Snapshot>()));
            }
        }
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;
    #[cfg(feature = "std")]
    use alloc::string::ToString;
    use alloc::vec;
    #[cfg(all(feature = "std", unix))]
    use std::os::unix::ffi::OsStringExt;

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
        assert_eq!(
            environment
                .var_os_ref("SERVICE".as_ref())
                .unwrap()
                .as_encoded_bytes(),
            b"checkout"
        );
        assert_eq!(environment.var("EMPTY").unwrap(), "");
        assert_eq!(environment.var_os("EMPTY"), Some(OsString::from("")));
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
        assert_eq!(environment.var_os("PATH"), None);
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
            assert_eq!(environment.var_os(name), None);
        }
    }

    #[cfg(any(not(feature = "std"), unix))]
    #[test]
    fn supplied_os_strings_preserve_non_unicode_names_and_values() {
        let key = OsString::from_vec(vec![b'K', 0xff]);
        let value = OsString::from_vec(vec![0xfe, b'=', b'v']);
        let snapshot = Snapshot::from_variables([
            (key.clone(), value.clone()),
            (key.clone(), OsString::from("ignored")),
        ])
        .unwrap();
        let environment = ShadowEnvironment::new();
        environment.install(snapshot).unwrap();

        assert_eq!(environment.var_os(&key), Some(value.clone()));
        assert_eq!(
            environment.var(&key),
            Err(VarError::NotUnicode(value.clone()))
        );
        let borrowed_key: &OsStr = key.as_ref();
        let borrowed_value: &OsStr = value.as_ref();
        let borrowed = Snapshot::from_variables([(borrowed_key, borrowed_value)]).unwrap();
        assert_eq!(borrowed.values.get(borrowed_key), Some(&value));
    }

    #[cfg(all(feature = "std", windows))]
    #[test]
    fn supplied_native_strings_preserve_unpaired_utf16_surrogates() {
        use std::os::windows::ffi::OsStringExt;

        let key = OsString::from_wide(&[u16::from(b'K'), 0xd800]);
        let value = OsString::from_wide(&[0xdc00]);
        let environment = ShadowEnvironment::new();
        environment
            .install(Snapshot::from_variables([(key.clone(), value.clone())]).unwrap())
            .unwrap();
        assert_eq!(environment.var_os(&key), Some(value.clone()));
        assert_eq!(environment.var(&key), Err(VarError::NotUnicode(value)));
    }

    #[cfg(feature = "std")]
    #[test]
    fn uninitialized_std_environment_uses_system_values() {
        let environment = ShadowEnvironment::new();
        let expected = std::env::var("PATH").map_err(|error| match error {
            std::env::VarError::NotPresent => VarError::NotPresent,
            std::env::VarError::NotUnicode(value) => VarError::NotUnicode(value),
        });
        assert_eq!(environment.var("PATH"), expected);
        let name: &OsStr = "PATH".as_ref();
        assert_eq!(environment.var_os(name), std::env::var_os("PATH"));
        assert!(!environment.is_initialized());
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn uninitialized_no_std_environment_has_no_system_fallback() {
        let environment = ShadowEnvironment::new();
        assert_eq!(environment.var("PATH"), Err(VarError::NotPresent));
        assert_eq!(environment.var_os("PATH"), None);
        assert!(!environment.is_initialized());
    }

    #[cfg(any(not(feature = "std"), unix))]
    #[test]
    fn c_environment_is_copied_and_preserves_non_unicode_values() {
        use alloc::{ffi::CString, vec::Vec};

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
        let raw = OsString::from_vec(alloc::vec![0xff]);
        assert_eq!(environment.var_os("RAW"), Some(raw.clone()));
        assert_eq!(environment.var("RAW"), Err(VarError::NotUnicode(raw)));
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

    #[cfg(feature = "std")]
    #[test]
    fn competing_storage_modes_return_with_a_readable_snapshot() {
        static mut DATA: [u8; 13] = *b"VALUE=buffer\0";
        let environment = ShadowEnvironment::new();
        let barrier = std::sync::Barrier::new(2);
        // SAFETY: this test consumes its unique static storage once.
        let buffer = unsafe { &mut *core::ptr::addr_of_mut!(DATA) };
        std::thread::scope(|scope| {
            let buffered = scope.spawn(|| {
                barrier.wait();
                let result = environment
                    .install_buffer(buffer, 13)
                    .map_err(|failure| failure.error);
                assert!(environment.is_initialized());
                assert!(environment.var_os_ref("VALUE".as_ref()).is_some());
                result
            });
            let owned = scope.spawn(|| {
                let snapshot = Snapshot::from_variables([("VALUE", "owned")]).unwrap();
                barrier.wait();
                let result = environment.install(snapshot);
                assert!(environment.is_initialized());
                assert!(environment.var_os_ref("VALUE".as_ref()).is_some());
                result
            });
            let results = [buffered.join().unwrap(), owned.join().unwrap()];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert!(results.contains(&Err(InitError::AlreadyInitialized)));
        });
    }
}

#[cfg(test)]
mod buffer_storage_tests {
    use super::*;

    #[test]
    fn empty_buffer_is_initialized_and_authoritative() {
        static mut EMPTY: [u8; 0] = [];
        let environment = ShadowEnvironment::new();
        assert!(!environment.is_initialized());
        assert!(environment.var_os_ref("PATH".as_ref()).is_none());
        // SAFETY: this test consumes its unique static storage once.
        let buffer = unsafe { &mut *core::ptr::addr_of_mut!(EMPTY) };
        environment.install_buffer(buffer, 0).unwrap();
        assert!(environment.is_initialized());
        assert!(environment.var_os_ref("PATH".as_ref()).is_none());
        #[cfg(feature = "alloc")]
        assert_eq!(environment.var_os("PATH"), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn publication_makes_buffer_and_length_visible_to_readers() {
        static mut DATA: [u8; 4] = *b"K=v\0";
        let environment = ShadowEnvironment::new();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let environment = &environment;
                scope.spawn(move || {
                    while !environment.is_initialized() {
                        std::thread::yield_now();
                    }
                    assert_eq!(
                        environment
                            .var_os_ref("K".as_ref())
                            .unwrap()
                            .as_encoded_bytes(),
                        b"v"
                    );
                });
            }
            // SAFETY: this test consumes its unique static storage once.
            let buffer = unsafe { &mut *core::ptr::addr_of_mut!(DATA) };
            environment.install_buffer(buffer, 4).unwrap();
        });
    }
}
