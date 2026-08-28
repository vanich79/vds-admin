//! Deriving network rates from cumulative counters.
//!
//! `/proc/net/dev` reports bytes since boot. A rate needs two readings and the time
//! between them, which no collector can know — so it is computed here, from consecutive
//! snapshots.
//!
//! The subtleties this handles, all of which produce spectacular false readings if
//! ignored: a reboot resets counters to zero, a 32-bit counter wraps, an interface
//! disappears, and two snapshots can arrive with no time between them.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use vds_domain::server::NetworkInterface;

/// The previous reading, kept per server between collection cycles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceCounters {
    pub taken_at: DateTime<Utc>,
    /// Cumulative bytes per interface name.
    pub interfaces: HashMap<String, (u64, u64)>,
}

impl InterfaceCounters {
    pub fn from_snapshot(interfaces: &[NetworkInterface], at: DateTime<Utc>) -> Self {
        Self {
            taken_at: at,
            interfaces: interfaces
                .iter()
                .map(|i| (i.name.clone(), (i.rx_bytes, i.tx_bytes)))
                .collect(),
        }
    }
}

/// Bytes per second in each direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetworkRates {
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
}

/// Longest gap between readings that still yields a meaningful rate.
///
/// Beyond this the average is so smeared it says nothing useful, and it usually means
/// the app was asleep or the server was unreachable in between.
const MAX_USEFUL_GAP: Duration = Duration::minutes(15);

/// Computes rates between two readings.
///
/// Returns `None` — rather than zero — whenever the numbers cannot be trusted. A
/// fabricated zero on a busy server is a worse answer than an honest gap in the chart.
pub fn rates_between(
    previous: &InterfaceCounters,
    current: &[NetworkInterface],
    now: DateTime<Utc>,
) -> Option<NetworkRates> {
    let elapsed = now - previous.taken_at;
    if elapsed <= Duration::zero() || elapsed > MAX_USEFUL_GAP {
        return None;
    }
    let seconds = elapsed.num_milliseconds() as f64 / 1_000.0;
    if seconds <= 0.0 {
        return None;
    }

    let mut rx_delta = 0_u64;
    let mut tx_delta = 0_u64;
    let mut matched = false;

    for interface in current {
        // An interface that appeared since the last reading has no baseline; counting
        // its lifetime total as one interval's traffic would produce an enormous spike.
        let Some((prev_rx, prev_tx)) = previous.interfaces.get(&interface.name) else {
            continue;
        };
        matched = true;

        rx_delta = rx_delta.saturating_add(counter_delta(*prev_rx, interface.rx_bytes));
        tx_delta = tx_delta.saturating_add(counter_delta(*prev_tx, interface.tx_bytes));
    }

    if !matched {
        return None;
    }

    Some(NetworkRates {
        rx_bytes_per_sec: rx_delta as f64 / seconds,
        tx_bytes_per_sec: tx_delta as f64 / seconds,
    })
}

/// The increase in a monotonic counter, treating any decrease as a reset.
///
/// A counter that went backwards means the machine rebooted or the interface was reset.
/// The honest answer for that interval is "we do not know how much traffic there was",
/// and zero is the only non-fabricated stand-in — reporting the raw new value would
/// claim the machine's entire lifetime traffic happened in thirty seconds.
fn counter_delta(previous: u64, current: u64) -> u64 {
    current.saturating_sub(previous)
}

/// Remembers the last reading for each server.
#[derive(Debug, Default)]
pub struct RateTracker {
    last: HashMap<vds_domain::ids::ServerId, InterfaceCounters>,
}

impl RateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a new reading and returns the rate since the previous one, if any.
    pub fn observe(
        &mut self,
        server: vds_domain::ids::ServerId,
        interfaces: &[NetworkInterface],
        now: DateTime<Utc>,
    ) -> Option<NetworkRates> {
        let rates = self
            .last
            .get(&server)
            .and_then(|prev| rates_between(prev, interfaces, now));
        self.last
            .insert(server, InterfaceCounters::from_snapshot(interfaces, now));
        rates
    }

    /// Drops a server's history, e.g. when it is deleted.
    pub fn forget(&mut self, server: vds_domain::ids::ServerId) {
        self.last.remove(&server);
    }

    pub fn tracked_count(&self) -> usize {
        self.last.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ids::ServerId;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn iface(name: &str, rx: u64, tx: u64) -> NetworkInterface {
        NetworkInterface {
            name: name.to_owned(),
            rx_bytes: rx,
            tx_bytes: tx,
            rx_errors: 0,
            tx_errors: 0,
        }
    }

    #[test]
    fn the_first_reading_yields_no_rate() {
        // With one sample there is nothing to compare against; a rate would be invented.
        let mut tracker = RateTracker::new();
        let rates = tracker.observe(ServerId::new(), &[iface("eth0", 1_000, 2_000)], at(0));
        assert_eq!(rates, None);
    }

    #[test]
    fn the_second_reading_yields_the_rate() {
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 1_000, 2_000)], at(0));

        let rates = tracker
            .observe(server, &[iface("eth0", 11_000, 4_000)], at(10))
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 1_000.0);
        assert_eq!(rates.tx_bytes_per_sec, 200.0);
    }

    #[test]
    fn rates_sum_across_interfaces() {
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 0, 0), iface("eth1", 0, 0)], at(0));

        let rates = tracker
            .observe(
                server,
                &[iface("eth0", 100, 50), iface("eth1", 900, 150)],
                at(10),
            )
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 100.0);
        assert_eq!(rates.tx_bytes_per_sec, 20.0);
    }

    #[test]
    fn a_reboot_does_not_produce_a_gigabyte_per_second_spike() {
        // Counters reset to near zero on reboot. Without the reset check, the *next*
        // reading's delta would be interpreted as a colossal burst.
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(
            server,
            &[iface("eth0", 900_000_000_000, 900_000_000_000)],
            at(0),
        );

        let rates = tracker
            .observe(server, &[iface("eth0", 5_000, 5_000)], at(10))
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 0.0);
        assert_eq!(rates.tx_bytes_per_sec, 0.0);
    }

    #[test]
    fn a_new_interface_does_not_dump_its_lifetime_total_into_one_interval() {
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 0, 0)], at(0));

        // A VPN interface appears with a large historical counter.
        let rates = tracker
            .observe(
                server,
                &[iface("eth0", 100, 100), iface("tun9", 5_000_000, 5_000_000)],
                at(10),
            )
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 10.0);
    }

    #[test]
    fn an_interface_that_disappears_is_simply_not_counted() {
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 0, 0), iface("eth1", 0, 0)], at(0));

        let rates = tracker
            .observe(server, &[iface("eth0", 100, 100)], at(10))
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 10.0);
    }

    #[test]
    fn a_long_gap_yields_no_rate_rather_than_a_meaningless_average() {
        // A laptop that was asleep for six hours: averaging over that window says
        // nothing about the network.
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 0, 0)], at(0));
        assert_eq!(
            tracker.observe(server, &[iface("eth0", 1_000_000, 0)], at(6 * 3_600)),
            None
        );
    }

    #[test]
    fn a_zero_or_negative_interval_yields_no_rate_rather_than_dividing_by_zero() {
        let previous = InterfaceCounters::from_snapshot(&[iface("eth0", 0, 0)], at(100));
        assert_eq!(
            rates_between(&previous, &[iface("eth0", 500, 0)], at(100)),
            None
        );
        // Clock went backwards.
        assert_eq!(
            rates_between(&previous, &[iface("eth0", 500, 0)], at(50)),
            None
        );
    }

    #[test]
    fn a_server_with_no_shared_interfaces_yields_no_rate() {
        let previous = InterfaceCounters::from_snapshot(&[iface("eth0", 0, 0)], at(0));
        assert_eq!(
            rates_between(&previous, &[iface("wlan0", 500, 0)], at(10)),
            None
        );
    }

    #[test]
    fn sub_second_intervals_are_handled_without_rounding_to_zero() {
        let previous = InterfaceCounters::from_snapshot(&[iface("eth0", 0, 0)], at(0));
        let half_second = at(0) + Duration::milliseconds(500);
        let rates = rates_between(&previous, &[iface("eth0", 500, 0)], half_second)
            .expect("rate available");
        assert_eq!(rates.rx_bytes_per_sec, 1_000.0);
    }

    #[test]
    fn each_server_is_tracked_independently() {
        let mut tracker = RateTracker::new();
        let a = ServerId::new();
        let b = ServerId::new();

        tracker.observe(a, &[iface("eth0", 0, 0)], at(0));
        // b's first reading must not borrow a's baseline.
        assert_eq!(
            tracker.observe(b, &[iface("eth0", 999_999, 0)], at(0)),
            None
        );
        assert_eq!(tracker.tracked_count(), 2);

        let rates = tracker
            .observe(a, &[iface("eth0", 100, 0)], at(10))
            .expect("rate");
        assert_eq!(rates.rx_bytes_per_sec, 10.0);
    }

    #[test]
    fn forgetting_a_server_drops_its_baseline() {
        let mut tracker = RateTracker::new();
        let server = ServerId::new();
        tracker.observe(server, &[iface("eth0", 0, 0)], at(0));
        tracker.forget(server);
        assert_eq!(tracker.tracked_count(), 0);
        assert_eq!(
            tracker.observe(server, &[iface("eth0", 100, 0)], at(10)),
            None
        );
    }
}
