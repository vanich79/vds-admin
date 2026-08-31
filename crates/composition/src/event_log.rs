//! Connecting the event port to the event table.
//!
//! [`EventPublisher`] is a domain port; [`EventRepository`] is infrastructure. Joining
//! them is the composition root's job, and until this existed the join was missing: every
//! producer published dutifully into [`NullEventPublisher`](vds_domain::ports::NullEventPublisher),
//! which discards. The events table stayed empty, the activity feed stayed blank, and a
//! failed analytics refresh reported itself to nobody.
//!
//! # Why a channel rather than a direct write
//!
//! The port's contract is explicit: publication is infallible and non-blocking, and a
//! lagging subscriber loses events rather than slowing the producer. A monitoring cycle
//! must not stall on a disk write, and `publish` is not `async`, so it could not await one
//! anyway. So `publish` hands the event to a bounded channel and returns; a background
//! task does the writing.
//!
//! Bounded, and dropping when full, for the reason the port gives. A queue that grew
//! without limit would turn a slow disk into exhausted memory — the failure that takes the
//! whole application with it instead of costing it a few log lines.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use vds_domain::events::{DomainEvent, EventEnvelope};
use vds_domain::ports::{Clock, EventPublisher, EventRepository};

/// How many events may be waiting to be written.
///
/// A monitoring cycle over a few hundred subjects produces events in bursts, and the
/// writer drains them in milliseconds. This is slack for the burst, not a buffer anyone
/// should be relying on.
const CAPACITY: usize = 1024;

/// Publishes domain events into the event log.
pub struct PersistentEventPublisher {
    sender: mpsc::Sender<DomainEvent>,
    /// Events discarded because the writer fell behind.
    ///
    /// Counted rather than ignored: an event log that quietly has holes in it is worse
    /// than one that admits to them, and this is the only way anyone would find out.
    dropped: AtomicU64,
}

impl PersistentEventPublisher {
    /// Creates the publisher and the receiving half of its channel.
    ///
    /// Two pieces because of the order things are built in: the publisher has to exist
    /// before the application is assembled — producers take it as a dependency — and the
    /// repository it writes to only exists afterwards. [`spawn_writer`] joins them once
    /// both are available.
    pub fn new() -> (Arc<Self>, EventLogReceiver) {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        (
            Arc::new(Self {
                sender,
                dropped: AtomicU64::new(0),
            }),
            EventLogReceiver { receiver },
        )
    }

    /// How many events have been discarded for want of a writer.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl EventPublisher for PersistentEventPublisher {
    fn publish(&self, event: DomainEvent) {
        // `try_send` rather than `send`: this is called from a monitoring cycle, which
        // must not wait for a disk.
        if self.sender.try_send(event).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Only the first, and then powers of ten. A full queue produces a great many
            // of these, and filling the log with them helps nobody.
            if dropped == 1 || dropped.is_power_of_two() {
                tracing::warn!(dropped, "the event log is behind; events are being dropped");
            }
        }
    }
}

impl std::fmt::Debug for PersistentEventPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentEventPublisher")
            .field("dropped", &self.dropped())
            .finish()
    }
}

/// The receiving half, waiting to be given a repository.
pub struct EventLogReceiver {
    receiver: mpsc::Receiver<DomainEvent>,
}

/// Writes published events to the repository until the publisher is dropped.
///
/// Returns a future for the caller to spawn rather than spawning one itself. That is not
/// a stylistic preference: `tokio::spawn` panics unless it is called from inside a runtime
/// context, and the natural call site here is `main`, which owns a runtime without having
/// entered it. A function that quietly requires an ambient context is one that compiles
/// everywhere and crashes in exactly one place.
pub async fn write_events(
    receiver: EventLogReceiver,
    repository: Arc<dyn EventRepository>,
    clock: Arc<dyn Clock>,
) {
    let mut receiver = receiver.receiver;
    while let Some(event) = receiver.recv().await {
        let envelope = EventEnvelope::new(event, clock.now());
        if let Err(error) = repository.append(&envelope).await {
            // Logged and dropped. Retrying would hold up every event behind it, and the
            // event log is a record of what happened, not part of what happens.
            tracing::warn!(%error, kind = envelope.event.kind(), "could not record an event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use vds_domain::ids::ServerId;
    use vds_domain::metrics::TimeWindow;
    use vds_domain::ports::{RepositoryError, SystemClock};

    #[derive(Default)]
    struct SpyRepository {
        appended: Mutex<Vec<EventEnvelope>>,
        fail: Mutex<bool>,
    }

    #[async_trait]
    impl EventRepository for SpyRepository {
        async fn append(&self, event: &EventEnvelope) -> Result<(), RepositoryError> {
            if *self.fail.lock().expect("test mutex") {
                return Err(RepositoryError::Backend("disk is on fire".into()));
            }
            self.appended
                .lock()
                .expect("test mutex")
                .push(event.clone());
            Ok(())
        }

        async fn recent(&self, _: u32) -> Result<Vec<EventEnvelope>, RepositoryError> {
            Ok(self.appended.lock().expect("test mutex").clone())
        }

        async fn recent_for_subject(
            &self,
            _: vds_domain::events::AlertSubject,
            _: u32,
        ) -> Result<Vec<EventEnvelope>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn in_window(&self, _: TimeWindow) -> Result<Vec<EventEnvelope>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn prune(&self, _: chrono::DateTime<chrono::Utc>) -> Result<u64, RepositoryError> {
            Ok(0)
        }
    }

    fn an_event() -> DomainEvent {
        DomainEvent::ServerMetricsCollected {
            server_id: ServerId::new(),
            metric_count: 12,
        }
    }

    #[tokio::test]
    async fn a_published_event_reaches_the_repository() {
        // The join that was missing: every producer published into a publisher that
        // discarded, so the events table was empty however much happened.
        let (publisher, receiver) = PersistentEventPublisher::new();
        let repository = Arc::new(SpyRepository::default());
        let writer = tokio::spawn(write_events(
            receiver,
            Arc::clone(&repository) as Arc<dyn EventRepository>,
            Arc::new(SystemClock),
        ));

        publisher.publish(an_event());
        publisher.publish(an_event());
        drop(publisher);
        writer
            .await
            .expect("the writer finishes when the channel closes");

        assert_eq!(repository.appended.lock().expect("test mutex").len(), 2);
    }

    #[tokio::test]
    async fn publishing_does_not_wait_for_the_disk() {
        // `publish` is called from inside a monitoring cycle. If it blocked, one slow
        // write would delay every server's collection.
        let (publisher, _receiver) = PersistentEventPublisher::new();

        let started = std::time::Instant::now();
        for _ in 0..100 {
            publisher.publish(an_event());
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_backlog_is_dropped_and_counted_rather_than_grown() {
        // The port's contract: a lagging subscriber loses events rather than slowing the
        // producer. An unbounded queue would turn a slow disk into exhausted memory.
        let (publisher, _receiver) = PersistentEventPublisher::new();

        for _ in 0..(CAPACITY + 50) {
            publisher.publish(an_event());
        }

        assert_eq!(publisher.dropped(), 50, "a full queue must drop, not grow");
    }

    #[tokio::test]
    async fn a_failed_write_does_not_stop_the_ones_behind_it() {
        // The event log records what happened; it is not part of what happens. One
        // rejected row must not silence everything after it.
        let (publisher, receiver) = PersistentEventPublisher::new();
        let repository = Arc::new(SpyRepository::default());
        *repository.fail.lock().expect("test mutex") = true;

        let writer = tokio::spawn(write_events(
            receiver,
            Arc::clone(&repository) as Arc<dyn EventRepository>,
            Arc::new(SystemClock),
        ));

        publisher.publish(an_event());
        // Let the writer take the failing one before the disk "recovers".
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        *repository.fail.lock().expect("test mutex") = false;
        publisher.publish(an_event());

        drop(publisher);
        writer.await.expect("the writer survives a failed write");

        assert_eq!(repository.appended.lock().expect("test mutex").len(), 1);
    }
}
