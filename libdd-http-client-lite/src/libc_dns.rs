// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Optional blocking DNS through libc resolver functions.
//!
//! This module is enabled by the `libc_dns` feature. libc DNS may allocate,
//! lock, read files, and use process-global state, so it is not
//! async-signal-safe.

use core::{fmt, net::IpAddr};

use embedded_nal_async::AddrType;

use crate::dns::Resolver as GenericResolver;

/// Error returned by [`Resolver`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The libc resolver is unavailable on this target.
    Unavailable,
    /// The hostname exceeds the fixed DNS name buffer.
    NameTooLong,
    /// The hostname contains a NUL byte and cannot be passed to libc.
    InteriorNul,
    /// `getaddrinfo` returned an error code.
    LookupFailed(i32),
    /// `getaddrinfo` returned no supported IP addresses.
    NotFound,
    /// The resolved IP address did not match the requested address type.
    AddressTypeUnavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("libc DNS resolver is unavailable"),
            Self::NameTooLong => formatter.write_str("DNS hostname is too long"),
            Self::InteriorNul => formatter.write_str("DNS hostname contains a NUL byte"),
            Self::LookupFailed(code) => {
                write!(formatter, "libc DNS lookup failed with code {code}")
            }
            Self::NotFound => formatter.write_str("libc DNS lookup returned no IP address"),
            Self::AddressTypeUnavailable => {
                formatter.write_str("DNS address does not match the requested type")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Blocking DNS resolver backed by the platform libc.
#[derive(Clone, Copy, Debug, Default)]
pub struct Resolver;

impl GenericResolver for Resolver {
    type Error = Error;

    fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, Self::Error> {
        if let Ok(address) = host.parse() {
            return require_address_type(address, addr_type);
        }
        platform::resolve(host, &addr_type)
    }
}

const fn require_address_type(address: IpAddr, addr_type: AddrType) -> Result<IpAddr, Error> {
    match (address, addr_type) {
        (IpAddr::V4(_), AddrType::IPv6) | (IpAddr::V6(_), AddrType::IPv4) => {
            Err(Error::AddressTypeUnavailable)
        }
        _ => Ok(address),
    }
}

#[cfg(unix)]
mod platform {
    use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use embedded_nal_async::AddrType;

    use super::Error;

    const HOSTNAME_CAPACITY: usize = 256;

    pub fn resolve(host: &str, addr_type: &AddrType) -> Result<IpAddr, Error> {
        if host.as_bytes().contains(&0) {
            return Err(Error::InteriorNul);
        }

        let mut hostname = [0 as libc::c_char; HOSTNAME_CAPACITY];
        if host.len() >= hostname.len() {
            return Err(Error::NameTooLong);
        }
        for (output, input) in hostname.iter_mut().zip(host.bytes()) {
            *output = libc::c_char::from_ne_bytes([input]);
        }

        resolve_host(hostname.as_ptr(), addr_type)
    }

    fn resolve_host(hostname: *const libc::c_char, addr_type: &AddrType) -> Result<IpAddr, Error> {
        // SAFETY: libc documents an all-zero addrinfo value as valid hints storage.
        let mut hints: libc::addrinfo = unsafe { core::mem::zeroed() };
        hints.ai_family = match *addr_type {
            AddrType::IPv4 => libc::AF_INET,
            AddrType::IPv6 => libc::AF_INET6,
            AddrType::Either => libc::AF_UNSPEC,
        };
        hints.ai_socktype = libc::SOCK_STREAM;

        let mut result = core::ptr::null_mut();
        // SAFETY: hostname is NUL-terminated and both output pointers are valid.
        let status = unsafe {
            libc::getaddrinfo(
                hostname,
                core::ptr::null(),
                &raw const hints,
                &raw mut result,
            )
        };
        if status != 0 {
            return Err(Error::LookupFailed(status));
        }
        let results = AddrInfoList(result);
        let mut current = results.0;

        while !current.is_null() {
            // SAFETY: getaddrinfo returns a linked list valid until freeaddrinfo.
            let entry = unsafe { &*current };
            if let Some(address) = address_from_addrinfo(entry) {
                return Ok(address);
            }
            current = entry.ai_next;
        }
        Err(Error::NotFound)
    }

    fn address_from_addrinfo(entry: &libc::addrinfo) -> Option<IpAddr> {
        if entry.ai_addr.is_null() {
            return None;
        }

        match entry.ai_family {
            libc::AF_INET
                if entry.ai_addrlen as usize >= core::mem::size_of::<libc::sockaddr_in>() =>
            {
                // SAFETY: getaddrinfo guarantees ai_addr points to an object
                // matching ai_family and the length was checked above.
                let socket =
                    unsafe { core::ptr::read_unaligned(entry.ai_addr.cast::<libc::sockaddr_in>()) };
                Some(IpAddr::V4(Ipv4Addr::from(
                    socket.sin_addr.s_addr.to_ne_bytes(),
                )))
            }
            libc::AF_INET6
                if entry.ai_addrlen as usize >= core::mem::size_of::<libc::sockaddr_in6>() =>
            {
                // SAFETY: getaddrinfo guarantees ai_addr points to an object
                // matching ai_family and the length was checked above.
                let socket = unsafe {
                    core::ptr::read_unaligned(entry.ai_addr.cast::<libc::sockaddr_in6>())
                };
                Some(IpAddr::V6(Ipv6Addr::from(socket.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }

    struct AddrInfoList(*mut libc::addrinfo);

    impl Drop for AddrInfoList {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from a successful getaddrinfo call
                // and has not been freed.
                unsafe { libc::freeaddrinfo(self.0) };
            }
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use core::net::IpAddr;

    use embedded_nal_async::AddrType;

    use super::Error;

    pub fn resolve(_host: &str, _addr_type: &AddrType) -> Result<IpAddr, Error> {
        Err(Error::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use core::net::{IpAddr, Ipv4Addr};

    use embedded_nal_async::AddrType;

    use super::Resolver;
    use crate::dns::Resolver as _;

    #[test]
    fn resolves_numeric_addresses_without_libc() {
        assert_eq!(
            Resolver.resolve("127.0.0.1", AddrType::IPv4),
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_localhost() {
        assert!(matches!(
            Resolver.resolve("localhost", AddrType::Either),
            Ok(address) if address.is_loopback()
        ));
    }
}
