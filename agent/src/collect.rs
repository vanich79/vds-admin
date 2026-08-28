//! Reading the host, and not reading it more often than necessary.
//!
//! ## Why there is a cache
//!
//! A collection cycle reads a dozen `/proc` files and, when Docker and systemd are
//! present, spawns two short-lived processes. That is cheap once, and it is not cheap
//! once per client per poll: several app instances may watch the same host, a dashboard
//! refresh and a detail page can arrive together, and nothing stops a scrape loop from
//! being misconfigured to one second.
//!
//! So a report is served from memory for [`AgentConfig::cache_ttl_secs`] before the host
//! is read again. The staleness is bounded and visible — every report carries the
//! timestamp of the collection that produced it, so the app never has to guess how old
//! a number is.
//!
//! Concurrent requests that arrive on a cold cache all wait on the same collection rather
//! than each starting one; that is what the `Mutex` around the collection itself buys.

use crate::config::AgentConfig;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vds_agent_protocol::MetricsReport;
use vds_domain::ids::ServerId;
use vds_domain::ports::CommandRunner;
use vds_infra_collectors::{CollectorRegistry, LocalCommandRunner};

/// Collects host metrics, with a short cache in front.
pub struct Collector {
    registry: CollectorRegistry,
    runner: LocalCommandRunner,
    /// The agent has no opinion about which server it is; the app knows that. A stable
    /// placeholder keeps the snapshot type happy without inventing an identity.
    server_id: ServerId,
    ttl: Duration,
    cached: parking_lot::Mutex<Option<Cached>>,
    /// Held across a collection so that a thundering herd on a cold cache produces one
    /// read of the host rather than one per caller.
    collecting: tokio::sync::Mutex<()>,
}

struct Cached {
    report: Arc<MetricsReport>,
    at: Instant,
}

impl Collector {
    /// Builds a collector from configuration.
    pub fn new(config: &AgentConfig) -> Self {
        Self {
            registry: registry_for(config),
            runner: LocalCommandRunner::new(Duration::from_secs(config.collect_timeout_secs)),
            server_id: ServerId::new(),
            ttl: Duration::from_secs(config.cache_ttl_secs),
            cached: parking_lot::Mutex::new(None),
            collecting: tokio::sync::Mutex::new(()),
        }
    }

    /// The current report, collecting only if the cached one has expired.
    pub async fn report(&self) -> Result<Arc<MetricsReport>, CollectionFailed> {
        if let Some(fresh) = self.fresh() {
            return Ok(fresh);
        }

        let _guard = self.collecting.lock().await;

        // Another caller may have collected while this one waited for the lock.
        if let Some(fresh) = self.fresh() {
            return Ok(fresh);
        }

        let snapshot = self
            .registry
            .collect(
                &self.runner as &dyn CommandRunner,
                self.server_id,
                chrono::Utc::now(),
            )
            .await
            .map_err(|err| CollectionFailed(err.to_string()))?;

        let report = Arc::new(crate::report::to_report(&snapshot));
        *self.cached.lock() = Some(Cached {
            report: Arc::clone(&report),
            at: Instant::now(),
        });
        Ok(report)
    }

    /// The cached report, if it has not expired.
    fn fresh(&self) -> Option<Arc<MetricsReport>> {
        let guard = self.cached.lock();
        let cached = guard.as_ref()?;
        (cached.at.elapsed() < self.ttl).then(|| Arc::clone(&cached.report))
    }

    /// Drops the cached report. Used by the tests; also the honest thing to call if a
    /// future endpoint ever offers a forced refresh.
    #[cfg(test)]
    fn invalidate(&self) {
        *self.cached.lock() = None;
    }
}

/// Which collectors run, given the configuration.
///
/// A disabled collector is *left out of the plan* rather than run and discarded: the
/// point of turning off the process table on a busy host is to stop reading it.
fn registry_for(config: &AgentConfig) -> CollectorRegistry {
    use std::sync::Arc as StdArc;
    use vds_domain::ports::Collector as CollectorPort;
    use vds_infra_collectors::{
        CpuCollector, DiskCollector, DockerCollector, LoadCollector, MemoryCollector,
        NetworkCollector, ProcessCollector, ServiceCollector, SystemCollector,
        TemperatureCollector,
    };

    let mut collectors: Vec<StdArc<dyn CollectorPort>> = vec![
        StdArc::new(SystemCollector),
        StdArc::new(CpuCollector),
        StdArc::new(MemoryCollector),
        StdArc::new(DiskCollector),
        StdArc::new(NetworkCollector),
        StdArc::new(LoadCollector),
        StdArc::new(TemperatureCollector),
    ];

    if config.collect_processes {
        collectors.push(StdArc::new(ProcessCollector));
    }
    if config.collect_docker {
        collectors.push(StdArc::new(DockerCollector));
    }
    if config.collect_services {
        collectors.push(StdArc::new(ServiceCollector));
    }

    CollectorRegistry::new(collectors)
}

/// The host could not be read at all.
#[derive(Debug, thiserror::Error)]
#[error("could not collect host metrics: {0}")]
pub struct CollectionFailed(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AgentConfig {
        AgentConfig::default()
    }

    #[test]
    fn every_collector_is_present_by_default() {
        let registry = registry_for(&config());
        assert_eq!(registry.len(), 10);
    }

    #[test]
    fn a_disabled_collector_is_left_out_of_the_plan_entirely() {
        // Not run-and-discard: the point of switching one off is to stop the work.
        let config = AgentConfig {
            collect_processes: false,
            collect_docker: false,
            collect_services: false,
            ..Default::default()
        };
        let registry = registry_for(&config);
        assert_eq!(registry.len(), 7);

        let plan = registry.plan();
        let commands = format!("{:?}", plan.commands);
        assert!(!commands.contains("docker"), "docker survived: {commands}");
        assert!(
            !commands.contains("systemctl"),
            "systemd survived: {commands}"
        );
    }

    #[tokio::test]
    async fn a_second_request_inside_the_ttl_is_served_from_the_cache() {
        // The point of the cache: several watchers must not multiply the load.
        let collector = Collector::new(&config());

        let first = collector.report().await.expect("collects");
        let second = collector.report().await.expect("collects");

        assert!(
            Arc::ptr_eq(&first, &second),
            "the second call re-read the host"
        );
    }

    #[tokio::test]
    async fn an_expired_cache_causes_a_fresh_collection() {
        let collector = Collector::new(&AgentConfig {
            cache_ttl_secs: 0,
            ..Default::default()
        });

        let first = collector.report().await.expect("collects");
        let second = collector.report().await.expect("collects");
        assert!(!Arc::ptr_eq(&first, &second), "a zero TTL must never cache");
    }

    #[tokio::test]
    async fn invalidating_forces_the_next_request_to_collect() {
        let collector = Collector::new(&config());
        let first = collector.report().await.expect("collects");
        collector.invalidate();
        let second = collector.report().await.expect("collects");
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn concurrent_cold_requests_produce_one_collection() {
        let collector = Arc::new(Collector::new(&config()));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let collector = Arc::clone(&collector);
            handles.push(tokio::spawn(async move { collector.report().await }));
        }

        let mut reports = Vec::new();
        for handle in handles {
            reports.push(handle.await.expect("joins").expect("collects"));
        }

        let first = &reports[0];
        assert!(
            reports.iter().all(|report| Arc::ptr_eq(report, first)),
            "the herd caused more than one collection"
        );
    }

    #[tokio::test]
    async fn a_host_without_linux_proc_files_still_produces_a_report() {
        // This test runs on the developer's machine, which may well not be Linux. A
        // collector that finds nothing must degrade to `NotAvailable`, never fail the
        // whole cycle — the same guarantee a stripped-down container needs.
        let collector = Collector::new(&config());
        let report = collector.report().await.expect("a report even here");

        assert_eq!(
            report.protocol_version,
            vds_agent_protocol::PROTOCOL_VERSION
        );
        assert!(report.collected_at > 0);
    }
}
