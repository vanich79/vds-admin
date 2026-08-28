# ADR-003 — Analytics: provider-independent domain model with capability negotiation

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

Yandex.Metrica must be integrated now; Google Analytics, Plausible, Matomo, Cloudflare
Analytics and custom APIs must be addable later without rewriting the dashboard. Providers
disagree about which metrics exist: Metrica has no concept distinct from "visits" for
"sessions"; Plausible has no "returning visitors" as a first-class metric; GA4 has its own
vocabulary entirely.

The failure mode to avoid is a schema and a UI shaped like whichever provider was
integrated first.

## Decision

**1. A provider-independent domain model.** The UI and the application layer know only:

```rust
AnalyticsSnapshot   { website_id, period, metrics: Map<AnalyticsMetric, MetricValue> }
AnalyticsTimeSeries { website_id, metric, interval, points: Vec<AnalyticsPoint> }
AnalyticsCapabilities { supported_metrics, supports_time_series, supports_top_pages,
                        supports_referrers, supports_realtime, min_interval }
```

**2. `MetricValue` models absence explicitly.**

```rust
enum MetricValue { Available(f64), NotAvailable }
```

A provider that cannot serve a metric returns `NotAvailable`. Fabricating a zero is
forbidden — it would be indistinguishable from real zero traffic.

**3. Capability-driven UI.** The UI queries `capabilities()` and hides unsupported
features rather than rendering broken panels. Capabilities are data, so a new provider
changes UI behaviour without changing UI code.

**4. Provider-neutral persistence.** Storage is `analytics_integrations`,
`analytics_snapshots`, `analytics_time_series`, keyed by a `provider_id` string.
Provider-specific configuration lives in a versioned `settings` JSON column, isolated
from the business model. There is no `yandex_*` table.

**5. Credentials via the shared `SecretStore`.** An integration row stores only a
`credential_ref` UUID. OAuth tokens live in the same OS keystore as SSH credentials.

## Alternatives considered

* **Model the domain on Metrica's API and adapt others to it later.** Rejected: this is
  exactly the "provider-shaped core" trap; GA4's model does not map cleanly onto
  Metrica's, so the adaptation cost would land later and larger.
* **Lowest-common-denominator metric set.** Rejected: throws away useful data from richer
  providers, and there is no way to grow it without a breaking change.
* **Per-provider tables and per-provider UI panels.** Rejected: every new provider would
  touch the schema, the repositories, and the dashboard.

## Consequences

**Positive**

* Adding Google Analytics is one new file implementing `AnalyticsProvider`, one
  registration in the composition root, and a capability mapping. Zero changes to domain,
  schema, application or UI code.
* Snapshots from different providers are directly comparable and can be aggregated on one
  dashboard.
* Absent data is visibly absent, which is the honest behaviour.

**Negative**

* Provider-specific advanced features (Metrica's segment expressions, GA4's custom
  dimensions) are not expressible in the shared model. They are reachable only by
  extending `AnalyticsCapabilities` and the metric enum — deliberately a conscious,
  reviewed act rather than an ad-hoc field addition.
* The mapping layer adds indirection, and metric semantics across providers are similar
  but not identical (e.g. what counts as a "bounce"). The mapping table in
  `docs/ARCHITECTURE.md` §10 documents each equivalence so the approximation is explicit.
