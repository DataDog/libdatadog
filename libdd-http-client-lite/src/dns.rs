// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! DNS resolvers for constrained and conventional environments.

use core::{fmt, net::IpAddr};

use embedded_nal_async::AddrType;

use crate::env::OsEnv;

/// Synchronous DNS interface shared by all lite client resolvers.
pub trait Resolver {
    /// Error returned by the resolver.
    type Error: fmt::Debug;

    /// Resolves the first IP address for `host` matching `addr_type`.
    ///
    /// # Errors
    ///
    /// Returns a resolver-specific error when the hostname cannot be resolved.
    fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, Self::Error>;
}

/// Error returned by [`DnsResolver`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error<E> {
    /// The environment lookup failed.
    Environment(E),
    /// The hostname was not present in the environment.
    NotFound,
    /// The environment value was not a valid IP address.
    InvalidAddress,
    /// The resolved IP address did not match the requested address type.
    AddressTypeUnavailable,
}

impl<E> fmt::Display for Error<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment(_) => formatter.write_str("DNS environment lookup failed"),
            Self::NotFound => formatter.write_str("DNS hostname was not found"),
            Self::InvalidAddress => {
                formatter.write_str("DNS environment value is not an IP address")
            }
            Self::AddressTypeUnavailable => {
                formatter.write_str("DNS address does not match the requested type")
            }
        }
    }
}

#[cfg(feature = "std")]
impl<E> std::error::Error for Error<E> where E: fmt::Debug + std::error::Error + 'static {}

/// A `no_std` resolver backed by an environment-like key/value source.
///
/// Numeric hostnames are parsed directly. Other hostnames are used verbatim as
/// keys whose values must contain an IPv4 or IPv6 address.
#[derive(Clone, Copy, Debug)]
pub struct DnsResolver<K> {
    environment: K,
}

impl<K> DnsResolver<K> {
    /// Creates a resolver backed by `environment`.
    #[must_use]
    pub const fn new(environment: K) -> Self {
        Self { environment }
    }

    /// Returns the underlying environment.
    #[must_use]
    pub fn into_inner(self) -> K {
        self.environment
    }
}

impl<K> Resolver for DnsResolver<K>
where
    K: OsEnv,
{
    type Error = Error<K::Error>;

    fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, Self::Error> {
        if let Ok(address) = host.parse() {
            return require_address_type(address, addr_type);
        }

        let value = self
            .environment
            .get(host)
            .map_err(Error::Environment)?
            .ok_or(Error::NotFound)?;
        let address = value.as_ref().parse().map_err(|_| Error::InvalidAddress)?;
        require_address_type(address, addr_type)
    }
}

const fn require_address_type<E>(address: IpAddr, addr_type: AddrType) -> Result<IpAddr, Error<E>> {
    match (address, addr_type) {
        (IpAddr::V4(_), AddrType::IPv6) | (IpAddr::V6(_), AddrType::IPv4) => {
            Err(Error::AddressTypeUnavailable)
        }
        _ => Ok(address),
    }
}

#[cfg(test)]
mod tests {
    use core::net::{IpAddr, Ipv4Addr};

    use embedded_nal_async::AddrType;

    use super::{DnsResolver, Error, Resolver as _};
    use crate::env::Environment;

    const ENTRIES: &[(&str, &str)] = &[("agent.internal", "127.0.0.1")];

    #[test]
    fn resolves_an_environment_entry() {
        let resolver = DnsResolver::new(Environment::new(ENTRIES));
        assert_eq!(
            resolver.resolve("agent.internal", AddrType::Either),
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn resolves_a_numeric_address_without_an_environment_entry() {
        let resolver = DnsResolver::new(Environment::new(&[]));
        assert_eq!(
            resolver.resolve("127.0.0.1", AddrType::IPv4),
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn enforces_the_requested_address_type() {
        let resolver = DnsResolver::new(Environment::new(ENTRIES));
        assert_eq!(
            resolver.resolve("agent.internal", AddrType::IPv6),
            Err(Error::AddressTypeUnavailable)
        );
    }
}
