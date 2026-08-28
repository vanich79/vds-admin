//! Turning a snapshot into storable samples.
//!
//! Only measurements that actually exist become rows. A metric the host could not
//! provide is *absent* from storage rather than stored as zero, which is what lets a
//! chart show a gap instead of a plausible-looking lie.

use crate::monitoring::rates::NetworkRates;
use vds_domain::ids::ServerId;
use vds_domain::metrics::{MetricKind, MetricSample};
use vds_domain::server::ServerSnapshot;

/// Extracts every available measurement from a snapshot.
pub fn samples_from_snapshot(
    snapshot: &ServerSnapshot,
    rates: Option<NetworkRates>,
) -> Vec<MetricSample> {
    let mut samples = Vec::with_capacity(12);
    let server_id = snapshot.server_id;
    let timestamp = snapshot.collected_at;

    let mut push = |kind: MetricKind, value: Option<f64>| {
        if let Some(value) = value
            && value.is_finite()
        {
            samples.push(MetricSample {
                server_id,
                kind,
                value,
                timestamp,
            });
        }
    };

    push(MetricKind::CpuUsage, snapshot.cpu.total_percent.value());
    push(
        MetricKind::MemoryUsage,
        snapshot.memory.used_percent().value(),
    );
    push(
        MetricKind::SwapUsage,
        snapshot.memory.swap_used_percent().value(),
    );
    push(
        MetricKind::DiskUsage,
        snapshot.worst_filesystem_percent().value(),
    );
    push(
        MetricKind::TemperatureCelsius,
        snapshot.temperature_celsius.value(),
    );

    push(
        MetricKind::MemoryUsedBytes,
        snapshot.memory.used_bytes.map(|b| b as f64),
    );
    if !snapshot.filesystems.is_empty() {
        push(
            MetricKind::DiskUsedBytes,
            Some(snapshot.used_disk_bytes() as f64),
        );
    }

    if let Some(load) = snapshot.load {
        push(MetricKind::LoadAverage1, Some(load.one));
        push(MetricKind::LoadAverage5, Some(load.five));
        push(MetricKind::LoadAverage15, Some(load.fifteen));
    }

    push(
        MetricKind::UptimeSeconds,
        snapshot.uptime_secs.map(|s| s as f64),
    );

    if !snapshot.processes.is_empty() {
        push(
            MetricKind::ProcessCount,
            Some(snapshot.processes.len() as f64),
        );
    }

    // Rates are absent on the first collection after startup, because there is no
    // previous reading to difference against.
    if let Some(rates) = rates {
        push(
            MetricKind::NetworkRxBytesPerSec,
            Some(rates.rx_bytes_per_sec),
        );
        push(
            MetricKind::NetworkTxBytesPerSec,
            Some(rates.tx_bytes_per_sec),
        );
    }

    samples
}

/// Extracts the samples a website check produces.
pub fn samples_from_check(
    check: &vds_domain::website::WebsiteCheck,
    server_id: ServerId,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<MetricSample> {
    let mut samples = Vec::with_capacity(2);

    if let Some(ms) = check.response_ms {
        samples.push(MetricSample {
            server_id,
            kind: MetricKind::ResponseTimeMs,
            value: f64::from(ms),
            timestamp: check.checked_at,
        });
    }

    if let Some(ssl) = &check.ssl {
        samples.push(MetricSample {
            server_id,
            kind: MetricKind::SslDaysRemaining,
            value: ssl.days_remaining(now) as f64,
            timestamp: check.checked_at,
        });
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::metrics::MetricValue;
    use vds_domain::server::{FilesystemUsage, LoadAverage, MemoryUsage, ProcessInfo};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn full_snapshot() -> ServerSnapshot {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(1_000));
        snapshot.cpu.total_percent = MetricValue::Available(42.0);
        snapshot.memory = MemoryUsage {
            total_bytes: Some(1_000),
            used_bytes: Some(600),
            available_bytes: Some(400),
            swap_total_bytes: Some(500),
            swap_used_bytes: Some(100),
        };
        snapshot.filesystems = vec![FilesystemUsage {
            mount_point: "/".into(),
            device: None,
            filesystem: None,
            total_bytes: 1_000,
            used_bytes: 720,
            available_bytes: 280,
        }];
        snapshot.load = Some(LoadAverage {
            one: 1.5,
            five: 1.2,
            fifteen: 0.9,
        });
        snapshot.uptime_secs = Some(123_456);
        snapshot.temperature_celsius = MetricValue::Available(48.0);
        snapshot.processes = vec![ProcessInfo {
            pid: 1,
            user: None,
            command: "init".into(),
            cpu_percent: 0.0,
            memory_percent: 0.0,
            rss_bytes: None,
        }];
        snapshot
    }

    fn kinds(samples: &[MetricSample]) -> Vec<MetricKind> {
        samples.iter().map(|s| s.kind).collect()
    }

    fn value_of(samples: &[MetricSample], kind: MetricKind) -> Option<f64> {
        samples.iter().find(|s| s.kind == kind).map(|s| s.value)
    }

    #[test]
    fn a_full_snapshot_produces_every_metric() {
        let samples = samples_from_snapshot(&full_snapshot(), None);
        let kinds = kinds(&samples);

        for expected in [
            MetricKind::CpuUsage,
            MetricKind::MemoryUsage,
            MetricKind::SwapUsage,
            MetricKind::DiskUsage,
            MetricKind::TemperatureCelsius,
            MetricKind::MemoryUsedBytes,
            MetricKind::DiskUsedBytes,
            MetricKind::LoadAverage1,
            MetricKind::LoadAverage5,
            MetricKind::LoadAverage15,
            MetricKind::UptimeSeconds,
            MetricKind::ProcessCount,
        ] {
            assert!(kinds.contains(&expected), "{expected} missing");
        }
    }

    #[test]
    fn values_are_carried_through_correctly() {
        let samples = samples_from_snapshot(&full_snapshot(), None);
        assert_eq!(value_of(&samples, MetricKind::CpuUsage), Some(42.0));
        assert_eq!(value_of(&samples, MetricKind::MemoryUsage), Some(60.0));
        assert_eq!(value_of(&samples, MetricKind::SwapUsage), Some(20.0));
        assert_eq!(value_of(&samples, MetricKind::DiskUsage), Some(72.0));
        assert_eq!(value_of(&samples, MetricKind::LoadAverage1), Some(1.5));
        assert_eq!(
            value_of(&samples, MetricKind::UptimeSeconds),
            Some(123_456.0)
        );
    }

    #[test]
    fn every_sample_carries_the_collection_timestamp() {
        let samples = samples_from_snapshot(&full_snapshot(), None);
        assert!(samples.iter().all(|s| s.timestamp == at(1_000)));
    }

    #[test]
    fn an_empty_snapshot_produces_no_samples_rather_than_a_row_of_zeroes() {
        // Storing zeroes here is the single most damaging thing this function could do:
        // every chart would show a healthy, idle machine that was never measured.
        let snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        assert!(samples_from_snapshot(&snapshot, None).is_empty());
    }

    #[test]
    fn a_host_without_thermal_sensors_stores_no_temperature() {
        let mut snapshot = full_snapshot();
        snapshot.temperature_celsius = MetricValue::NotAvailable;
        let samples = samples_from_snapshot(&snapshot, None);
        assert!(!kinds(&samples).contains(&MetricKind::TemperatureCelsius));
    }

    #[test]
    fn a_host_without_swap_stores_no_swap_usage() {
        let mut snapshot = full_snapshot();
        snapshot.memory.swap_total_bytes = Some(0);
        snapshot.memory.swap_used_bytes = Some(0);
        let samples = samples_from_snapshot(&snapshot, None);
        assert!(!kinds(&samples).contains(&MetricKind::SwapUsage));
    }

    #[test]
    fn network_rates_are_absent_until_there_is_a_previous_reading() {
        let samples = samples_from_snapshot(&full_snapshot(), None);
        assert!(!kinds(&samples).contains(&MetricKind::NetworkRxBytesPerSec));

        let rates = NetworkRates {
            rx_bytes_per_sec: 1_500.0,
            tx_bytes_per_sec: 250.0,
        };
        let samples = samples_from_snapshot(&full_snapshot(), Some(rates));
        assert_eq!(
            value_of(&samples, MetricKind::NetworkRxBytesPerSec),
            Some(1_500.0)
        );
        assert_eq!(
            value_of(&samples, MetricKind::NetworkTxBytesPerSec),
            Some(250.0)
        );
    }

    #[test]
    fn non_finite_values_never_reach_storage() {
        let rates = NetworkRates {
            rx_bytes_per_sec: f64::NAN,
            tx_bytes_per_sec: 10.0,
        };
        let samples = samples_from_snapshot(&full_snapshot(), Some(rates));
        assert!(!kinds(&samples).contains(&MetricKind::NetworkRxBytesPerSec));
        assert_eq!(
            value_of(&samples, MetricKind::NetworkTxBytesPerSec),
            Some(10.0)
        );
    }

    #[test]
    fn a_website_check_yields_response_time_and_ssl_days() {
        use vds_domain::ids::WebsiteId;
        use vds_domain::website::{SslInfo, WebsiteCheck};

        let check = WebsiteCheck {
            website_id: WebsiteId::new(),
            checked_at: at(500),
            status: vds_domain::Status::Healthy,
            resolved_addresses: vec![],
            dns_ms: None,
            connect_ms: None,
            response_ms: Some(142),
            http_status: Some(200),
            final_url: None,
            ssl: Some(SslInfo {
                subject: "CN=example.com".into(),
                issuer: "CN=CA".into(),
                not_before: at(0),
                not_after: at(86_400 * 42),
                fingerprint: "ab".into(),
                san: vec![],
            }),
            failure: None,
        };

        let samples = samples_from_check(&check, ServerId::new(), at(0));
        assert_eq!(value_of(&samples, MetricKind::ResponseTimeMs), Some(142.0));
        assert_eq!(value_of(&samples, MetricKind::SslDaysRemaining), Some(42.0));
    }

    #[test]
    fn a_plain_http_check_yields_no_ssl_sample() {
        use vds_domain::ids::WebsiteId;
        use vds_domain::website::WebsiteCheck;

        let mut check = WebsiteCheck::failed(
            WebsiteId::new(),
            at(0),
            vds_domain::website::CheckStage::HttpRequest,
            "boom",
        );
        check.response_ms = Some(10);

        let samples = samples_from_check(&check, ServerId::new(), at(0));
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].kind, MetricKind::ResponseTimeMs);
    }
}
