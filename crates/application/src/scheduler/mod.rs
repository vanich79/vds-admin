//! The one scheduling framework.
//!
//! There are no ad-hoc `loop { sleep }` tasks anywhere in this codebase. Everything
//! recurring — server collection, website checks, analytics refresh, screenshots,
//! rollups, retention — is a [`Task`] registered here, so that priorities, concurrency
//! limits, backoff, deduplication and shutdown are implemented once and obeyed by
//! everything.
//!
//! The interesting logic lives in [`ScheduleQueue`], which is pure and heavily tested.
//! This module is the thin async driver around it.

mod backoff;
mod priority;
mod queue;
mod rate_limit;

pub use backoff::BackoffPolicy;
pub use priority::Priority;
pub use queue::{ConcurrencyLimits, JobKey, JobOutcome, ScheduleQueue, ScheduledJob};
pub use rate_limit::{RateDecision, RateLimitManager};

use async_trait::async_trait;
use chrono::Duration;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use vds_domain::ports::Clock;

/// A unit of recurring work.
#[async_trait]
pub trait Task: Send + Sync {
    /// Stable identity, also used to deduplicate in-flight runs.
    fn key(&self) -> JobKey;

    fn priority(&self) -> Priority;

    /// Runs once.
    ///
    /// Implementations must return promptly when `cancel` is triggered; the scheduler
    /// will wait for them during shutdown.
    async fn run(&self, cancel: CancellationToken) -> JobOutcome;
}

/// Source of jitter for backoff.
///
/// A trait so tests can make backoff deterministic without a fake RNG crate.
pub trait Jitter: Send + Sync {
    /// A value in `0.0..=1.0`.
    fn fraction(&self) -> f64;
}

/// Jitter derived from the system clock's sub-second component.
///
/// Good enough for spreading retries — this is load-shaping, not cryptography — and it
/// avoids a random-number dependency in the application layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockJitter;

impl Jitter for ClockJitter {
    fn fraction(&self) -> f64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        f64::from(nanos) / 1_000_000_000.0
    }
}

/// Fixed jitter, for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(pub f64);

impl Jitter for FixedJitter {
    fn fraction(&self) -> f64 {
        self.0
    }
}

/// Longest the driver will sleep before re-examining the queue.
///
/// Bounded so that a newly registered task is picked up promptly even if the queue was
/// otherwise idle for hours.
const MAX_SLEEP: Duration = Duration::seconds(5);

/// Runs registered tasks according to their priority, interval and backoff.
pub struct Scheduler {
    queue: Arc<Mutex<ScheduleQueue>>,
    tasks: Arc<Mutex<HashMap<JobKey, Arc<dyn Task>>>>,
    limits: ConcurrencyLimits,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn Jitter>,
    cancel: CancellationToken,
    /// Notified whenever the queue changes, so the driver re-evaluates immediately
    /// instead of sleeping through a newly due job.
    wake: Arc<tokio::sync::Notify>,
}

impl Scheduler {
    pub fn new(clock: Arc<dyn Clock>, limits: ConcurrencyLimits) -> Self {
        Self {
            queue: Arc::new(Mutex::new(ScheduleQueue::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            limits,
            clock,
            jitter: Arc::new(ClockJitter),
            cancel: CancellationToken::new(),
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Replaces the jitter source. Used by tests to make backoff deterministic.
    pub fn with_jitter(mut self, jitter: Arc<dyn Jitter>) -> Self {
        self.jitter = jitter;
        self
    }

    /// Token that stops the scheduler and every running task.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Registers a task, or updates the schedule of one already registered.
    pub fn register(&self, task: Arc<dyn Task>, interval: Duration, backoff: BackoffPolicy) {
        let key = task.key();
        let job = ScheduledJob::new(key.clone(), task.priority(), interval, self.clock.now())
            .with_backoff(backoff);

        self.tasks.lock().insert(key, task);
        self.queue.lock().upsert(job);
        self.wake.notify_one();
    }

    /// Registers a task that starts after an initial delay.
    ///
    /// Used to stagger the first run of a large fleet so that adding two hundred servers
    /// does not produce two hundred simultaneous connections at startup.
    pub fn register_delayed(
        &self,
        task: Arc<dyn Task>,
        interval: Duration,
        backoff: BackoffPolicy,
        initial_delay: Duration,
    ) {
        let key = task.key();
        let mut job = ScheduledJob::new(key.clone(), task.priority(), interval, self.clock.now())
            .with_backoff(backoff);
        job.due_at = self.clock.now() + initial_delay;

        self.tasks.lock().insert(key, task);
        self.queue.lock().upsert(job);
        self.wake.notify_one();
    }

    /// Removes a task. A run already in flight is left to finish.
    pub fn unregister(&self, key: &JobKey) {
        self.tasks.lock().remove(key);
        self.queue.lock().remove(key);
        self.wake.notify_one();
    }

    /// Makes a task due immediately. Returns false if it is already running.
    pub fn trigger(&self, key: &JobKey) -> bool {
        let triggered = self.queue.lock().trigger_now(key, self.clock.now());
        if triggered {
            self.wake.notify_one();
        }
        triggered
    }

    pub fn registered_count(&self) -> usize {
        self.queue.lock().len()
    }

    pub fn running_count(&self) -> usize {
        self.queue.lock().running_count()
    }

    /// Every job's state, for the debug panel.
    pub fn snapshot(&self) -> Vec<ScheduledJob> {
        self.queue.lock().snapshot()
    }

    /// Runs one scheduling pass: claims what is ready and spawns it.
    ///
    /// Returns the number of tasks started. Exposed separately from [`Scheduler::run`]
    /// so tests can step the scheduler deterministically.
    pub fn tick(&self) -> usize {
        let now = self.clock.now();
        let ready = self.queue.lock().claim_ready(now, &self.limits);

        for key in &ready {
            let Some(task) = self.tasks.lock().get(key).cloned() else {
                // Unregistered between claim and spawn; release the slot.
                self.queue
                    .lock()
                    .complete(key, JobOutcome::Skipped, now, 0.0);
                continue;
            };

            let queue = Arc::clone(&self.queue);
            let clock = Arc::clone(&self.clock);
            let jitter = Arc::clone(&self.jitter);
            let wake = Arc::clone(&self.wake);
            let cancel = self.cancel.child_token();
            let key = key.clone();

            tokio::spawn(async move {
                let outcome = task.run(cancel).await;
                if let JobOutcome::Retry(reason) | JobOutcome::Permanent(reason) = &outcome {
                    tracing::debug!(job = %key, reason = %reason, "task failed");
                }
                queue
                    .lock()
                    .complete(&key, outcome, clock.now(), jitter.fraction());
                wake.notify_one();
            });
        }

        ready.len()
    }

    /// Drives the scheduler until cancelled.
    ///
    /// Shutdown is graceful: the loop stops claiming new work, cancels running tasks
    /// through their child tokens, and waits — bounded by `drain_timeout` — for them to
    /// finish before returning.
    pub async fn run(&self, drain_timeout: std::time::Duration) {
        loop {
            self.tick();

            let sleep_for = {
                let queue = self.queue.lock();
                queue
                    .time_until_next(self.clock.now())
                    .unwrap_or(MAX_SLEEP)
                    .min(MAX_SLEEP)
                    .max(Duration::zero())
            };
            let sleep_for = sleep_for.to_std().unwrap_or(std::time::Duration::ZERO);

            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(sleep_for.max(std::time::Duration::from_millis(10))) => {}
            }
        }

        self.drain(drain_timeout).await;
    }

    /// Waits for in-flight tasks to finish, up to a timeout.
    async fn drain(&self, timeout: std::time::Duration) {
        // Cancelling the parent token has already signalled every child.
        self.cancel.cancel();

        let deadline = tokio::time::Instant::now() + timeout;
        while self.running_count() > 0 {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    in_flight = self.running_count(),
                    "shutting down with tasks still running"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Stops the scheduler.
    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.wake.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vds_domain::ports::FixedClock;

    /// A task that counts its runs and returns a scripted outcome.
    struct CountingTask {
        key: JobKey,
        priority: Priority,
        runs: Arc<AtomicUsize>,
        outcome: JobOutcome,
        /// Blocks until released, to test in-flight behaviour.
        gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl CountingTask {
        fn new(key: &str, priority: Priority) -> (Arc<Self>, Arc<AtomicUsize>) {
            let runs = Arc::new(AtomicUsize::new(0));
            let task = Arc::new(Self {
                key: JobKey::new(key),
                priority,
                runs: Arc::clone(&runs),
                outcome: JobOutcome::Success,
                gate: None,
            });
            (task, runs)
        }

        fn failing(key: &str) -> Arc<Self> {
            Arc::new(Self {
                key: JobKey::new(key),
                priority: Priority::CoreMetrics,
                runs: Arc::new(AtomicUsize::new(0)),
                outcome: JobOutcome::Retry("boom".into()),
                gate: None,
            })
        }

        fn gated(key: &str, gate: Arc<tokio::sync::Semaphore>) -> Arc<Self> {
            Arc::new(Self {
                key: JobKey::new(key),
                priority: Priority::CoreMetrics,
                runs: Arc::new(AtomicUsize::new(0)),
                outcome: JobOutcome::Success,
                gate: Some(gate),
            })
        }
    }

    #[async_trait]
    impl Task for CountingTask {
        fn key(&self) -> JobKey {
            self.key.clone()
        }

        fn priority(&self) -> Priority {
            self.priority
        }

        async fn run(&self, cancel: CancellationToken) -> JobOutcome {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                tokio::select! {
                    permit = gate.acquire() => { drop(permit); }
                    _ = cancel.cancelled() => return JobOutcome::Skipped,
                }
            }
            self.outcome.clone()
        }
    }

    fn scheduler(clock: FixedClock, limits: ConcurrencyLimits) -> Scheduler {
        Scheduler::new(Arc::new(clock), limits).with_jitter(Arc::new(FixedJitter(0.5)))
    }

    fn generous() -> ConcurrencyLimits {
        ConcurrencyLimits {
            global: 100,
            per_priority: HashMap::new(),
        }
    }

    /// Lets spawned tasks make progress.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    #[tokio::test]
    async fn a_registered_task_runs() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let (task, runs) = CountingTask::new("a", Priority::CoreMetrics);

        scheduler.register(task, Duration::seconds(30), BackoffPolicy::default());
        assert_eq!(scheduler.tick(), 1);
        settle().await;

        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(scheduler.running_count(), 0);
    }

    #[tokio::test]
    async fn a_task_does_not_run_again_until_its_interval_elapses() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let (task, runs) = CountingTask::new("a", Priority::CoreMetrics);

        scheduler.register(task, Duration::seconds(30), BackoffPolicy::default());
        scheduler.tick();
        settle().await;

        clock.advance(Duration::seconds(10));
        assert_eq!(scheduler.tick(), 0);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        clock.advance(Duration::seconds(25));
        assert_eq!(scheduler.tick(), 1);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_task_already_running_is_not_started_again() {
        // The deduplication guarantee: a slow SSH collection must not pile up.
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let task = CountingTask::gated("slow", Arc::clone(&gate));
        let runs = Arc::clone(&task.runs);

        scheduler.register(task, Duration::seconds(1), BackoffPolicy::default());
        scheduler.tick();
        settle().await;
        assert_eq!(scheduler.running_count(), 1);

        // Far past the interval, but it is still in flight.
        clock.advance(Duration::seconds(600));
        assert_eq!(scheduler.tick(), 0);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        gate.add_permits(1);
        settle().await;
        assert_eq!(scheduler.running_count(), 0);
    }

    #[tokio::test]
    async fn failures_are_backed_off() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let task = CountingTask::failing("flaky");
        let runs = Arc::clone(&task.runs);

        scheduler.register(
            task,
            Duration::seconds(10),
            BackoffPolicy {
                initial_secs: 60,
                max_secs: 600,
                multiplier: 2,
                jitter_percent: 0,
            },
        );

        scheduler.tick();
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // The normal interval has passed, but backoff pushed the next run to +60s.
        clock.advance(Duration::seconds(30));
        assert_eq!(scheduler.tick(), 0);

        clock.advance(Duration::seconds(31));
        assert_eq!(scheduler.tick(), 1);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn priority_decides_who_runs_when_slots_are_scarce() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let limits = ConcurrencyLimits {
            global: 1,
            per_priority: HashMap::new(),
        };
        let scheduler = scheduler(clock.clone(), limits);

        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let shot = CountingTask::gated("screenshot", Arc::clone(&gate));
        let shot_runs = Arc::clone(&shot.runs);
        let (alert, alert_runs) = CountingTask::new("alert", Priority::CriticalAlert);

        // Register the screenshot first, so ordering by priority is what decides.
        scheduler.register(
            Arc::new(CountingTask {
                key: JobKey::new("screenshot"),
                priority: Priority::Screenshots,
                runs: Arc::clone(&shot_runs),
                outcome: JobOutcome::Success,
                gate: Some(Arc::clone(&gate)),
            }),
            Duration::seconds(3_600),
            BackoffPolicy::default(),
        );
        drop(shot);
        scheduler.register(alert, Duration::seconds(10), BackoffPolicy::default());

        scheduler.tick();
        settle().await;

        assert_eq!(
            alert_runs.load(Ordering::SeqCst),
            1,
            "the alert must win the slot"
        );
        assert_eq!(shot_runs.load(Ordering::SeqCst), 0);
        gate.add_permits(10);
    }

    #[tokio::test]
    async fn a_manual_trigger_runs_the_task_now() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let (task, runs) = CountingTask::new("a", Priority::Analytics);

        scheduler.register(task, Duration::hours(1), BackoffPolicy::default());
        scheduler.tick();
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Nowhere near due again.
        clock.advance(Duration::seconds(5));
        assert_eq!(scheduler.tick(), 0);

        assert!(scheduler.trigger(&JobKey::new("a")));
        assert_eq!(scheduler.tick(), 1);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn unregistering_stops_future_runs() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let (task, runs) = CountingTask::new("a", Priority::CoreMetrics);

        scheduler.register(task, Duration::seconds(10), BackoffPolicy::default());
        scheduler.tick();
        settle().await;

        scheduler.unregister(&JobKey::new("a"));
        clock.advance(Duration::seconds(100));
        assert_eq!(scheduler.tick(), 0);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(scheduler.registered_count(), 0);
    }

    #[tokio::test]
    async fn a_staggered_registration_does_not_run_immediately() {
        // Adding 200 servers must not open 200 SSH connections in the same instant.
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let (task, runs) = CountingTask::new("a", Priority::ServerAvailability);

        scheduler.register_delayed(
            task,
            Duration::seconds(30),
            BackoffPolicy::default(),
            Duration::seconds(20),
        );

        assert_eq!(scheduler.tick(), 0);
        clock.advance(Duration::seconds(21));
        assert_eq!(scheduler.tick(), 1);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_reaches_running_tasks() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock.clone(), generous());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let task = CountingTask::gated("slow", gate);

        scheduler.register(task, Duration::seconds(10), BackoffPolicy::default());
        scheduler.tick();
        settle().await;
        assert_eq!(scheduler.running_count(), 1);

        // The task is blocked on a gate that is never released; cancellation is the only
        // thing that can free it.
        scheduler.shutdown();
        settle().await;
        assert_eq!(scheduler.running_count(), 0);
    }

    #[tokio::test]
    async fn the_run_loop_exits_when_cancelled() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = Arc::new(scheduler(clock, generous()));
        let (task, _) = CountingTask::new("a", Priority::CoreMetrics);
        scheduler.register(task, Duration::seconds(30), BackoffPolicy::default());

        let driver = Arc::clone(&scheduler);
        let handle = tokio::spawn(async move {
            driver.run(std::time::Duration::from_millis(200)).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        scheduler.shutdown();

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("the run loop must exit promptly")
            .expect("driver task did not panic");
    }

    #[tokio::test]
    async fn the_snapshot_reports_state_for_the_debug_panel() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        let scheduler = scheduler(clock, generous());
        let (task, _) = CountingTask::new("a", Priority::CoreMetrics);
        scheduler.register(task, Duration::seconds(30), BackoffPolicy::default());
        scheduler.tick();
        settle().await;

        let snapshot = scheduler.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].last_outcome, Some(JobOutcome::Success));
        assert!(snapshot[0].last_started.is_some());
    }
}
