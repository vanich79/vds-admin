# ADR-001 — Technology stack: pure Rust workspace with a Slint GUI

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

The product must ship one codebase that builds for nine desktop targets and four Android
ABIs:

```
Windows: x64, x86, ARM64
Linux:   x64, x86, ARM64, ARMv7
macOS:   x64, ARM64
Android: arm64-v8a, armeabi-v7a, x86_64, x86
```

It must also provide a modern admin-panel GUI, a high-performance asynchronous monitoring
core capable of handling hundreds of servers, low resource usage, and simple
cross-compilation. iOS is desirable but explicitly conditional on the framework.

## Decision

Build the entire application as a single Cargo workspace in Rust, with
[Slint](https://slint.dev) as the GUI framework for both desktop and Android.

## Alternatives considered

### Flutter (UI) + Rust (core) via `flutter_rust_bridge`

The strongest option on UI quality and mobile maturity, and the one the brief suggested
first. Rejected because Flutter's desktop embedders cover **only** Windows x64/ARM64 and
Linux x64/ARM64. Three explicitly required targets — Windows x86, Linux x86, Linux ARMv7 —
have no Flutter desktop support at all. Meeting the matrix would require a second,
architecturally different application for those targets, which the brief forbids
("не создавай архитектуру, которая потребует поддерживать совершенно разные приложения
для каждой платформы"). Secondary costs: two toolchains, a code-generation step in the
build, and a UI layer that `cargo test` cannot reach.

### Rust + Tauri 2

Covers desktop and mobile including iOS, and web frontends make excellent charts.
Rejected because Tauri depends on a system WebView — WebKitGTK on Linux. That makes
Linux ARMv7/x86 cross-compilation require a full target sysroot with WebKit, which is the
single hardest thing in this matrix, and it contradicts the "минимальное потребление
ресурсов" requirement: a WebView process costs an order of magnitude more RAM than a
native renderer. Startup time also suffers.

### Go + Fyne

Excellent cross-compilation story and full matrix coverage. Rejected on GUI quality — Fyne
cannot plausibly deliver the "modern professional admin panel" the brief describes — and
because it would discard the explicit preference for a Rust monitoring core, along with
Rust's stronger guarantees around the error-handling rules in §29 of the brief.

### egui (Rust)

Full matrix coverage and trivial builds, but immediate-mode rendering redraws
continuously by default (bad for battery on Android) and its visual language is
developer-tooling, not admin panel.

## Consequences

**Positive**

* One language, one build tool. Cross-compiling is `cargo build --target <triple>` plus a
  linker — `cross` supplies those for the Linux targets, the Android NDK for Android.
* The entire application, view-models included, is reachable by `cargo test`.
* Native binary, no WebView, no runtime: single-digit-MB idle RAM, millisecond startup.
* Library choices (`russh`, `rustls`, bundled `rusqlite`) avoid OpenSSL and system C
  dependencies entirely, which is what actually makes ARM/Android builds tractable.

**Negative**

* **iOS is not delivered in v1.** Slint's iOS support is experimental. This is the real
  cost of the decision. Mitigation: everything below the presentation layer is
  UI-agnostic, so an iOS build means re-implementing `apps/ui` only.
* Charts must be implemented in-repo (Slint has no chart widget). Mitigated by computing
  geometry in Rust — where it is unit-tested — and keeping `.slint` files to pure drawing.
* Slint's ecosystem of ready-made widgets is smaller than Flutter's; the design system in
  `apps/ui/ui/theme.slint` is written by hand.
* Fewer developers know Slint than know Flutter, raising the onboarding cost. Mitigated by
  keeping UI logic thin — the `.slint` files are declarative views over Rust view-models.
