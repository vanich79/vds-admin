//! Network interface counters from `/proc/net/dev`.
//!
//! Only cumulative counters are collected here. Rates are derived by the application
//! layer from two consecutive snapshots, because a rate needs to know how much wall
//! time elapsed between collections — something a collector cannot see.

use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::NetworkInterface;

/// Interfaces excluded from the totals.
///
/// Loopback traffic is real but is not network capacity, and counting it makes a busy
/// single-host application look like it is saturating a link.
const EXCLUDED_PREFIXES: &[&str] = &["lo", "veth", "docker", "br-", "virbr", "tun", "tap"];

/// Reads `/proc/net/dev`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkCollector;

impl Collector for NetworkCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("network")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ProcFs]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::read("/proc/net/dev")]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for /proc/net/dev"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("reading /proc/net/dev failed: {}", output.stderr.trim()),
            ));
        }

        Ok(CollectorOutput::Network(parse_net_dev(&output.stdout)))
    }
}

/// Parses `/proc/net/dev`, keeping only physical-ish interfaces.
///
/// Layout after the interface name and colon:
/// receive  = bytes packets errs drop fifo frame compressed multicast (8 fields)
/// transmit = bytes packets errs drop fifo colls carrier compressed  (8 fields)
pub fn parse_net_dev(text: &str) -> Vec<NetworkInterface> {
    text.lines()
        .filter_map(|line| {
            // The two header lines have no colon, which conveniently filters them out.
            let (name, counters) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let values: Vec<u64> = counters
                .split_whitespace()
                .map(|v| v.parse().unwrap_or(0))
                .collect();
            // Anything shorter than the documented 16 columns is not a row we can trust.
            if values.len() < 16 {
                return None;
            }
            Some(NetworkInterface {
                name: name.to_owned(),
                rx_bytes: values[0],
                tx_bytes: values[8],
                rx_errors: values[2],
                tx_errors: values[10],
            })
        })
        .filter(|iface| is_reportable(&iface.name))
        .collect()
}

/// Whether an interface should count towards network activity.
pub fn is_reportable(name: &str) -> bool {
    !EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Sums received and transmitted bytes across interfaces.
pub fn totals(interfaces: &[NetworkInterface]) -> (u64, u64) {
    interfaces.iter().fold((0, 0), |(rx, tx), iface| {
        (
            rx.saturating_add(iface.rx_bytes),
            tx.saturating_add(iface.tx_bytes),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1234567    1234    0    0    0     0          0         0  1234567    1234    0    0    0     0       0          0
  eth0: 987654321  123456    3    0    0     0          0         0 87654321   98765    1    0    0     0       0          0
  eth1: 111111111   11111    0    0    0     0          0         0 22222222   22222    0    0    0     0       0          0
docker0:  555555     555    0    0    0     0          0         0   666666     666    0    0    0     0       0          0
vethabc123: 777    7    0    0    0     0          0         0      888       8    0    0    0     0       0          0";

    #[test]
    fn physical_interfaces_are_parsed() {
        let interfaces = parse_net_dev(NET_DEV);
        let names: Vec<&str> = interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["eth0", "eth1"]);
    }

    #[test]
    fn receive_and_transmit_columns_are_not_transposed() {
        let interfaces = parse_net_dev(NET_DEV);
        let eth0 = interfaces
            .iter()
            .find(|i| i.name == "eth0")
            .expect("eth0 present");
        assert_eq!(eth0.rx_bytes, 987_654_321);
        assert_eq!(eth0.tx_bytes, 87_654_321);
        assert_eq!(eth0.rx_errors, 3);
        assert_eq!(eth0.tx_errors, 1);
    }

    #[test]
    fn loopback_and_virtual_interfaces_are_excluded() {
        let interfaces = parse_net_dev(NET_DEV);
        assert!(interfaces.iter().all(|i| i.name != "lo"));
        assert!(interfaces.iter().all(|i| i.name != "docker0"));
        assert!(interfaces.iter().all(|i| !i.name.starts_with("veth")));
    }

    #[test]
    fn header_lines_are_not_mistaken_for_interfaces() {
        let interfaces = parse_net_dev(NET_DEV);
        assert!(interfaces.iter().all(|i| i.name != "face"));
        assert!(interfaces.iter().all(|i| !i.name.starts_with("Inter")));
    }

    #[test]
    fn totals_sum_across_interfaces() {
        let interfaces = parse_net_dev(NET_DEV);
        let (rx, tx) = totals(&interfaces);
        assert_eq!(rx, 987_654_321 + 111_111_111);
        assert_eq!(tx, 87_654_321 + 22_222_222);
    }

    #[test]
    fn truncated_rows_are_ignored() {
        let text = "  eth0: 100 200 0";
        assert!(parse_net_dev(text).is_empty());
    }

    #[test]
    fn an_interface_name_with_no_space_before_the_colon_still_parses() {
        // Long interface names run into the colon with no separating space.
        let text = "enp0s31f6:987654321 123456 0 0 0 0 0 0 87654321 98765 0 0 0 0 0 0";
        let interfaces = parse_net_dev(text);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "enp0s31f6");
        assert_eq!(interfaces[0].rx_bytes, 987_654_321);
    }

    #[test]
    fn a_host_with_only_virtual_interfaces_yields_nothing_without_error() {
        let text = "    lo: 1 2 0 0 0 0 0 0 3 4 0 0 0 0 0 0";
        let output = NetworkCollector
            .parse(&[Ok(CommandOutput::success(text))])
            .expect("parses");
        let CollectorOutput::Network(interfaces) = output else {
            panic!("expected network")
        };
        assert!(interfaces.is_empty());
    }

    #[test]
    fn wireless_and_predictable_names_are_reportable() {
        assert!(is_reportable("eth0"));
        assert!(is_reportable("enp3s0"));
        assert!(is_reportable("wlan0"));
        assert!(is_reportable("ens160"));
        assert!(!is_reportable("lo"));
        assert!(!is_reportable("br-1a2b3c"));
    }
}
