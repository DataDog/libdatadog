#![allow(invalid_reference_casting)]

use lazy_static::lazy_static;
use regex_automata::dfa::regex::Regex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

lazy_static! {
    static ref REDACTED_NAMES: HashSet<&'static [u8]> = HashSet::from([
        b"password" as &[u8],
        b"passwd",
        b"secret",
        b"apikey",
        b"auth",
        b"credentials",
        b"mysqlpwd",
        b"privatekey",
        b"token",
        b"ipaddress",
        b"session", // django, Sanic
        b"csrftoken",
        b"sessionid", // wsgi
        b"remoteaddr",
        b"xcsrftoken",
        b"xforwardedfor",
        b"setcookie",
        b"cookie",
        b"authorization",
        b"xapikey",
        b"xforwardedfor",
        b"xrealip",
        b"aiohttpsession", // aiohttp
        b"connect.sid", // Express
        b"csrftoken", // Pyramid, Bottle
        b"csrf", // Express
        b"phpsessid", // PHP
        b"symfony", // Symfony
        b"usersession", // Vue
        b"xsrf", // Tornado
        b"xsrftoken", // Angular, Laravel
        b"salt",
        b"passwordb",
        b"secretkey",
        b"cipher",
        b"credentials",
        b"pkcs8",
        b"ssn",
        b"ccnumber",
        b"creditcard",
        b"cvv",
        b"pin",
        b"encryptionkey",
        b"sshkey",
        b"pgpkey",
        b"gpgkey",
        b"securityquestion",
        b"securityanswer",
        b"phonenumber",
        b"address",
        b"email",
        b"2fa",
        b"oauth",
        b"uuid",
        b"accesstoken",
        b"refreshtoken",
        b"jti",
        b"config",
        b"dburl",
        b"pemfile",
        b"clientsecret",
        b"env",
        b"licensekey",
        b"twiliotoken",
        b"recaptchakey",
        b"geolocation",
        b"signature",
        b"xauthtoken",
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
pub unsafe fn add_redacted_type(name: &[u8]) {
    assert!(!REDACTED_TYPES_INITIALIZED.load(Ordering::Relaxed));
    if name.ends_with(&[b'*']) {
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

pub fn is_redacted_name<'a, I: Into<&'a [u8]>>(name: I) -> bool {
    fn invalid_char(c: u8) -> bool {
        c == b'_' || c == b'-' || c == b'$' || c == b'@'
    }
    let name = name.into();
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

pub fn is_redacted_type<'a, I: Into<&'a [u8]>>(name: I) -> bool {
    let name = name.into();
    if REDACTED_TYPES.contains(name) {
        true
    } else if !REDACTED_WILDCARD_TYPES_PATTERN.is_empty() {
        REDACTED_TYPES_REGEX.is_match(name)
    } else {
        false
    }
}
