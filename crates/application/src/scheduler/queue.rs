//! The scheduling queue: which job runs next, and when.
//!
//! Deliberately pure — no clock, no tasks, no I/O. Every scheduling decision the system
//! makes is a function of `(entries, now, limits)`, which is what allows fleet-scale
//! behaviour to be asserted in microseconds instead of observed over minutes.

use super::backoff::BackoffPolicy;
use super::priority::Priority;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

/// How long a permanently failing job waits before it is tried again.
///
/// Chosen against the thresholds it has to stay under rather than picked for feel:
/// fail2ban's shipped `sshd` jail bans after 5 failures in 10 minutes, and PAM lockouts
/// are usually configured in the same range. Four attempts an hour is comfortably clear
/// of both, and still recovers on its own within a quarter of an hour once the cause is
/// fixed.
const PERMANENT_RETRY: Duration = Duration::minutes(15);

/// Identifies a recurring job. Also the deduplication key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobKey(String);

impl JobKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a job run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// Ran and succeeded; the failure streak resets.
    Success,
    /// Failed in a way that retrying may fix; backoff applies.
    Retry(String),
    /// Failed in a way retrying cannot fix — bad credentials, invalid configuration.
    ///
    /// Retried on a long fixed interval rather than the polling one. Repeating a rejected
    /// credential every thirty seconds is not merely useless: it is how the application
    /// gets its own address banned by the server's brute-force protection, after which
    /// the server looks offline for a reason that has nothing to do with the server.
    ///
    /// The interval is fixed rather than exponential because the condition needs a human
    /// either way, and a user who fixes it should not have to wait out a backoff that has
    /// grown to hours. Pressing refresh runs the job immediately regardless.
    Permanent(String),
    /// Skipped because a precondition was not met; not a failure.
    Skipped,
}

impl JobOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, JobOutcome::Retry(_) | JobOutcome::Permanent(_))
    }
}

/// A registered recurring job.
#[derive(Debug, Clone)]
pub struct ScheduledJob {
    pub key: JobKey,
    pub priority: Priority,
    /// Normal gap between runs.
    pub interval: Duration,
    pub backoff: BackoffPolicy,
    /// When this job may next start.
    pub due_at: DateTime<Utc>,
    /// Consecutive failures, driving backoff.
    pub consecutive_failures: u32,
    /// True while a run is in flight, which is what deduplicates work.
    pub running: bool,
    pub last_started: Option<DateTime<Utc>>,
    pub last_finished: Option<DateTime<Utc>>,
    pub last_outcome: Option<JobOutcome>,
}

impl ScheduledJob {
    pub fn new(
        key: JobKey,
        priority: Priority,
        interval: Duration,
        first_run: DateTime<Utc>,
    ) -> Self {
        Self {
            key,
            priority,
            interval,
            backoff: BackoffPolicy::default(),
            due_at: first_run,
            consecutive_failures: 0,
            running: false,
            last_started: None,
            last_finished: None,
            last_outcome: None,
        }
    }

    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn is_ready(&self, now: DateTime<Utc>) -> bool {
        !self.running && self.due_at <= now
    }
}

/// Concurrency ceilings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrencyLimits {
    /// Ceiling across all jobs.
    pub global: usize,
    /// Per-priority ceilings. A priority not listed is bounded only by `global`.
    pub per_priority: HashMap<Priority, usize>,
}

impl Default for ConcurrencyLimits {
    fn default() -> Self {
        let mut per_priority = HashMap::new();
        per_priority.insert(Priority::ServerAvailability, 16);
        per_priority.insert(Priority::WebsiteAvailability, 32);
        per_priority.insert(Priority::CoreMetrics, 16);
        per_priority.insert(Priority::Analytics, 4);
        // A headless browser is expensive; never more than a couple at a time.
        per_priority.insert(Priority::Screenshots, 2);
        per_priority.insert(Priority::Maintenance, 1);
        Self {
            global: 48,
            per_priority,
        }
    }
}

impl ConcurrencyLimits {
    pub fn limit_for(&self, priority: Priority) -> usize {
        self.per_priority
            .get(&priority)
            .copied()
            .unwrap_or(self.global)
            .min(self.global)
    }
}

/// The set of scheduled jobs and the rules for picking what runs next.
#[derive(Debug, Default)]
pub struct ScheduleQueue {
    jobs: HashMap<JobKey, ScheduledJob>,
}

impl ScheduleQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a job.
    ///
    /// Replacing preserves the in-flight flag and failure streak of an existing entry:
    /// editing a server's polling interval must not cancel a running collection or
    /// silently forgive an outage.
    pub fn upsert(&mut self, job: ScheduledJob) {
        match self.jobs.get_mut(&job.key) {
            Some(existing) => {
                existing.priority = job.priority;
                existing.interval = job.interval;
                existing.backoff = job.backoff;
                // Only pull the due time earlier, never push it later, so a settings
                // change cannot indefinitely postpone a job.
                existing.due_at = existing.due_at.min(job.due_at);
            }
            None => {
                self.jobs.insert(job.key.clone(), job);
            }
        }
    }

    pub fn remove(&mut self, key: &JobKey) -> Option<ScheduledJob> {
        self.jobs.remove(key)
    }

    pub fn get(&self, key: &JobKey) -> Option<&ScheduledJob> {
        self.jobs.get(key)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &JobKey> {
        self.jobs.keys()
    }

    /// How many jobs are currently in flight.
    pub fn running_count(&self) -> usize {
        self.jobs.values().filter(|j| j.running).count()
    }

    fn running_count_for(&self, priority: Priority) -> usize {
        self.jobs
            .values()
            .filter(|j| j.running && j.priority == priority)
            .count()
    }

    /// Picks the jobs that should start now, marking them as running.
    ///
    /// Selection is strictly by priority, then by how overdue a job is. That ordering is
    /// the whole point: when the machine cannot keep up, alert evaluation and
    /// availability checks must still get through while screenshots wait.
    pub fn claim_ready(&mut self, now: DateTime<Utc>, limits: &ConcurrencyLimits) -> Vec<JobKey> {
        let mut available_global = limits.global.saturating_sub(self.running_count());
        if available_global == 0 {
            return Vec::new();
        }

        let mut per_priority_used: HashMap<Priority, usize> = HashMap::new();
        for priority in Priority::ALL {
            per_priority_used.insert(*priority, self.running_count_for(*priority));
        }

        let mut candidates: Vec<(Priority, Duration, JobKey)> = self
            .jobs
            .values()
            .filter(|job| job.is_ready(now))
            .map(|job| (job.priority, now - job.due_at, job.key.clone()))
            .collect();

        // Highest priority first; within a priority, the most overdue first.
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });

        let mut claimed = Vec::new();
        for (priority, _, key) in candidates {
            if available_global == 0 {
                break;
            }
            let used = per_priority_used.entry(priority).or_insert(0);
            if *used >= limits.limit_for(priority) {
                continue;
            }
            if let Some(job) = self.jobs.get_mut(&key) {
                job.running = true;
                job.last_started = Some(now);
            }
            *used += 1;
            available_global -= 1;
            claimed.push(key);
        }

        claimed
    }

    /// Records a completed run and schedules the next one.
    ///
    /// `jitter_fraction` is supplied by the caller so this stays deterministic.
    pub fn complete(
        &mut self,
        key: &JobKey,
        outcome: JobOutcome,
        now: DateTime<Utc>,
        jitter_fraction: f64,
    ) {
        let Some(job) = self.jobs.get_mut(key) else {
            return;
        };

        job.running = false;
        job.last_finished = Some(now);

        let next_delay = match &outcome {
            JobOutcome::Success | JobOutcome::Skipped => {
                job.consecutive_failures = 0;
                job.interval
            }
            JobOutcome::Retry(_) => {
                job.consecutive_failures = job.consecutive_failures.saturating_add(1);
                let backoff = job.backoff.delay(job.consecutive_failures, jitter_fraction);
                // Backoff extends the wait; it never shortens it below the normal
                // interval, or a failing server would be polled *more* often than a
                // healthy one.
                backoff.max(job.interval)
            }
            JobOutcome::Permanent(_) => {
                job.consecutive_failures = job.consecutive_failures.saturating_add(1);
                // A permanent failure does not fix itself: a rejected key is rejected
                // again thirty seconds later. Repeating that on the normal interval is
                // how a monitoring application gets its own address banned — five failed
                // authentications inside ten minutes is the default fail2ban trigger, and
                // a 30-second interval reaches it in two and a half minutes. The server
                // then stops answering entirely and looks offline for a reason that has
                // nothing to do with the server.
                //
                // Retried rarely enough to stay far under any sane lockout threshold.
                // Recovery does not depend on this: fixing the credential and pressing
                // refresh runs the job at once, through `trigger`.
                PERMANENT_RETRY.max(job.interval)
            }
        };

        job.last_outcome = Some(outcome);
        job.due_at = now + next_delay;
    }

    /// Forces a job to become due immediately, for a manual refresh.
    ///
    /// Has no effect on a job already running, which is what stops a user from starting
    /// ten overlapping collections by clicking repeatedly.
    pub fn trigger_now(&mut self, key: &JobKey, now: DateTime<Utc>) -> bool {
        match self.jobs.get_mut(key) {
            Some(job) if !job.running => {
                job.due_at = now;
                true
            }
            _ => false,
        }
    }

    /// When the next job becomes due, ignoring concurrency.
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.jobs
            .values()
            .filter(|j| !j.running)
            .map(|j| j.due_at)
            .min()
    }

    /// How long to sleep before the next job is due, floored at zero.
    pub fn time_until_next(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.next_due().map(|due| (due - now).max(Duration::zero()))
    }

    /// Snapshot of every job, for the debug panel.
    pub fn snapshot(&self) -> Vec<ScheduledJob> {
        let mut jobs: Vec<ScheduledJob> = self.jobs.values().cloned().collect();
        jobs.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.key.cmp(&b.key)));
        jobs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn job(key: &str, priority: Priority, interval_secs: i64) -> ScheduledJob {
        ScheduledJob::new(
            JobKey::new(key),
            priority,
            Duration::seconds(interval_secs),
            at(0),
        )
    }

    fn generous_limits() -> ConcurrencyLimits {
        ConcurrencyLimits {
            global: 100,
            per_priority: HashMap::new(),
        }
    }

    #[test]
    fn a_due_job_is_claimed() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));
        let claimed = queue.claim_ready(at(0), &generous_limits());
        assert_eq!(claimed, vec![JobKey::new("a")]);
    }

    #[test]
    fn a_job_that_is_not_yet_due_is_left_alone() {
        let mut queue = ScheduleQueue::new();
        let mut entry = job("a", Priority::CoreMetrics, 30);
        entry.due_at = at(100);
        queue.upsert(entry);
        assert!(queue.claim_ready(at(50), &generous_limits()).is_empty());
        assert_eq!(
            queue.claim_ready(at(100), &generous_limits()),
            vec![JobKey::new("a")]
        );
    }

    #[test]
    fn a_running_job_is_never_claimed_twice() {
        // Request deduplication: the same server must not be collected concurrently.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));

        assert_eq!(queue.claim_ready(at(0), &generous_limits()).len(), 1);
        assert!(queue.claim_ready(at(1_000), &generous_limits()).is_empty());
    }

    #[test]
    fn higher_priority_work_is_claimed_first_under_pressure() {
        // The behaviour the priority list exists for: when only one slot is free, an
        // availability check beats a screenshot.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("shot", Priority::Screenshots, 3_600));
        queue.upsert(job("alert", Priority::CriticalAlert, 10));
        queue.upsert(job("site", Priority::WebsiteAvailability, 60));

        let limits = ConcurrencyLimits {
            global: 1,
            per_priority: HashMap::new(),
        };
        assert_eq!(
            queue.claim_ready(at(0), &limits),
            vec![JobKey::new("alert")]
        );
    }

    #[test]
    fn within_a_priority_the_most_overdue_job_wins() {
        let mut queue = ScheduleQueue::new();
        let mut recent = job("recent", Priority::CoreMetrics, 30);
        recent.due_at = at(90);
        let mut stale = job("stale", Priority::CoreMetrics, 30);
        stale.due_at = at(10);
        queue.upsert(recent);
        queue.upsert(stale);

        let limits = ConcurrencyLimits {
            global: 1,
            per_priority: HashMap::new(),
        };
        assert_eq!(
            queue.claim_ready(at(100), &limits),
            vec![JobKey::new("stale")]
        );
    }

    #[test]
    fn the_global_concurrency_ceiling_is_respected() {
        let mut queue = ScheduleQueue::new();
        for i in 0..10 {
            queue.upsert(job(&format!("job-{i}"), Priority::CoreMetrics, 30));
        }
        let limits = ConcurrencyLimits {
            global: 3,
            per_priority: HashMap::new(),
        };
        assert_eq!(queue.claim_ready(at(0), &limits).len(), 3);
        assert_eq!(queue.running_count(), 3);
        // Nothing more starts until something finishes.
        assert!(queue.claim_ready(at(0), &limits).is_empty());
    }

    #[test]
    fn a_per_priority_ceiling_stops_one_kind_of_work_starving_the_rest() {
        // Twenty screenshots must not consume every worker and stall availability
        // checks behind them.
        let mut queue = ScheduleQueue::new();
        for i in 0..20 {
            queue.upsert(job(&format!("shot-{i}"), Priority::Screenshots, 3_600));
        }
        queue.upsert(job("site", Priority::WebsiteAvailability, 60));

        let limits = ConcurrencyLimits::default();
        let claimed = queue.claim_ready(at(0), &limits);

        let screenshots = claimed
            .iter()
            .filter(|k| k.as_str().starts_with("shot-"))
            .count();
        assert_eq!(screenshots, 2, "screenshot concurrency must be capped");
        assert!(claimed.contains(&JobKey::new("site")));
    }

    #[test]
    fn a_thousand_servers_do_not_all_start_at_once() {
        let mut queue = ScheduleQueue::new();
        for i in 0..1_000 {
            queue.upsert(job(
                &format!("server-{i}"),
                Priority::ServerAvailability,
                30,
            ));
        }
        let limits = ConcurrencyLimits::default();
        let claimed = queue.claim_ready(at(0), &limits);
        assert_eq!(
            claimed.len(),
            limits.limit_for(Priority::ServerAvailability)
        );
        assert!(claimed.len() <= limits.global);
    }

    #[test]
    fn success_reschedules_at_the_normal_interval_and_clears_the_streak() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));
        queue.claim_ready(at(0), &generous_limits());
        queue.complete(
            &JobKey::new("a"),
            JobOutcome::Retry("boom".into()),
            at(1),
            0.5,
        );
        queue.complete(&JobKey::new("a"), JobOutcome::Success, at(2), 0.5);

        let entry = queue.get(&JobKey::new("a")).expect("job present");
        assert_eq!(entry.consecutive_failures, 0);
        assert_eq!(entry.due_at, at(32));
        assert!(!entry.running);
    }

    #[test]
    fn repeated_failures_back_off() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(
            job("a", Priority::ServerAvailability, 10).with_backoff(BackoffPolicy {
                initial_secs: 30,
                max_secs: 600,
                multiplier: 2,
                jitter_percent: 0,
            }),
        );
        let key = JobKey::new("a");

        queue.complete(&key, JobOutcome::Retry("1".into()), at(0), 0.5);
        assert_eq!(queue.get(&key).expect("present").due_at, at(30));

        queue.complete(&key, JobOutcome::Retry("2".into()), at(30), 0.5);
        assert_eq!(queue.get(&key).expect("present").due_at, at(90));

        queue.complete(&key, JobOutcome::Retry("3".into()), at(90), 0.5);
        assert_eq!(queue.get(&key).expect("present").due_at, at(210));
    }

    #[test]
    fn backoff_never_polls_a_failing_job_more_often_than_a_healthy_one() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(
            job("a", Priority::CoreMetrics, 300).with_backoff(BackoffPolicy {
                initial_secs: 5,
                max_secs: 60,
                multiplier: 2,
                jitter_percent: 0,
            }),
        );
        let key = JobKey::new("a");
        queue.complete(&key, JobOutcome::Retry("boom".into()), at(0), 0.5);
        // Backoff of 5s must not override the 300s interval.
        assert_eq!(queue.get(&key).expect("present").due_at, at(300));
    }

    #[test]
    fn a_permanent_failure_keeps_a_constant_cadence_rather_than_accelerating() {
        // The cadence must not shorten with each failure — but it is the *permanent*
        // interval, not the polling one. An earlier version of this test asserted the
        // polling interval and so locked in the behaviour that got a real server to ban
        // the monitoring host; see `a_permanent_failure_is_not_retried_on_the_normal_interval`.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::ServerAvailability, 60));
        let key = JobKey::new("a");

        queue.complete(
            &key,
            JobOutcome::Permanent("bad password".into()),
            at(0),
            0.5,
        );
        let first = queue.get(&key).expect("present").due_at;

        queue.complete(
            &key,
            JobOutcome::Permanent("bad password".into()),
            first,
            0.5,
        );
        let second = queue.get(&key).expect("present").due_at;

        assert_eq!(first - at(0), second - first, "the gap must not change");
        assert_eq!(first - at(0), Duration::minutes(15));
    }

    #[test]
    fn completing_an_unknown_job_is_a_no_op_rather_than_a_panic() {
        // Happens when a server is deleted while its collection is in flight.
        let mut queue = ScheduleQueue::new();
        queue.complete(&JobKey::new("ghost"), JobOutcome::Success, at(0), 0.5);
        assert!(queue.is_empty());
    }

    #[test]
    fn upserting_preserves_a_running_job_and_its_failure_streak() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));
        queue.claim_ready(at(0), &generous_limits());
        queue.complete(&JobKey::new("a"), JobOutcome::Retry("x".into()), at(1), 0.5);
        queue.claim_ready(at(1_000), &generous_limits());

        // Editing the server's settings mid-collection.
        queue.upsert(job("a", Priority::CoreMetrics, 15));

        let entry = queue.get(&JobKey::new("a")).expect("present");
        assert!(entry.running, "an in-flight run must not be forgotten");
        assert_eq!(entry.consecutive_failures, 1);
        assert_eq!(entry.interval, Duration::seconds(15));
    }

    #[test]
    fn a_manual_trigger_makes_a_job_due_immediately() {
        let mut queue = ScheduleQueue::new();
        let mut entry = job("a", Priority::Analytics, 900);
        entry.due_at = at(10_000);
        queue.upsert(entry);

        assert!(queue.trigger_now(&JobKey::new("a"), at(5)));
        assert_eq!(
            queue.claim_ready(at(5), &generous_limits()),
            vec![JobKey::new("a")]
        );
    }

    #[test]
    fn triggering_a_running_job_does_nothing() {
        // Clicking "refresh" five times must not start five collections.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::Analytics, 900));
        queue.claim_ready(at(0), &generous_limits());
        assert!(!queue.trigger_now(&JobKey::new("a"), at(1)));
        assert_eq!(queue.running_count(), 1);
    }

    #[test]
    fn the_next_wake_up_ignores_running_jobs() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("running", Priority::CoreMetrics, 30));
        let mut waiting = job("waiting", Priority::CoreMetrics, 30);
        waiting.due_at = at(500);
        queue.upsert(waiting);

        queue.claim_ready(at(0), &generous_limits());
        assert_eq!(queue.next_due(), Some(at(500)));
        assert_eq!(queue.time_until_next(at(100)), Some(Duration::seconds(400)));
    }

    #[test]
    fn time_until_next_never_goes_negative() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));
        assert_eq!(queue.time_until_next(at(9_999)), Some(Duration::zero()));
    }

    #[test]
    fn removing_a_job_takes_it_out_of_scheduling() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::CoreMetrics, 30));
        assert!(queue.remove(&JobKey::new("a")).is_some());
        assert!(queue.is_empty());
        assert_eq!(queue.next_due(), None);
    }

    #[test]
    fn the_snapshot_is_ordered_by_priority_for_the_debug_panel() {
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("z-shot", Priority::Screenshots, 3_600));
        queue.upsert(job("a-alert", Priority::CriticalAlert, 10));
        let snapshot = queue.snapshot();
        assert_eq!(snapshot[0].priority, Priority::CriticalAlert);
        assert_eq!(snapshot[1].priority, Priority::Screenshots);
    }

    #[test]
    fn a_permanent_failure_is_not_retried_on_the_normal_interval() {
        // Not a theoretical concern. A wrong SSH key retried every thirty seconds reaches
        // fail2ban's default sshd trigger — five failures in ten minutes — in two and a
        // half minutes. The server then refuses the monitoring host outright and appears
        // offline for a reason that has nothing to do with the server.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::ServerAvailability, 30));
        queue.claim_ready(at(0), &generous_limits());

        queue.complete(
            &JobKey::new("a"),
            JobOutcome::Permanent("authentication failed".into()),
            at(0),
            0.0,
        );

        let entry = queue.get(&JobKey::new("a")).expect("job present");
        assert!(
            entry.due_at - at(0) >= Duration::minutes(15),
            "retried after {}, fast enough to get the address banned",
            entry.due_at - at(0)
        );
    }

    #[test]
    fn a_permanent_failure_still_recovers_without_a_restart() {
        // Rare is not never: a corrected credential has to be noticed on its own within
        // a sensible time, even from a user who never presses refresh.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::ServerAvailability, 30));
        queue.claim_ready(at(0), &generous_limits());
        queue.complete(
            &JobKey::new("a"),
            JobOutcome::Permanent("nope".into()),
            at(0),
            0.0,
        );

        let entry = queue.get(&JobKey::new("a")).expect("job present");
        assert!(entry.due_at - at(0) <= Duration::hours(1));
    }

    #[test]
    fn a_manual_refresh_overrides_the_permanent_backoff() {
        // Which is what makes the long wait acceptable: someone who has just fixed the
        // key does not have to sit through it.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("a", Priority::ServerAvailability, 30));
        queue.claim_ready(at(0), &generous_limits());
        queue.complete(
            &JobKey::new("a"),
            JobOutcome::Permanent("nope".into()),
            at(0),
            0.0,
        );

        assert!(queue.trigger_now(&JobKey::new("a"), at(5)));
        let entry = queue.get(&JobKey::new("a")).expect("job present");
        assert_eq!(entry.due_at, at(5));
    }

    #[test]
    fn a_long_interval_is_not_shortened_by_the_permanent_floor() {
        // A server polled hourly must not start being retried every fifteen minutes just
        // because its credential is wrong.
        let mut queue = ScheduleQueue::new();
        queue.upsert(job("slow", Priority::ServerAvailability, 3_600));
        queue.claim_ready(at(0), &generous_limits());
        queue.complete(
            &JobKey::new("slow"),
            JobOutcome::Permanent("nope".into()),
            at(0),
            0.0,
        );

        let entry = queue.get(&JobKey::new("slow")).expect("job present");
        assert_eq!(entry.due_at - at(0), Duration::hours(1));
    }
}
