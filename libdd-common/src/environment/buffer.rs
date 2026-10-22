// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{ENVIRONMENT, InitError, valid_name};
use crate::ffi::OsStr;
use core::fmt;
#[cfg(any(not(feature = "std"), unix))]
use core::{ffi::CStr, ffi::c_char, marker::PhantomData, ptr};
#[cfg(all(feature = "std", unix))]
use std::os::unix::ffi::OsStrExt;

/// Initialization failed without publishing the buffer. The caller retains its storage.
pub struct BufferInitError {
    /// Why the snapshot was not installed.
    pub error: InitError,
    /// The original storage, which was not retained and can be reused.
    pub buffer: &'static mut [u8],
}

impl fmt::Debug for BufferInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferInitError")
            .field("error", &self.error)
            .field("buffer_len", &self.buffer.len())
            .finish()
    }
}

impl fmt::Display for BufferInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl core::error::Error for BufferInitError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Initialize the shadow environment using process-lifetime caller storage.
///
/// Methods consume this adapter. Successful initialization retains the buffer for the
/// process lifetime; errors return it for reuse. Call [`Self::into_buffer`] to recover
/// the storage without initializing.
#[must_use = "initialize the shadow environment or recover the storage with into_buffer"]
pub struct BufferedInitializer {
    buffer: &'static mut [u8],
}

/// Choose caller-provided storage for allocation-free shadow initialization.
///
/// Available with or without `alloc` and `std`. Supply typed pairs with
/// [`BufferedInitializer::initialize_from`] or a stable C environment array with
/// `BufferedInitializer::initialize_from_envp`.
pub fn with_buffer(buffer: &'static mut [u8]) -> BufferedInitializer {
    BufferedInitializer { buffer }
}

impl BufferedInitializer {
    /// Recover the caller's storage without initializing the shadow environment.
    pub fn into_buffer(self) -> &'static mut [u8] {
        self.buffer
    }

    /// Copy variables into caller storage without allocating.
    ///
    /// Entries are packed as `name=value\0`, and lookups scan the packed entries. Duplicate
    /// names keep their first value. Names and values obey the same validation rules as
    /// `initialize_from`. An empty iterator installs an authoritative empty snapshot.
    ///
    /// On success, the buffer is held for the process lifetime. On failure, no snapshot is
    /// published and the error returns the buffer; its contents may have been overwritten.
    pub fn initialize_from<I, K, V>(self, variables: I) -> Result<(), BufferInitError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let buffer = self.buffer;
        if ENVIRONMENT.is_initialized() {
            return Err(BufferInitError {
                error: InitError::AlreadyInitialized,
                buffer,
            });
        }
        let result = {
            let mut writer = Writer::new(buffer);
            variables
                .into_iter()
                .try_for_each(|(name, value)| writer.push(name.as_ref(), value.as_ref()))
                .map(|()| writer.used)
        };
        match result {
            Ok(used) => ENVIRONMENT.install_buffer(buffer, used),
            Err(error) => Err(BufferInitError { error, buffer }),
        }
    }

    /// Copy a C environment array into caller storage without allocating.
    ///
    /// A null `envp` installs an empty snapshot. Malformed entries are skipped and duplicate
    /// names retain their first value. Buffer ownership and errors match [`Self::initialize_from`].
    /// Available with `std` on Unix, and without `std` on any supported target.
    ///
    /// ```
    /// use libdd_common::environment;
    ///
    /// static mut BUFFER: [u8; 64] = [0; 64];
    /// let envp = [c"DD_EXTERNAL_ENV=startup".as_ptr(), core::ptr::null()];
    /// // SAFETY: this startup code creates the only reference to BUFFER.
    /// let buffer = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };
    /// // SAFETY: envp is terminated, and its C strings remain valid during the copy.
    /// unsafe { environment::with_buffer(buffer).initialize_from_envp(envp.as_ptr()) }?;
    /// assert_eq!(
    ///     environment::var_os_ref("DD_EXTERNAL_ENV")
    ///         .unwrap()
    ///         .as_encoded_bytes(),
    ///     b"startup"
    /// );
    /// # Ok::<(), environment::BufferInitError>(())
    /// ```
    ///
    /// # Safety
    /// The array and all its NUL-terminated strings must be valid and immutable throughout
    /// this call, and the array must end in a null pointer. No source pointers are retained.
    #[cfg(any(not(feature = "std"), unix))]
    pub unsafe fn initialize_from_envp(
        self,
        envp: *const *const c_char,
    ) -> Result<(), BufferInitError> {
        // SAFETY: the caller guarantees the array remains valid during the copy.
        self.initialize_from(unsafe { Envp::new(envp) })
    }
}

struct Writer<'a> {
    buffer: &'a mut [u8],
    used: usize,
}

impl<'a> Writer<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, used: 0 }
    }

    fn push(&mut self, name: &OsStr, value: &OsStr) -> Result<(), InitError> {
        if !valid_name(name) {
            return Err(InitError::InvalidName);
        }
        let value = value.as_encoded_bytes();
        if value.contains(&0) {
            return Err(InitError::InvalidValue);
        }
        if find_bytes(&self.buffer[..self.used], name).is_some() {
            return Ok(());
        }
        let name = name.as_encoded_bytes();
        let end = self
            .used
            .checked_add(name.len())
            .and_then(|size| size.checked_add(value.len()))
            .and_then(|size| size.checked_add(2))
            .filter(|size| *size <= self.buffer.len())
            .ok_or(InitError::BufferTooSmall)?;
        // The checked total includes both strings, '=' and NUL, so every range fits.
        let name_end = self.used + name.len();
        self.buffer[self.used..name_end].copy_from_slice(name);
        self.buffer[name_end] = b'=';
        self.buffer[name_end + 1..end - 1].copy_from_slice(value);
        self.buffer[end - 1] = 0;
        self.used = end;
        Ok(())
    }
}

fn find_bytes<'a>(buffer: &'a [u8], name: &OsStr) -> Option<&'a [u8]> {
    buffer.split(|byte| *byte == 0).find_map(|entry| {
        let separator = entry.iter().position(|byte| *byte == b'=')?;
        (entry[..separator] == *name.as_encoded_bytes()).then_some(&entry[separator + 1..])
    })
}

/// # Safety
/// Entries must contain complete native OS-string encodings copied by `Writer`.
pub(super) unsafe fn find<'a>(buffer: &'a [u8], name: &OsStr) -> Option<&'a OsStr> {
    let value = find_bytes(buffer, name)?;
    #[cfg(feature = "std")]
    {
        // SAFETY: whole OS strings were copied without modifying their encoding. '=' and
        // NUL delimiters are ASCII boundaries, so the returned value preserves that encoding.
        Some(unsafe { OsStr::from_encoded_bytes_unchecked(value) })
    }
    #[cfg(not(feature = "std"))]
    Some(OsStr::from_bytes(value))
}

#[cfg(any(not(feature = "std"), unix))]
pub(super) struct Envp<'a> {
    next: *const *const c_char,
    lifetime: PhantomData<&'a OsStr>,
}

#[cfg(any(not(feature = "std"), unix))]
impl Envp<'_> {
    /// # Safety
    /// The array and its C strings must remain valid and immutable for this iterator's lifetime.
    pub(super) unsafe fn new(next: *const *const c_char) -> Self {
        Self {
            next,
            lifetime: PhantomData,
        }
    }
}

#[cfg(any(not(feature = "std"), unix))]
impl<'a> Iterator for Envp<'a> {
    type Item = (&'a OsStr, &'a OsStr);

    fn next(&mut self) -> Option<Self::Item> {
        while !self.next.is_null() {
            // SAFETY: the constructor requires a readable null-terminated pointer array.
            let entry = unsafe { *self.next };
            if entry.is_null() {
                self.next = ptr::null();
                return None;
            }
            // SAFETY: the next slot exists up to and including the array's terminator.
            self.next = unsafe { self.next.add(1) };
            // SAFETY: the constructor requires valid, immutable C strings for this lifetime.
            let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
            if let Some(separator) = bytes.iter().position(|byte| *byte == b'=') {
                if separator != 0 {
                    return Some((
                        OsStr::from_bytes(&bytes[..separator]),
                        OsStr::from_bytes(&bytes[separator + 1..]),
                    ));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_capacity_preserves_empty_values_equals_and_first_duplicates() {
        let mut buffer = [0u8; 13];
        let mut writer = Writer::new(&mut buffer);
        for (key, value) in [("K", "v"), ("K", "ignored"), ("E", ""), ("Q", "a=b")] {
            writer.push(key.as_ref(), value.as_ref()).unwrap();
        }
        assert_eq!(writer.used, 13);
        assert_eq!(
            find_bytes(writer.buffer, "K".as_ref()),
            Some(b"v".as_slice())
        );
        assert_eq!(
            find_bytes(writer.buffer, "E".as_ref()),
            Some(b"".as_slice())
        );
        assert_eq!(
            find_bytes(writer.buffer, "Q".as_ref()),
            Some(b"a=b".as_slice())
        );
        assert_eq!(find_bytes(writer.buffer, "MISSING".as_ref()), None);
    }

    #[test]
    fn insufficient_capacity_does_not_write_a_partial_entry() {
        let mut bytes = [0xaau8; 3];
        let mut writer = Writer::new(&mut bytes);
        assert_eq!(
            writer.push("K".as_ref(), "v".as_ref()),
            Err(InitError::BufferTooSmall)
        );
        assert_eq!(writer.used, 0);
        assert_eq!(bytes, [0xaa; 3]);
    }

    #[test]
    fn invalid_entries_are_rejected() {
        let mut bytes = [0u8; 32];
        let mut writer = Writer::new(&mut bytes);
        for name in ["", "A=B", "A\0B"] {
            assert_eq!(
                writer.push(name.as_ref(), "v".as_ref()),
                Err(InitError::InvalidName)
            );
        }
        assert_eq!(
            writer.push("K".as_ref(), "v\0x".as_ref()),
            Err(InitError::InvalidValue)
        );
        assert_eq!(writer.used, 0);
    }

    #[cfg(any(not(feature = "std"), unix))]
    #[test]
    fn raw_envp_and_non_unicode_values_are_copied_losslessly() {
        let entries = [
            c"malformed".as_ptr(),
            c"=invalid".as_ptr(),
            c"K=first".as_ptr(),
            c"K=ignored".as_ptr(),
            c"RAW=\xff".as_ptr(),
            ptr::null(),
        ];
        let mut bytes = [0u8; 32];
        let mut writer = Writer::new(&mut bytes);
        // SAFETY: static C strings and the terminated pointer array live throughout iteration.
        for (key, value) in unsafe { Envp::new(entries.as_ptr()) } {
            writer.push(key, value).unwrap();
        }
        let packed = &writer.buffer[..writer.used];
        // SAFETY: Writer copied complete native OS-string encodings.
        let value = unsafe { find(packed, "RAW".as_ref()) }.unwrap();
        assert_eq!(value.as_encoded_bytes(), &[0xff]);
        assert_eq!(find_bytes(packed, "K".as_ref()), Some(b"first".as_slice()));
        // SAFETY: null is an empty environment.
        assert!(unsafe { Envp::new(ptr::null()) }.next().is_none());
    }
}
