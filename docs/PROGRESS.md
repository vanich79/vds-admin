# Progress

Updated as each phase completes. `[x]` done and tested, `[~]` in progress, `[ ]` planned.

## Test count

Every crate's tests run with `cargo test --workspace`. Current totals:

| Crate | Tests |
|---|---|
| `vds-domain` | 129 |
| `vds-agent-protocol` | 8 |
| `vds-infra-collectors` | 137 |
| `vds-application` | 313 |
| `vds-infra-db` | 113 |
| `vds-infra-web` | 30 |
| `vds-infra-secrets` | 26 |
| `vds-infra-analytics` | 52 |
| `vds-infra-ssh` | 39 |
| `vds-infra-screenshot` | 32 |
| `vds-infra-notify` | 18 |
| `vds-composition` | 28 |
| `apps/ui` | 96 |
| `agent` | 65 |
| **Total** | **1086** |

Measured with `cargo test --workspace --all-features`, which is what CI runs. Four further
tests are `#[ignore]`d because they touch a real browser, the real OS keystore or the real
desktop notification daemon; see [DEVELOPMENT.md](DEVELOPMENT.md) §4.

---

## Phase 1 — Architecture

- [x] Requirements analysis
- [x] Technology stack selection and justification ([ADR-001](adr/001-technology-stack.md))
- [x] Layered architecture, module boundaries, dependency rule
- [x] Architecture, module and data-flow diagrams (Mermaid, in [ARCHITECTURE.md](ARCHITECTURE.md))
- [x] Provider architecture (analytics, screenshots, notifications, credentials)
- [x] Database strategy and retention tiers ([ADR-005](adr/005-metrics-storage.md))
- [x] Screenshot strategy ([ADR-004](adr/004-screenshot-architecture.md))
- [x] Yandex.Metrica strategy ([ADR-003](adr/003-analytics-provider-architecture.md))
- [x] Cross-platform build strategy ([ADR-006](adr/006-cross-platform-build.md))
- [x] Architectural risk register (ARCHITECTURE.md §15)
- [x] Extension recipes: how to add a provider, collector, storage backend (§16)

## Phase 2 — Foundation

- [x] Cargo workspace, shared dependency versions, lint and format configuration
- [x] Domain model: servers, websites, analytics, metrics, alerts, events, screenshots
- [x] Status model with severity ordering and thresholds
- [x] Ports: every trait the outside world implements
- [x] Configuration: typed settings, validation, versioning, migration machinery
- [x] SQLite storage with numbered migrations and a `user_version` marker
- [x] Secure credential storage: OS keystore + encrypted-file fallback
- [x] Event system (publisher port, recording/null implementations)
- [x] Scheduler: priorities, concurrency limits, backoff, deduplication, cancellation
- [x] In-memory fakes for every port (`vds-application` `testing` feature)

## Phase 3 — Monitoring core

- [x] Collector layer with a `CommandRunner` seam (SSH / local / scripted)
- [x] Collectors: system, CPU, memory, disk, network, load, processes, Docker, systemd, temperature
- [x] Collector registry with command batching and per-collector outcomes
- [x] Server monitoring use case (probe → evaluate → persist → publish)
- [x] Offline detection with a configurable consecutive-failure threshold
- [x] Network rate derivation (reboot, counter-reset and new-interface safe)
- [x] Website monitoring: DNS, TCP, TLS certificate inspection, HTTP, expectations
- [x] Metric storage, rollup cascade and retention with an aggregation-lag guard
- [x] Alert engine (hold timers, incidents, renotification, acknowledgement)
- [x] Provisioning use case: credential stored before the entity, rolled back if the save fails
- [x] Notification dispatcher with pluggable providers
- [x] SSH transport (`vds-infra-ssh`): pooled sessions, batched commands, host-key pinning
- [ ] Container and service control (start/stop/restart/logs) — deliberately deferred, see below

## Phase 4 — Analytics

- [x] `AnalyticsProvider` port with capability negotiation
- [x] Provider registry
- [x] Provider-neutral storage (no provider-specific tables)
- [x] Analytics service: cache-first reads, scheduled refresh, rate limiting
- [x] Traffic anomaly detection (median baseline, configurable thresholds)
- [x] Yandex.Metrica provider (OAuth, metric mapping, derived returning visitors)
- [x] Fleet-wide traffic series (additive metrics summed, rates averaged)
- [x] Top pages and per-website series, both capability-gated
- [x] Demo provider, behind the `demo-providers` feature and never auto-registered

## Phase 5 — Screenshot system

- [x] `ScreenshotProvider` port
- [x] `ScreenshotService`: cache policy, refresh scheduling, honest staleness reporting
- [x] Chromium CLI provider and thumbnail generation
- [x] Filesystem screenshot store with content hashing
- [x] Demo screenshot provider, behind the same feature

## Phase 6 — GUI

- [x] Dashboard query service and widget architecture
- [x] Slint UI: Dashboard, Servers, Server Details, Websites, Website Details, Analytics, Alerts, Settings
- [x] Responsive layout (sidebar on desktop, bottom navigation on mobile)
- [x] Light/dark/system theming
- [x] Charts drawn from Rust-computed geometry, for both metrics and traffic
- [x] Send-safe payload boundary between the worker and the UI thread, with a compile-time guard
- [x] Add-server dialog: SSH and agent modes, three authentication kinds, inline errors
- [x] Add-website dialog, with scheme inference and expectation fields
- [x] Server Settings tab: forget host key, remove server — both two-step
- [x] Localisation: English and Russian from one generated catalogue, switchable live
- [ ] More languages — a column in the table; nothing in the code is in the way
- [ ] Add-rule dialog — the alert screen lists and toggles rules but cannot create one

## Phase 7 — Agent

- [x] `vds-agent` binary: HTTPS, bearer token, three read-only endpoints
- [x] Versioned wire protocol (`vds-agent-protocol`)
- [x] Reuses the collector layer rather than duplicating the parsers
- [x] Self-signed certificate generated on first start; fingerprint pinned by the app
- [x] Constant-time token comparison; token never logged
- [x] Short report cache, so several watchers do not multiply the load on the host
- [x] Hardened systemd unit with `ExecStartPre` validation and resource ceilings
- [x] Installer with checksum verification, optional GPG signature verification, and
      `--uninstall` / `--purge`

## Phase 8 — Cross-platform build

- [x] Build scripts per platform (`scripts/build-*.sh`), called by CI rather than duplicated
- [x] GitHub Actions: lint → tests → agent matrix → desktop builds → Android
- [x] Release pipeline producing every artefact from the tagged commit
- [x] Installers: NSIS, `.deb`, AppImage, `.dmg`, APK, agent tarballs
- [x] `SHA256SUMS` per release, with an optional detached GPG signature
- [x] `scripts/check-layering.sh` enforcing the dependency rule in CI

## Documentation

- [x] `docs/ARCHITECTURE.md`
- [x] `docs/adr/` (six records)
- [x] `docs/PROGRESS.md`
- [x] `README.md`
- [x] `docs/DEVELOPMENT.md`
- [x] `docs/BUILDING.md`
- [x] `docs/CROSS_COMPILATION.md`
- [x] `docs/AGENT.md`
- [x] `docs/SSH.md`
- [x] `docs/SECURITY.md`
- [x] `docs/РУКОВОДСТВО.md` — руководство пользователя на русском

---

## Notes on verification

The development machine had no compiler at all when this work started, so a MinGW-w64
toolchain and Rust were installed first. Everything marked `[x]` above compiles and its
tests pass — the counts in the table are real `cargo test` output, not estimates.

Three design defects were found by tests during construction and fixed in the code rather
than in the tests:

* a 7-day chart served from five-minute rollups returned 2016 points, not the ~500 the
  architecture claimed — the tier mapping was wrong (`docs/adr/005`);
* the rate limiter accrued tokens *during* a provider back-off penalty, so it would burst
  straight back into being rate limited the moment the penalty expired;
* the alert engine cleared an incident's identifier before the service could use it to
  close that incident, which would have left every recovered incident open forever.

One documented claim was also wrong and was corrected: a moving *average* does not absorb
a single outlying day, so the anomaly detector's default baseline is now a moving median,
which does.

Later phases found five more, all fixed in the code:

* the rate limiter treated any 4xx from Metrica as retryable, so a malformed request would
  be retried forever; non-429 client errors are now permanent failures;
* log redaction leaked `Authorization: Basic <credential>`, and the first fix
  over-redacted across newlines — it now works per line;
* the `df` parser could not distinguish unreadable output from a host with only
  pseudo-filesystems, which would have shown "no disks" instead of an error;
* `apps/ui` declared a dependency on `vds-infra-screenshot` that nothing used — caught by
  the new layering check, and exactly the kind of thing that becomes a real violation
  later;
* `cargo lint` had never passed with `-D warnings`, because Slint's generated code trips
  `unwrap_used`. The generated module now carries a local `allow`, so the denials stay in
  force for every hand-written line;
* URL normalisation turned `ftp://files.example` into `https://ftp://files.example`, which
  parses — the host becomes `ftp` — and would have been accepted as a website that could
  never resolve. A string that already carries a scheme is now left alone, so validation
  rejects it by name.

The provisioning work also had to be structured around a failure that is easy to overlook:
a credential is written to the OS keystore *before* the row that references it. If the save
then fails, the secret is orphaned — unreachable, undeletable, and duplicated every time
the user retries. `create_server` rolls it back, and a test with a scripted save failure
proves it does.

## Deliberately not in this version

**Container and service control** — start, stop, restart, view logs. The domain model,
the UI tabs and the agent are all structured for it. It is not shipped because a remote
execution endpoint changes what a stolen credential is worth, and it deserves confirmation
flows and an audit trail before it exists rather than after.

**Creating alert rules from the interface.** Servers and websites can be added and
removed; the seven default rules are seeded on a fresh installation and can be enabled and
disabled, but a new rule cannot yet be written from the Alerts screen. The domain model,
the repository and the engine all support it — only the dialog is missing.

**Editing what exists.** A server's poll interval, thresholds and credentials can be
changed in the database but not yet in the interface. Removing and re-adding works and
loses that server's metric history, which is a poor answer.

**iOS.** Slint supports it and nothing in the architecture is in the way. It needs Apple
developer enrolment and a review process, not code.
