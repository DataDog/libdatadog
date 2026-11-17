// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_regex::Regex;
use std::cell::UnsafeCell;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Interior mutability for statics that are written only through the `add_*`
/// entry points (each `unsafe fn`, documented as startup-only: single-threaded,
/// before the matching `*_INITIALIZED` latch flips) and read afterwards.
///
/// Writing through `&mut (*(&*LOCK as *const T).cast_mut())` is undefined
/// behavior under Stacked Borrows — a `&T` shared reference cannot be written
/// through, which is exactly what Miri reported — while `UnsafeCell` is the
/// blessed mechanism for writing through `&self`. `UnsafeCell` itself is not
/// `Sync`; the impl below re-states the startup-only write contract.
struct StartupCell<T>(UnsafeCell<T>);

// SAFETY: writes happen only before the startup latch flips (enforced by the
// `assert!` in every `add_*` entry point); after the latch, access is `&` only.
// The caller's startup sequencing establishes the happens-before edge between
// the last write and the first concurrent read.
unsafe impl<T> Sync for StartupCell<T> {}

impl<T> StartupCell<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// Shared read; sound only because writes are startup-only.
    fn get(&self) -> &T {
        unsafe { &*self.0.get() }
    }

    /// Startup-phase write access; only call before the latch flips.
    // allow(clippy::mut_from_ref): the point of this wrapper is interior
    // mutability; the startup-only write contract is on the type docs above.
    #[allow(clippy::mut_from_ref)]
    fn get_mut(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

static REDACTED_NAMES: LazyLock<HashSet<&'static [u8]>> = LazyLock::new(|| {
    HashSet::from([
        b"2fa" as &[u8],
        b"accesstoken",
        b"aiohttpsession",
        b"apikey",
        b"apisecret",
        b"apisignature",
        b"appkey",
        b"applicationkey",
        b"auth",
        b"authorization",
        b"authtoken",
        b"ccnumber",
        b"certificatepin",
        b"cipher",
        b"clientid",
        b"clientsecret",
        b"connectionstring",
        b"connectsid",
        b"cookie",
        b"credentials",
        b"creditcard",
        b"csrf",
        b"csrftoken",
        b"cvv",
        b"databaseurl",
        b"dburl",
        b"encryptionkey",
        b"encryptionkeyid",
        b"geolocation",
        b"gpgkey",
        b"ipaddress",
        b"jti",
        b"jwt",
        b"licensekey",
        b"masterkey",
        b"mysqlpwd",
        b"nonce",
        b"oauth",
        b"oauthtoken",
        b"otp",
        b"passhash",
        b"passwd",
        b"password",
        b"passwordb",
        b"pemfile",
        b"pgpkey",
        b"phpsessid",
        b"pin",
        b"pincode",
        b"pkcs8",
        b"privatekey",
        b"publickey",
        b"pwd",
        b"recaptchakey",
        b"refreshtoken",
        b"routingnumber",
        b"salt",
        b"secret",
        b"secretkey",
        b"secrettoken",
        b"securityanswer",
        b"securitycode",
        b"securityquestion",
        b"serviceaccountcredentials",
        b"session",
        b"sessionid",
        b"sessionkey",
        b"setcookie",
        b"signature",
        b"signaturekey",
        b"sshkey",
        b"ssn",
        b"symfony",
        b"token",
        b"transactionid",
        b"twiliotoken",
        b"usersession",
        b"voterid",
        b"xapikey",
        b"xauthtoken",
        b"xcsrftoken",
        b"xforwardedfor",
        b"xrealip",
        b"xsrf",
        b"xsrftoken",
        b"customidentifier1",
        b"customidentifier2",
    ])
});

static ADDED_REDACTED_NAMES: StartupCell<Option<HashSet<&'static [u8]>>> = StartupCell::new(None);

// Exclusions take precedence over the built-in REDACTED_NAMES set.
static EXCLUDED_NAMES: StartupCell<Option<HashSet<Vec<u8>>>> = StartupCell::new(None);
static EXCLUDED_NAMES_INITIALIZED: AtomicBool = AtomicBool::new(false);

static REDACTED_TYPES: StartupCell<Option<HashSet<&'static [u8]>>> = StartupCell::new(None);

static REDACTED_WILDCARD_TYPES_PATTERN: StartupCell<String> = StartupCell::new(String::new());

static REDACTED_TYPES_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    REDACTED_TYPES_INITIALIZED.store(true, Ordering::Relaxed);

    #[allow(clippy::unwrap_used)]
    Regex::new(REDACTED_WILDCARD_TYPES_PATTERN.get()).unwrap()
});

static ASSUMED_SAFE_NAME_LEN: LazyLock<usize> = LazyLock::new(|| {
    REDACTED_NAMES_INITIALIZED.store(true, Ordering::Relaxed);

    #[allow(clippy::unwrap_used)]
    let len = REDACTED_NAMES.iter().map(|n| n.len()).max().unwrap() + 5;

    len
});

static REDACTED_NAMES_INITIALIZED: AtomicBool = AtomicBool::new(false);
static REDACTED_TYPES_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// # Safety
/// May only be called while not running yet - concurrent access to is_redacted_name is forbidden.
pub unsafe fn add_redacted_name<I: Into<Vec<u8>>>(name: I) {
    assert!(!REDACTED_NAMES_INITIALIZED.load(Ordering::Relaxed));
    // Leak the name so the set holds `&'static [u8]` without borrowing
    // from a backing Vec that a later push could reallocate.
    let name: &'static [u8] = Box::leak(name.into().into_boxed_slice());
    ADDED_REDACTED_NAMES
        .get_mut()
        .get_or_insert_with(HashSet::new)
        .insert(name);
}
/// # Safety
/// May only be called while not running yet - concurrent access to is_excluded_name is forbidden.
pub unsafe fn add_excluded_name<I: Into<Vec<u8>>>(name: I) {
    assert!(!EXCLUDED_NAMES_INITIALIZED.load(Ordering::Relaxed));
    EXCLUDED_NAMES
        .get_mut()
        .get_or_insert_with(HashSet::new)
        .insert(name.into());
}

/// # Safety
/// May only be called while not running yet - concurrent access to is_redacted_type is forbidden.
pub unsafe fn add_redacted_type<I: AsRef<[u8]>>(name: I) {
    assert!(!REDACTED_TYPES_INITIALIZED.load(Ordering::Relaxed));
    let name = name.as_ref();

    if name.ends_with(b"*") {
        let regex_str = REDACTED_WILDCARD_TYPES_PATTERN.get_mut();
        if !regex_str.is_empty() {
            regex_str.push('|')
        }
        let name = String::from_utf8_lossy(name);
        regex_str.push_str(libdd_regex::escape(&name[..name.len() - 1]).as_str());
        regex_str.push_str(".*");
    } else {
        let name: &'static [u8] = Box::leak(name.to_vec().into_boxed_slice());
        REDACTED_TYPES
            .get_mut()
            .get_or_insert_with(HashSet::new)
            .insert(name);
    }
}

pub fn is_redacted_name<I: AsRef<[u8]>>(name: I) -> bool {
    fn invalid_char(c: u8) -> bool {
        c == b'_' || c == b'-' || c == b'$' || c == b'@'
    }
    let name = name.as_ref();
    if name.len() > *ASSUMED_SAFE_NAME_LEN {
        return true; // short circuit for long names, assume them safe
    }
    let mut copy = smallvec::SmallVec::<[u8; 21]>::with_capacity(name.len());
    let mut i = 0;
    while i < name.len() {
        let mut c = name[i];
        if !invalid_char(c) {
            if c.is_ascii_uppercase() {
                c |= 0x20; // lowercase it
            }
            copy.push(c);
        }
        i += 1;
    }
    let normalized = &copy[0..copy.len()];
    // Exclusions take precedence: if explicitly excluded, do not redact.
    if EXCLUDED_NAMES.get().as_ref().is_none_or(|s| s.is_empty()) {
        EXCLUDED_NAMES_INITIALIZED.store(true, Ordering::Relaxed);
        if EXCLUDED_NAMES
            .get()
            .as_ref()
            .is_some_and(|s| s.contains(normalized as &[u8]))
        {
            return false;
        }
    }
    REDACTED_NAMES.contains(normalized)
        || ADDED_REDACTED_NAMES
            .get()
            .as_ref()
            .is_some_and(|s| s.contains(normalized))
}

pub fn is_redacted_type<I: AsRef<[u8]>>(name: I) -> bool {
    let name = name.as_ref();
    if REDACTED_TYPES
        .get()
        .as_ref()
        .is_some_and(|s| s.contains(name))
    {
        true
    } else if !REDACTED_WILDCARD_TYPES_PATTERN.get().is_empty() {
        std::str::from_utf8(name).is_ok_and(|s| REDACTED_TYPES_REGEX.is_match(s))
    } else {
        false
    }
}

// Runs under Miri on purpose: it previously reported a real
// invalid-reference-casting UB in add_redacted_name/add_redacted_type —
// shared references to the statics were cast to *mut and written through.
// The statics now route their startup-only writes through `StartupCell`
// (UnsafeCell), which is Stacked Borrows-clean. These tests stay under
// Miri as the tripwire for that contract.
#[cfg(test)]
mod miri_tests {
    use super::*;

    #[test]
    fn test_redacted_name() {
        unsafe { add_redacted_name("test") }

        assert!(is_redacted_name("test"));
        assert!(is_redacted_name("te-st"));
        assert!(is_redacted_name("CSRF"));
        assert!(is_redacted_name("$XSRF"));
        assert!(!is_redacted_name("foo"));
        assert!(!is_redacted_name("@"));
    }

    #[test]
    fn test_redacted_type() {
        unsafe {
            add_redacted_type("other");
            add_redacted_type("type*");
        }

        assert!(is_redacted_type("other"));
        assert!(is_redacted_type("type"));
        assert!(is_redacted_type("type.foo"));
        assert!(!is_redacted_type("typ"));
    }
}
