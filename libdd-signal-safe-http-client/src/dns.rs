// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! libc DNS helpers for non-signal-handler setup paths.
//!
//! This module is only available with the `libc-dns` feature. It calls libc's
//! `getaddrinfo` through symbols discovered at runtime. Missing resolver symbols
//! are reported as regular errors rather than becoming link-time failures.

use alloc::vec::Vec;
use core::{fmt, mem::MaybeUninit, ptr};

/// DNS resolution error.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DnsError {
    /// libc DNS support is not implemented for this target.
    #[error("libc DNS is not supported on this platform")]
    UnsupportedPlatform,
    /// A libc resolver symbol could not be found at runtime.
    #[error("libc DNS resolver symbol is missing: {symbol}")]
    MissingResolverSymbol {
        /// Missing symbol name.
        symbol: &'static str,
    },
    /// The hostname contains an interior NUL byte and cannot be passed to libc.
    #[error("DNS host contains an interior NUL byte")]
    HostContainsNul,
    /// Allocating storage for the resolver request or response failed.
    #[error("failed to allocate DNS resolver storage")]
    AllocationFailed,
    /// libc `getaddrinfo` returned a non-zero status code.
    #[error("getaddrinfo failed with code {code}")]
    GetAddrInfo {
        /// Raw `getaddrinfo` status code.
        code: i32,
    },
    /// libc returned a socket address larger than `sockaddr_storage`.
    #[error("getaddrinfo returned an address larger than sockaddr_storage")]
    AddressTooLarge,
}

/// A socket address returned by libc `getaddrinfo`.
#[derive(Clone, Copy)]
pub struct ResolvedAddress {
    storage: libc::sockaddr_storage,
    len: libc::socklen_t,
}

impl ResolvedAddress {
    /// Creates a resolved address from raw socket-address storage.
    pub const fn new(storage: libc::sockaddr_storage, len: libc::socklen_t) -> Self {
        Self { storage, len }
    }

    /// Returns a pointer suitable for libc socket calls such as `connect`.
    pub fn as_ptr(&self) -> *const libc::sockaddr {
        ptr::addr_of!(self.storage).cast()
    }

    /// Returns the socket address length.
    pub const fn len(&self) -> libc::socklen_t {
        self.len
    }

    /// Returns whether the socket address length is zero.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the socket address family.
    pub const fn family(&self) -> libc::sa_family_t {
        self.storage.ss_family
    }

    /// Returns the raw socket-address storage.
    pub const fn storage(&self) -> &libc::sockaddr_storage {
        &self.storage
    }
}

impl fmt::Debug for ResolvedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedAddress")
            .field("family", &self.family())
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// Resolves `host:port` with libc `getaddrinfo`.
///
/// This allocates a result vector and may use resolver state managed by libc or the operating
/// system. Use it during setup, not from an async signal handler.
pub fn resolve_host(host: &str, port: u16) -> Result<Vec<ResolvedAddress>, DnsError> {
    imp::resolve_host(host, port)
}

#[cfg(unix)]
mod imp {
    use super::{ptr, DnsError, MaybeUninit, ResolvedAddress, Vec};
    use core::mem;

    type GetAddrInfo = unsafe extern "C" fn(
        *const libc::c_char,
        *const libc::c_char,
        *const libc::addrinfo,
        *mut *mut libc::addrinfo,
    ) -> libc::c_int;
    type FreeAddrInfo = unsafe extern "C" fn(*mut libc::addrinfo);

    const GETADDRINFO: &[u8] = b"getaddrinfo\0";
    const FREEADDRINFO: &[u8] = b"freeaddrinfo\0";

    pub(super) fn resolve_host(host: &str, port: u16) -> Result<Vec<ResolvedAddress>, DnsError> {
        let host = nul_terminated(host)?;
        let symbols = ResolverSymbols::load()?;
        let mut service_buffer = [0_u8; 6];
        let service = write_port(port, &mut service_buffer);
        let hints = libc::addrinfo {
            ai_flags: 0,
            ai_family: libc::AF_UNSPEC,
            ai_socktype: libc::SOCK_STREAM,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: ptr::null_mut(),
            ai_canonname: ptr::null_mut(),
            ai_next: ptr::null_mut(),
        };
        let mut result = ptr::null_mut();

        // SAFETY: Pointers are NUL-terminated and valid for the duration of the call. `result`
        // points to writable storage for libc to fill.
        let status = unsafe {
            (symbols.getaddrinfo)(
                host.as_ptr().cast(),
                service.as_ptr().cast(),
                ptr::addr_of!(hints),
                ptr::addr_of_mut!(result),
            )
        };
        if status != 0 {
            return Err(DnsError::GetAddrInfo { code: status });
        }

        let list = AddrInfoList {
            head: result,
            freeaddrinfo: symbols.freeaddrinfo,
        };
        collect_addresses(&list)
    }

    #[derive(Clone, Copy)]
    struct ResolverSymbols {
        getaddrinfo: GetAddrInfo,
        freeaddrinfo: FreeAddrInfo,
    }

    impl ResolverSymbols {
        fn load() -> Result<Self, DnsError> {
            Ok(Self {
                getaddrinfo: load_getaddrinfo()?,
                freeaddrinfo: load_freeaddrinfo()?,
            })
        }
    }

    fn load_getaddrinfo() -> Result<GetAddrInfo, DnsError> {
        let ptr = lookup_symbol(GETADDRINFO, "getaddrinfo")?;

        // SAFETY: The symbol name is for libc `getaddrinfo`, whose signature matches `GetAddrInfo`.
        Ok(unsafe { mem::transmute::<*mut libc::c_void, GetAddrInfo>(ptr) })
    }

    fn load_freeaddrinfo() -> Result<FreeAddrInfo, DnsError> {
        let ptr = lookup_symbol(FREEADDRINFO, "freeaddrinfo")?;

        // SAFETY: The symbol name is for libc `freeaddrinfo`, whose signature matches
        // `FreeAddrInfo`.
        Ok(unsafe { mem::transmute::<*mut libc::c_void, FreeAddrInfo>(ptr) })
    }

    fn lookup_symbol(
        nul_terminated_name: &'static [u8],
        symbol: &'static str,
    ) -> Result<*mut libc::c_void, DnsError> {
        // SAFETY: `nul_terminated_name` must be NUL-terminated. A null return is handled as a
        // normal error.
        let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, nul_terminated_name.as_ptr().cast()) };
        if ptr.is_null() {
            return Err(DnsError::MissingResolverSymbol { symbol });
        }
        Ok(ptr)
    }

    struct AddrInfoList {
        head: *mut libc::addrinfo,
        freeaddrinfo: FreeAddrInfo,
    }

    impl Drop for AddrInfoList {
        fn drop(&mut self) {
            if !self.head.is_null() {
                // SAFETY: `head` came from a successful `getaddrinfo` call and is freed exactly
                // once by this guard.
                unsafe { (self.freeaddrinfo)(self.head) };
            }
        }
    }

    fn collect_addresses(list: &AddrInfoList) -> Result<Vec<ResolvedAddress>, DnsError> {
        let mut addresses = Vec::new();
        let mut current = list.head;

        while !current.is_null() {
            // SAFETY: `current` walks the linked list returned by `getaddrinfo` until a null
            // terminator.
            let info = unsafe { &*current };
            if !info.ai_addr.is_null() {
                addresses
                    .try_reserve(1)
                    .map_err(|_| DnsError::AllocationFailed)?;
                addresses.push(copy_address(info)?);
            }
            current = info.ai_next;
        }

        Ok(addresses)
    }

    fn copy_address(info: &libc::addrinfo) -> Result<ResolvedAddress, DnsError> {
        let len = usize::try_from(info.ai_addrlen).map_err(|_| DnsError::AddressTooLarge)?;
        if len > mem::size_of::<libc::sockaddr_storage>() {
            return Err(DnsError::AddressTooLarge);
        }

        let mut storage = MaybeUninit::<libc::sockaddr_storage>::zeroed();
        // SAFETY: Both pointers are valid for `len` bytes. The destination is zero-initialized
        // storage large enough for any supported sockaddr.
        unsafe {
            ptr::copy_nonoverlapping(info.ai_addr.cast::<u8>(), storage.as_mut_ptr().cast(), len);
            Ok(ResolvedAddress::new(storage.assume_init(), info.ai_addrlen))
        }
    }

    fn nul_terminated(value: &str) -> Result<Vec<u8>, DnsError> {
        if value.as_bytes().contains(&0) {
            return Err(DnsError::HostContainsNul);
        }

        let mut out = Vec::new();
        out.try_reserve_exact(value.len() + 1)
            .map_err(|_| DnsError::AllocationFailed)?;
        out.extend_from_slice(value.as_bytes());
        out.push(0);
        Ok(out)
    }

    fn write_port(port: u16, buffer: &mut [u8; 6]) -> &[u8] {
        let mut value = port;
        let mut index = buffer.len() - 1;
        buffer[index] = 0;

        loop {
            index -= 1;
            buffer[index] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }

        &buffer[index..]
    }

    #[cfg(test)]
    mod tests {
        use super::{lookup_symbol, DnsError};

        #[test]
        fn missing_resolver_symbol_returns_specific_error() {
            assert!(matches!(
                lookup_symbol(
                    b"libdd_signal_safe_http_client_missing_symbol_for_test\0",
                    "missing-symbol"
                ),
                Err(DnsError::MissingResolverSymbol {
                    symbol: "missing-symbol"
                })
            ));
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{DnsError, ResolvedAddress, Vec};

    pub(super) fn resolve_host(_host: &str, _port: u16) -> Result<Vec<ResolvedAddress>, DnsError> {
        Err(DnsError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_with_nul_is_rejected() {
        assert!(matches!(
            resolve_host("local\0host", 8126),
            Err(DnsError::HostContainsNul)
        ));
    }
}
