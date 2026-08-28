# VDS Admin

One window for a fleet of servers: whether each machine is up, what it is doing, whether
the websites on it are answering, when their certificates expire, and how much traffic
they are getting.

Written in Rust, with a single codebase for Windows, Linux, macOS and Android.

---

## What it does

**Servers.** CPU, memory, disk, network, uptime, load average, temperature, processes,
Docker containers and systemd units — over SSH with no software installed on the target,
or through an optional lightweight agent.

**Websites.** DNS resolution, TCP connection, HTTP status, response time, TLS certificate
issuer and expiry, an availability history, and a screenshot of the page.

**Traffic.** Visitors, visits, page views, bounce rate and session duration, through a
provider interface. Yandex.Metrica is implemented; the interface is what makes the next
one cheap.

**Alerts.** Rules such as *CPU above 90% for five minutes*, *disk above 90%*, *server
offline*, *certificate expires within 14 days*, *container stopped* — delivered as desktop
and Android notifications, with e-mail, Telegram and webhooks behind the same interface.

**Languages.** English and Russian, switchable in Settings without a restart. Every string
comes from one generated catalogue, so adding a language is a column in a table rather
than an audit of the markup.

Everything runs locally. There is no account, no registration and no central server; the
architecture leaves room for an optional one later.

---

## Two ways to monitor a server

|                | **SSH** (agentless) | **Agent** |
|---|---|---|
| Install on the server | nothing | one static binary |
| Credentials | SSH key or password, in the OS keystore | bearer token |
| Cost per poll | one connection and a batch of commands | one HTTPS request |
| Best for | up to a few hundred servers | large fleets, or hosts you would rather not give SSH access to |

Start with SSH. Move a server to the agent when the connection cost starts to matter —
the choice is per server, and the data you get is the same either way.

```sh
# On the server you want to watch:
curl -fsSL https://github.com/vds-admin/vds-admin/releases/latest/download/install.sh | sudo sh
```

The installer verifies its download against published checksums, creates an unprivileged
system user, and prints the token and the certificate fingerprint to enter in the app.
[docs/AGENT.md](docs/AGENT.md) describes the signature-verified path to use on a
production fleet.

---

## Installing the application

Download from [Releases](https://github.com/vds-admin/vds-admin/releases):

| Platform | File |
|---|---|
| Windows | `vds-admin-<version>-setup.exe`, or the portable `.zip` |
| Linux | `.AppImage`, `.deb`, or `.tar.gz` |
| macOS | `.dmg` |
| Android | `vds-admin-arm64-v8a-release.apk` |

Check what you downloaded before running it:

```sh
sha256sum --check SHA256SUMS --ignore-missing
```

Or build it yourself — see [docs/BUILDING.md](docs/BUILDING.md).

---

## Design in one page

Four layers, and dependencies that only ever point inward:

```
  Presentation  apps/ui — Slint views, view models, chart geometry
       ↓
  Application   crates/application — use cases, scheduling, alerting, aggregation
       ↓
  Domain        crates/domain — the model, and every port the outside world implements
       ↑
  Infrastructure crates/infra-* — SSH, SQLite, HTTP, TLS, keystore, browser, providers
```

The domain knows nothing about Slint, SQLite, SSH, HTTP or any analytics vendor. The UI
performs no I/O: a click becomes an *intent* on a queue, work happens on the Tokio
runtime, and the result reaches the window through `invoke_from_event_loop`. Every
external integration — analytics, screenshots, notifications, credential storage — sits
behind a port with declared capabilities, and the interface hides what a provider says it
cannot do rather than showing an empty panel.

That rule is not a convention; `scripts/check-layering.sh` enforces it in CI.

A few decisions worth knowing before reading the code:

- **`MetricValue` is `Available(f64)` or `NotAvailable`.** A metric that was not measured
  is never zero, at any layer, on any wire format. A dash on screen means "not measured";
  a `0` means measured and zero.
- **Absent is not empty.** `containers: null` means the host has no Docker;
  `containers: []` means Docker is there and running nothing. The interface shows those
  differently.
- **No fabricated data in production.** The demo providers exist, are useful, and are
  behind a Cargo feature that release builds never enable.

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) has the diagrams, the data flow, the risk
register, and recipes for adding a provider, a collector or a storage backend. The
[ADRs](docs/adr/) record why each significant decision went the way it did.

---

## Building

```sh
cargo test --workspace          # the suite
cargo lint                      # clippy, warnings denied
cargo run --package vds-admin   # the application
```

The GUI needs `libfontconfig1-dev` and `libxkbcommon-dev` on Linux; nothing beyond a Rust
toolchain elsewhere. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the layout and how
to work on it; [docs/CROSS_COMPILATION.md](docs/CROSS_COMPILATION.md) covers the other
targets.

---

## Documentation

| | |
|---|---|
| [РУКОВОДСТВО.md](docs/РУКОВОДСТВО.md) | **Руководство пользователя** (на русском): запуск, добавление серверов и сайтов |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Layers, modules, data flow, risks, extension recipes |
| [DEVELOPMENT.md](docs/DEVELOPMENT.md) | Repository layout, conventions, testing, debugging |
| [BUILDING.md](docs/BUILDING.md) | Building and packaging for every platform |
| [CROSS_COMPILATION.md](docs/CROSS_COMPILATION.md) | Toolchains, targets, and what needs which host |
| [AGENT.md](docs/AGENT.md) | Installing, configuring and operating `vds-agent` |
| [SSH.md](docs/SSH.md) | How agentless mode connects, and how to troubleshoot it |
| [SECURITY.md](docs/SECURITY.md) | Threat model, credential handling, reporting a vulnerability |
| [adr/](docs/adr/) | Architecture decision records |
| [PROGRESS.md](docs/PROGRESS.md) | What is built, what is not |

---

## Status

The monitoring core, storage, alerting, analytics, screenshots, the agent and the
interface are implemented and tested. Container and service *control* — start, stop,
restart, logs — is deliberately not in this version: the architecture is built for it,
and shipping a remote-execution endpoint changes what a stolen credential is worth, so it
waits for the confirmation flows and audit trail it deserves.

See [docs/PROGRESS.md](docs/PROGRESS.md) for the detail.

## Licence

MIT.
