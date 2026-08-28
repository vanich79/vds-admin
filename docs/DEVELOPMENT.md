# Development

How the repository is laid out, how to work in it, and the conventions that are worth
knowing before changing anything.

---

## 1. Getting started

```sh
git clone https://github.com/vds-admin/vds-admin
cd vds-admin

cargo test --workspace      # ~1000 tests, under a minute
cargo run -p vds-admin      # the application
```

You need a stable Rust toolchain (2024 edition, 1.85 or newer). On Linux the GUI also
needs:

```sh
sudo apt install libfontconfig1-dev libxkbcommon-dev    # Debian/Ubuntu
sudo dnf install fontconfig-devel libxkbcommon-devel    # Fedora
```

Nothing else. No Node, no Python, no system Qt, no cmake, no OpenSSL headers — the
dependency choices in [ADR-001](adr/001-technology-stack.md) were made partly to keep
that true, because "install these seven things first" is how a project loses
contributors.

Before opening a pull request:

```sh
cargo fmt --all
cargo lint                      # clippy with -D warnings
cargo test --workspace --all-features
bash scripts/check-layering.sh
```

CI runs exactly these.

---

## 2. Layout

```
crates/
  domain/            The model and every port. Depends on nothing here.
  agent-protocol/    The app↔agent wire contract. Depends on nothing here.
  application/       Use cases. Depends on domain only.
  infra-ssh/         SSH transport
  infra-db/          SQLite
  infra-web/         HTTP, DNS, TLS inspection
  infra-collectors/  The Linux metric parsers
  infra-analytics/   Yandex.Metrica, and the demo provider
  infra-screenshot/  Headless-browser capture and file storage
  infra-secrets/     OS keystore and the encrypted-file fallback
  infra-notify/      Desktop notifications and webhooks
  composition/       The composition root: paths, logging, wiring
apps/ui/             The Slint application
agent/               The vds-agent daemon
docs/                Documentation and ADRs
packaging/           systemd unit, installer, desktop entry, NSIS script
scripts/             Build and check scripts
```

### The dependency rule

Dependencies point inward, always:

```
  apps/ui ──► composition ──► infra-* ──► domain
      │            │              ▲
      └────────────┴──► application ┘
```

- `domain` depends on nothing in the workspace, and imports no framework. No Slint, no
  SQLite, no HTTP client, no SSH library, no analytics SDK.
- `application` depends on `domain` only. It works with the *ports* — traits — and never
  names an implementation.
- `infra-*` implement ports. They may use `infra-collectors`, which is a pure parsing
  library with no I/O of its own, driven by whichever transport needs it.
- `composition` is the only place that knows every concrete type. That is its whole job.
- `apps/ui` may use `composition`, `application` and `domain`. **Never an `infra-` crate.**

This is enforced, not merely agreed:

```sh
bash scripts/check-layering.sh
```

It reads the manifests, checks the imports, and fails CI. The rule is what makes swapping
SQLite for PostgreSQL, or adding a second analytics provider, a change in one place. It
is violated by adding one line to a `Cargo.toml`, which is why a script checks it every
time rather than a reviewer checking it on a good day.

---

## 3. Conventions

### Errors

No `unwrap()`, no `expect()`, no `panic!()` in production code. Clippy denies all three,
and allows them in `#[cfg(test)]` — a test that cannot proceed *should* fail loudly.

Errors are typed with `thiserror` and carry what the reader needs:

```rust
#[error("could not read {path}: {source}")]
Read { path: PathBuf, #[source] source: std::io::Error },
```

Not `"read failed"`. Someone is going to see this at three in the morning.

### Absence

`MetricValue` is `Available(f64)` or `NotAvailable`. A metric that was not measured is
never zero, at any layer, in any format. This is the single most load-bearing convention
in the codebase:

```rust
// Wrong. A server that did not answer now looks idle.
let cpu = snapshot.cpu.total_percent.unwrap_or(0.0);

// Right.
match snapshot.cpu.total_percent {
    MetricValue::Available(percent) => /* ... */,
    MetricValue::NotAvailable => /* show a dash */,
}
```

The same holds for collections: `Option<Vec<T>>` where `None` means "the feature is not
present on this host" and `Some(vec![])` means "it is present and empty". The interface
renders those differently, and so does the agent's wire format.

### Async

Everything I/O is async on Tokio. Two rules:

- **The UI thread never blocks.** A callback pushes an *intent* onto a queue and returns.
  Work happens on the runtime. Results reach the window through
  `slint::invoke_from_event_loop`.
- **Slint types never cross a thread.** `ModelRc` is an `Rc` and `Image` holds a
  non-atomic handle, so neither is `Send`. Worker threads produce *payloads* — plain data,
  fully formatted, carrying a filename where an image goes — and `apps/ui/src/payload.rs`
  converts them on the UI thread. A compile-time test asserts every payload is `Send`, so
  a `ModelRc` that leaks into one stops the build.

### Strings

No user-facing text is written inline. Every string lives in one table, in
`scripts/generate-strings.py`, which writes both sides of the boundary:

| Generated | Read by |
|---|---|
| `apps/ui/ui/strings.slint` | markup, as `L.key` |
| `apps/ui/src/i18n.rs` | Rust, as `i18n::strings().key` |

Adding a string is one row in the table, then:

```sh
python scripts/generate-strings.py
```

Both files come from the same source, so a key cannot exist on one side and not the
other — and if the generated pair ever drifts, the missing setter stops the build rather
than producing a blank label.

Two rules the table enforces with tests:

- **Every language has every string.** An empty translation renders as a blank label
  rather than as an obvious mistake, so it fails the build instead.
- **Placeholders survive.** A translation that drops its `{}` silently loses the number it
  was meant to carry. That is the one localisation bug nobody notices.

Avoid sentences assembled from fragments. `"{n} need attention"` inflects the verb with
the count in English and needs three plural forms of the noun in Russian; `"Label: {n}"`
translates without a pluralisation engine. Where a phrase genuinely needs the count, give
the whole phrase a key.

### Comments

Explain *why*, not *what*. The code says what it does.

```rust
// Averages are taken over servers that actually reported a number. An unreachable
// server has no CPU usage, and counting it as zero would drag the fleet average
// down and hide a real problem.
```

That comment earns its place. `// increment the counter` does not.

---

## 4. Testing

Around 1000 tests, all of which run in under a minute with no network, no database server
and no Docker. That is not an accident — it is what the port architecture buys, and it is
worth protecting.

**Unit tests** live beside the code in `#[cfg(test)] mod tests`.

**Parsers are tested against captured real output.** `crates/infra-collectors` has
fixtures from actual machines: Debian, Alpine, a Raspberry Pi, a container with no
`/sys/class/thermal`. When a parser breaks on a distribution, add its output as a fixture
rather than patching the parser blind.

**Fakes, not mocks.** `vds-application` has a `testing` feature with in-memory
implementations of every port. A use case is tested by wiring it to fakes:

```rust
let servers = Arc::new(FakeServerRepository::new());
let probe = Arc::new(ScriptedProbe::new());
probe.respond(Ok(healthy_snapshot(id, at(1_000))));

let monitor = ServerMonitor::new(probe, servers, metrics, events, clock);
assert_eq!(monitor.collect(id).await, JobOutcome::Success);
```

**HTTP is tested against `wiremock`**, a real local server, so the request that goes out
is a real request.

**Time is injected.** `Clock` is a port; tests use `FixedClock`. Nothing sleeps to wait
for time to pass.

### Naming

Test names are sentences that state the guarantee:

```rust
fn an_unmeasured_average_shows_a_dash_not_a_zero()
fn a_token_that_is_merely_a_prefix_is_rejected()
fn absent_docker_and_empty_docker_stay_distinguishable()
```

When one fails, the report tells you what broke without opening the file. `test_cpu_2()`
does not.

### The four ignored tests

Everything else runs anywhere. These four touch something real, so they are `#[ignore]`d
and run on demand:

| Test | Needs | Run it with |
|---|---|---|
| `a_real_browser_captures_a_real_page` | a local Chromium or Chrome | `cargo test -p vds-infra-screenshot -- --ignored` |
| `the_platform_keystore_round_trips` | the OS keystore | `cargo test -p vds-infra-secrets -- --ignored` |
| `probing_reports_whether_the_keystore_is_usable` | the OS keystore | as above |
| `a_real_notification_is_shown` | a desktop notification daemon | `cargo test -p vds-infra-notify -- --ignored` |

They are worth running by hand after changing the code they cover. The logic around each
of them is tested normally — what these four verify is that the real integration still
works, which no fake can tell you.

### What to test

Behaviour that would be wrong if it changed — not implementation detail. The most
valuable tests here have been the ones asserting a *negative*: that an unavailable metric
is not zero, that a prefix of a token is not accepted, that a stale screenshot is not
shown without its age. Several of those caught real defects during development.

---

## 5. Running things

```sh
cargo run -p vds-admin                                  # the application
RUST_LOG=debug cargo run -p vds-admin                   # with logging

cargo run -p vds-agent -- --config agent.toml           # the agent
cargo run -p vds-agent -- --check --config agent.toml   # validate a config

cargo test -p vds-application                           # one crate
cargo test -p vds-application offline                   # matching tests
cargo test -- --nocapture                               # with output
```

The application writes to the platform data directory:

| | |
|---|---|
| Linux | `~/.local/share/vds-admin/` |
| Windows | `%APPDATA%\vds-admin\` |
| macOS | `~/Library/Application Support/vds-admin/` |

with `vds-admin.db`, `config.toml`, `logs/` and `screenshots/` inside. Delete the
directory to start clean; secrets in the OS keystore are separate and survive.

### Trying it without servers

The demo providers generate plausible analytics and placeholder screenshots, so the
interface can be exercised with nothing configured:

```sh
cargo run -p vds-admin --features demo-providers
```

They are behind a Cargo feature that release builds never enable, they name themselves
"Demo (fabricated data)", and nothing registers them automatically. Fabricated numbers
reaching a production dashboard would be worse than no numbers at all — a user looks at a
dashboard to decide whether something is wrong.

---

## 6. Common tasks

### Adding a collector

1. Write the parser in `crates/infra-collectors/src/`. It declares the commands it needs
   and parses their output. **No I/O.**
2. Implement `Collector`: `id()`, `requires()`, `commands()`, `parse()`.
3. Register it in `CollectorRegistry::linux()`.
4. Add fixtures from real machines, including one where the feature is absent.

Both SSH mode and the agent pick it up. Neither needs to know it exists.

### Adding an analytics provider

1. Implement `AnalyticsProvider` in `crates/infra-analytics/src/`.
2. Declare `capabilities()` **honestly** — the interface hides what a provider says it
   cannot do, and a provider that claims a metric it cannot serve produces an empty panel
   instead of a hidden one.
3. Register it in `crates/composition/src/wiring.rs`.

Nothing in the domain, the application layer, the database schema or the UI changes.
[ADR-003](adr/003-analytics-provider-architecture.md) explains why.

### Adding a database migration

1. Add the SQL to `crates/infra-db/src/migrations.rs` as a new numbered step.
2. Bump `SCHEMA_VERSION`.
3. Test the upgrade from every earlier version, not just the previous one.

Migrations are versioned and explicit. The schema is never altered by inference at
startup — see [ADR-005](adr/005-metrics-storage.md).

### Changing the interface

`.slint` files in `apps/ui/ui/` are layout only. Anything with logic in it belongs in
`view_model.rs` (formatting), `chart.rs` (geometry) or `runtime.rs` (queries), where it
can be tested without a window.

---

## 7. Debugging

**Logs.** `RUST_LOG=debug`, or **Settings → Debug mode** in the application. Written to
`logs/` in the data directory, rotated daily. Secrets are redacted at three layers, so a
log is safe to attach to an issue.

**The database.** Plain SQLite:

```sh
sqlite3 ~/.local/share/vds-admin/vds-admin.db
.tables
select name, status, last_check from server_state join server using (id);
```

**The scheduler.** **Settings → Debug mode** shows every registered job, when it last ran,
when it runs next, and its failure count. This is usually the fastest way to answer "why
is this not updating?".

**The agent, by hand:**

```sh
curl -k https://localhost:9443/v1/health
curl -k -H "Authorization: Bearer $(sudo cat /etc/vds-agent/token)" \
     https://localhost:9443/v1/metrics | jq .
```

---

## 8. Pull requests

- One change per pull request.
- Tests for anything that could break.
- `cargo fmt --all`, `cargo lint`, `cargo test --workspace --all-features`, and
  `scripts/check-layering.sh`, all clean.
- Update the documentation in the same commit as the change it describes.
- If a decision is significant — a new dependency, a new layer, a change to the storage
  model — add an ADR. `docs/adr/` is the record of *why*, and a decision without one is a
  decision the next person has to re-derive.

Commit messages: what changed and why, in the imperative. The diff already shows how.
