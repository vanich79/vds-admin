# ADR-002 — Monitoring architecture: one collector layer behind a `CommandRunner` seam

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

Metrics must be obtainable two ways: agentless over SSH (Mode A) and from a lightweight
agent installed on the server (Mode B). Both must yield the same domain model. Collectors
must be unit-testable, and the test suite must be able to emulate online / offline / slow
/ Docker-less / broken servers without a network.

A naive design gives each mode its own collection code, which doubles every parser and
guarantees the two modes drift apart.

## Decision

Introduce one narrow trait as the only seam between *acquisition* and *interpretation*:

```rust
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, command: &str) -> Result<CommandOutput, TransportError>;
    fn capabilities(&self) -> TransportCapabilities;
}
```

Collectors depend on `CommandRunner` and nothing else. Each collector is
*text in → typed struct out*:

```rust
#[async_trait]
pub trait Collector: Send + Sync {
    fn id(&self) -> CollectorId;
    fn requires(&self) -> &'static [Capability];
    async fn collect(&self, runner: &dyn CommandRunner) -> Result<CollectorOutput, CollectError>;
}
```

Three implementations of `CommandRunner` exist:

| Implementation | Crate | Used by |
|---|---|---|
| `SshCommandRunner` (russh, pooled session) | `infra-ssh` | GUI, Mode A |
| `LocalCommandRunner` (`/bin/sh -c`) | `infra-collectors` | the agent |
| `ScriptedCommandRunner` (canned responses) | `infra-collectors` (test support) | tests |

The agent additionally implements `ProcFsSource`, reading `/proc` and `/sys` directly for
CPU, memory, load, uptime and network, so its hot path spawns no processes at all.
Collectors that have a `ProcFsSource` fast path use it; the rest fall back to commands.

## Alternatives considered

* **Separate collection stacks per mode.** Rejected: duplicated parsers, guaranteed
  behavioural drift, and the SSH path would be untestable without a live server.
* **Make collectors depend on an SSH session type directly.** Rejected: couples
  interpretation to transport, makes the agent impossible to share code with, and makes
  unit tests require a network.
* **Ship a fixed remote shell script that returns JSON.** Tempting, and it reduces
  round-trips, but it puts parsing logic on the monitored host where it cannot be
  versioned with the app, and it breaks on hosts without the assumed interpreter. We do
  batch commands into a single payload per cycle for round-trip efficiency, but parsing
  stays local.

## Consequences

**Positive**

* Every parser is a pure function tested against captured real-world fixtures
  (`crates/infra-collectors/tests/fixtures/`), covering distro variations.
* `ScriptedCommandRunner` gives the whole test matrix the brief asks for — online, offline,
  slow, no-Docker, failing-systemctl — with no infrastructure.
* Adding a collector is one new file plus a registry line; it works over both transports
  immediately.
* The agent and the GUI cannot disagree about what "CPU usage" means.

**Negative**

* `CommandRunner` returning raw text means collectors must tolerate output variation
  across distributions; this is handled with defensive parsers that degrade to
  `MetricValue::NotAvailable` rather than failing the whole cycle.
* The seam prevents collectors from streaming output, so a collector that needed
  incremental results (e.g. `tail -f` style log following) would need a second, additive
  trait rather than reusing this one.
