# ADR-005 — Metrics storage: SQLite with explicit multi-tier rollups

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

The app is standalone by default — no cloud account, no server to install. It must retain
metric history for up to a year, render charts over ranges from 1 hour to 90 days, and
stay responsive with hundreds of servers producing samples every 15–30 seconds. At 200
servers × 6 metrics × 4 samples/minute that is ~2.5 million rows per day, so raw retention
alone is not viable, and the UI must never receive millions of points.

The storage engine also has to compile for Android and ARMv7 without a system database.

## Decision

**Engine: SQLite via `rusqlite` with the `bundled` feature**, in WAL mode. SQLite is
compiled from vendored C source as part of the Cargo build, so it cross-compiles wherever
a C cross-compiler exists — including the Android NDK — with no system package.

Access is synchronous, so repositories wrap calls in `tokio::task::spawn_blocking` behind
async traits. Writes funnel through a single writer task with batched inserts per
collection cycle.

**Four explicit tiers**, all with configurable retention:

| Tier | Storage | Default retention | Written by |
|---|---|---|---|
| raw | `metric_samples` | 7 days | collectors |
| 5-minute | `metric_rollups` `bucket='m5'` | 30 days | `MetricsAggregationService` |
| 1-hour | `metric_rollups` `bucket='h1'` | 365 days | `MetricsAggregationService` |
| 1-day | `metric_rollups` `bucket='d1'` | unlimited (configurable) | `MetricsAggregationService` |

Rollups store `min`, `max`, `avg`, `sum`, `count` per bucket, so charts can render a
band rather than a lying average, and higher tiers are computed from lower tiers rather
than from raw data (`d1` from `h1`, `h1` from `m5`, `m5` from raw), which keeps
aggregation cost constant as history grows.

**Tier selection is a query concern**, resolved by requested window so the UI receives
at most `MAX_CHART_POINTS` (750) for any range:

| Range | Tier | Points returned |
|---|---|---|
| 1 h | raw | ≤ 240 (at a 15 s poll) |
| 6 h, 24 h | `m5` | 72 / 288 |
| 7 d, 30 d | `h1` | 168 / 720 |
| 90 d, 1 y | `d1` | 90 / 365 |

A unit test (`every_range_stays_under_the_point_budget`) asserts the bound, so the
mapping cannot silently drift — it is what caught an earlier draft that served a 7-day
chart from five-minute rollups, i.e. 2016 points.

**Migrations** are numbered, embedded in the binary, and applied in a transaction with
`PRAGMA user_version` as the marker. Nothing mutates schema implicitly at startup. A
non-additive migration copies the database to `<db>.pre-v<N>.bak` first.

## Alternatives considered

* **Raw samples only, aggregate at query time.** Simplest, and rejected: unbounded growth
  and query cost that degrades exactly as history becomes valuable.
* **A dedicated embedded time-series store.** Nothing in the Rust ecosystem is both
  mature and embeddable across this platform matrix; adding a second storage engine also
  doubles the backup/migration story for a standalone desktop app.
* **InfluxDB / TimescaleDB from the start.** Rejected for v1: requires the user to run a
  server, which directly contradicts the standalone requirement. Both remain reachable —
  they are just another implementation of `MetricsRepository`.
* **Continuous aggregation via SQLite triggers.** Rejected: hides cost inside the write
  path and is hard to test; an explicit scheduled service is observable and unit-testable.

## Consequences

**Positive**

* One file, no server, works identically on desktop and Android.
* Storage growth is bounded and predictable; the year-long view costs 365 rows per series.
* Because rollups carry min/max, long-range charts still show spikes instead of averaging
  them away.
* `MetricsRepository` is a port, so PostgreSQL/Timescale is an additive change.

**Negative**

* SQLite is a single-writer database; very large fleets will eventually contend on the
  write lock. Mitigated by WAL, batching and a single writer task, and bounded by the
  agent-mode/central-server path in the scaling plan.
* Aggregation is a scheduled job, so the most recent bucket is incomplete until it runs;
  queries near the boundary blend the newest tier with raw data to avoid a visible gap.
* Blocking calls on a thread pool cost a little throughput versus a natively async driver.
  Acceptable: the workload is dominated by network I/O, not by SQLite.
