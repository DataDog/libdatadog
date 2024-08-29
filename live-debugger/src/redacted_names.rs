// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(invalid_reference_casting)]

use lazy_static::lazy_static;
use regex_automata::dfa::regex::Regex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

lazy_static! {
    static ref REDACTED_NAMES: HashSet<&'static [u8]> = HashSet::from([
        b"2fa" as &[u8],
        b"accesstoken",
        b"aiohttpsession",
        b"apikey",
        b"apisecret",
        b"apisignature",
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
        b"env",
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
    ]);
    static ref ADDED_REDACTED_NAMES: Vec<Vec<u8>> = vec![];
    static ref REDACTED_TYPES: HashSet<&'static [u8]> = HashSet::new();
    static ref ADDED_REDACTED_TYPES: Vec<Vec<u8>> = vec![];
    static ref REDACTED_WILDCARD_TYPES_PATTERN: String = "".to_string();
    static ref REDACTED_TYPES_REGEX: Regex = {
        REDACTED_TYPES_INITIALIZED.store(true, Ordering::Relaxed);
        Regex::new(&REDACTED_WILDCARD_TYPES_PATTERN).unwrap()
    };
    static ref ASSUMED_SAFE_NAME_LEN: usize = {
        REDACTED_NAMES_INITIALIZED.store(true, Ordering::Relaxed);
        REDACTED_NAMES.iter().map(|n| n.len()).max().unwrap() + 5
    };
}

static REDACTED_NAMES_INITIALIZED: AtomicBool = AtomicBool::new(false);
static REDACTED_TYPES_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// # Safety
/// May only be called while not running yet - concurrent access to is_redacted_name is forbidden.
pub unsafe fn add_redacted_name<I: Into<Vec<u8>>>(name: I) {
    assert!(!REDACTED_NAMES_INITIALIZED.load(Ordering::Relaxed));
    // I really don't want to Mutex this often checked value.
    // Hence, unsafe, and caller has to ensure safety.
    // An UnsafeCell would be perfect, but it isn't Sync...
    (*(&*ADDED_REDACTED_NAMES as *const Vec<Vec<u8>>).cast_mut()).push(name.into());
    (*(&*REDACTED_NAMES as *const HashSet<&'static [u8]>).cast_mut())
        .insert(&ADDED_REDACTED_NAMES[ADDED_REDACTED_NAMES.len() - 1]);
}

/// # Safety
/// May only be called while not running yet - concurrent access to is_redacted_type is forbidden.
pub unsafe fn add_redacted_type<I: AsRef<[u8]>>(name: I) {
    assert!(!REDACTED_TYPES_INITIALIZED.load(Ordering::Relaxed));
    let name = name.as_ref();
    if name.ends_with(b"*") {
        let regex_str = &mut *(&*REDACTED_WILDCARD_TYPES_PATTERN as *const String).cast_mut();
        if !regex_str.is_empty() {
            regex_str.push('|')
        }
        let name = String::from_utf8_lossy(name);
        regex_str.push_str(regex::escape(&name[..name.len() - 1]).as_str());
        regex_str.push_str(".*");
    } else {
        (*(&*ADDED_REDACTED_TYPES as *const Vec<Vec<u8>>).cast_mut()).push(name.to_vec());
        (*(&*REDACTED_TYPES as *const HashSet<&'static [u8]>).cast_mut())
            .insert(&ADDED_REDACTED_TYPES[ADDED_REDACTED_TYPES.len() - 1]);
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
    REDACTED_NAMES.contains(&copy[0..copy.len()])
}

pub fn is_redacted_type<I: AsRef<[u8]>>(name: I) -> bool {
    let name = name.as_ref();
    if REDACTED_TYPES.contains(name) {
        true
    } else if !REDACTED_WILDCARD_TYPES_PATTERN.is_empty() {
        REDACTED_TYPES_REGEX.is_match(name)
    } else {
        false
    }
}

#[test]
fn test_redacted_name() {
    unsafe {
        add_redacted_name("test")
    }

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
