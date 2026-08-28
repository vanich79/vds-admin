//! DNS resolution, timed and reported separately.
//!
//! Resolution is a distinct check stage because "the domain does not resolve" and "the
//! server refused the connection" are different problems with different fixes, and a
//! monitoring tool that collapses them into "site down" wastes the operator's time.

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{CLOUDFLARE, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Why a name could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DnsError {
    #[error("{host} does not resolve")]
    NotFound { host: String },
    #[error("resolution failed: {0}")]
    Failed(String),
    #[error("resolution timed out after {seconds}s")]
    Timeout { seconds: u64 },
}

/// A successful resolution and how long it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub addresses: Vec<IpAddr>,
    pub elapsed: Duration,
}

/// Resolves hostnames.
pub struct DnsResolver {
    resolver: TokioResolver,
}

impl DnsResolver {
    /// Builds a resolver from the system configuration, falling back to a public
    /// resolver when the system has none.
    ///
    /// The fallback matters on containers and minimal images where `/etc/resolv.conf`
    /// is absent: without it every website check would fail for a reason that has
    /// nothing to do with the website.
    pub fn from_system() -> Result<Self, DnsError> {
        if let Ok(builder) = TokioResolver::builder_tokio()
            && let Ok(resolver) = builder.build()
        {
            return Ok(Self { resolver });
        }

        tracing::warn!("no usable system DNS configuration; falling back to Cloudflare resolvers");
        Self::with_config(
            ResolverConfig::udp_and_tcp(&CLOUDFLARE),
            ResolverOpts::default(),
        )
    }

    /// Builds a resolver with explicit configuration, for tests and for the fallback.
    pub fn with_config(config: ResolverConfig, options: ResolverOpts) -> Result<Self, DnsError> {
        let mut builder =
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
        *builder.options_mut() = options;
        builder
            .build()
            .map(|resolver| Self { resolver })
            .map_err(|err| DnsError::Failed(format!("could not build a resolver: {err}")))
    }

    /// Resolves a host, timing the lookup.
    ///
    /// A literal IP address resolves to itself without a lookup, so monitoring a host by
    /// address does not depend on DNS working at all.
    pub async fn resolve(&self, host: &str, timeout: Duration) -> Result<Resolution, DnsError> {
        if let Ok(address) = host.parse::<IpAddr>() {
            return Ok(Resolution {
                addresses: vec![address],
                elapsed: Duration::ZERO,
            });
        }

        let started = Instant::now();
        let lookup = tokio::time::timeout(timeout, self.resolver.lookup_ip(host))
            .await
            .map_err(|_| DnsError::Timeout {
                seconds: timeout.as_secs(),
            })?;

        let elapsed = started.elapsed();

        match lookup {
            Ok(response) => {
                let addresses: Vec<IpAddr> = response.iter().collect();
                if addresses.is_empty() {
                    // A response with no records is a resolution failure, not a success
                    // with nothing in it.
                    Err(DnsError::NotFound {
                        host: host.to_owned(),
                    })
                } else {
                    Ok(Resolution { addresses, elapsed })
                }
            }
            Err(err) if err.is_no_records_found() => Err(DnsError::NotFound {
                host: host.to_owned(),
            }),
            Err(err) => Err(DnsError::Failed(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_literal_ipv4_address_needs_no_lookup() {
        // Monitoring a host by IP must not depend on DNS working.
        let resolver = DnsResolver::from_system().expect("a resolver is always available");
        let resolution = resolver
            .resolve("93.184.216.34", Duration::from_secs(1))
            .await
            .expect("resolves");
        assert_eq!(
            resolution.addresses,
            vec!["93.184.216.34".parse::<IpAddr>().expect("valid")]
        );
        assert_eq!(resolution.elapsed, Duration::ZERO);
    }

    #[tokio::test]
    async fn a_literal_ipv6_address_needs_no_lookup() {
        let resolver = DnsResolver::from_system().expect("a resolver is always available");
        let resolution = resolver
            .resolve("::1", Duration::from_secs(1))
            .await
            .expect("resolves");
        assert_eq!(
            resolution.addresses,
            vec!["::1".parse::<IpAddr>().expect("valid")]
        );
    }

    #[tokio::test]
    async fn localhost_resolves_through_the_system_resolver() {
        let resolver = DnsResolver::from_system().expect("a resolver is always available");
        let resolution = resolver
            .resolve("localhost", Duration::from_secs(5))
            .await
            .expect("localhost must resolve");
        assert!(!resolution.addresses.is_empty());
        assert!(resolution.addresses.iter().any(|a| a.is_loopback()));
    }

    #[tokio::test]
    async fn a_resolver_is_always_constructible_even_without_system_configuration() {
        // Containers with no /etc/resolv.conf must still get a working resolver.
        let resolver = DnsResolver::with_config(
            ResolverConfig::udp_and_tcp(&CLOUDFLARE),
            ResolverOpts::default(),
        )
        .expect("builds");
        // Resolving a literal exercises the object without needing the network.
        assert!(
            resolver
                .resolve("127.0.0.1", Duration::from_secs(1))
                .await
                .is_ok()
        );
    }

    #[test]
    fn dns_errors_name_the_host_that_failed() {
        let err = DnsError::NotFound {
            host: "nope.invalid".into(),
        };
        assert!(err.to_string().contains("nope.invalid"));
    }
}
