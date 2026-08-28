//! The event publication port.
//!
//! Producers depend on this trait only. They never know who is listening, which is the
//! property that lets notifications, audit logging, webhooks and correlation be added
//! as subscribers rather than as edits to the monitoring code.

use crate::events::DomainEvent;

/// Publishes domain events.
///
/// Publication is deliberately infallible and non-blocking: a monitoring cycle must not
/// fail, or stall, because nobody is listening or because a subscriber is slow.
/// Implementations drop events for lagging subscribers rather than applying
/// backpressure to the producer.
pub trait EventPublisher: Send + Sync {
    fn publish(&self, event: DomainEvent);
}

/// An publisher that discards everything, for tests and for headless runs that have no
/// subscribers.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEventPublisher;

impl EventPublisher for NullEventPublisher {
    fn publish(&self, _event: DomainEvent) {}
}

/// Collects published events in memory so tests can assert on them.
#[derive(Debug, Clone, Default)]
pub struct RecordingEventPublisher {
    events: std::sync::Arc<std::sync::Mutex<Vec<DomainEvent>>>,
}

impl RecordingEventPublisher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<DomainEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.events.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.events.lock() {
            guard.clear();
        }
    }

    /// Whether an event matching the predicate was published.
    pub fn contains(&self, predicate: impl Fn(&DomainEvent) -> bool) -> bool {
        self.events().iter().any(predicate)
    }
}

impl EventPublisher for RecordingEventPublisher {
    fn publish(&self, event: DomainEvent) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ServerId;
    use crate::status::Status;

    #[test]
    fn the_recording_publisher_captures_events_in_order() {
        let publisher = RecordingEventPublisher::new();
        assert!(publisher.is_empty());

        publisher.publish(DomainEvent::ServerStatusChanged {
            server_id: ServerId::new(),
            from: Status::Unknown,
            to: Status::Healthy,
            reason: None,
        });
        publisher.publish(DomainEvent::ServerMetricsCollected {
            server_id: ServerId::new(),
            metric_count: 3,
        });

        assert_eq!(publisher.len(), 2);
        assert!(publisher.contains(|e| e.kind() == "server_metrics_collected"));
        assert_eq!(publisher.events()[0].kind(), "server_status_changed");

        publisher.clear();
        assert!(publisher.is_empty());
    }

    #[test]
    fn the_null_publisher_accepts_and_forgets() {
        let publisher = NullEventPublisher;
        publisher.publish(DomainEvent::ScreenshotUpdated {
            website_id: crate::ids::WebsiteId::new(),
        });
    }
}
