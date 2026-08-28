# ADR-006 — Cross-platform build: `cross` for Linux, native runners elsewhere, `cargo-apk` for Android

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

Thirteen artifacts must be produced from one codebase:

```
Windows: x64, x86, ARM64          Linux: x64, x86, ARM64, ARMv7
macOS:   x64, ARM64               Android: arm64-v8a, armeabi-v7a, x86_64, x86
```

Rust can *compile* for all of them from any host; the hard part is always the **linker**
and the target C toolchain, which is needed here because `rusqlite` builds SQLite from C
source and `ring`/`aws-lc` (via rustls) has C and assembly components.

## Decision

Split the matrix by what each target actually needs, rather than pretending one command
covers everything.

**1. Linux targets → [`cross`](https://github.com/cross-rs/cross) (Docker).**
`cross build --target <triple>` runs the build inside an image that already contains the
matching GCC cross-toolchain. This covers `x86_64`, `i686`, `aarch64` and
`armv7-unknown-linux-gnueabihf` uniformly, on any host and on ordinary x64 CI runners. No
sysroot assembly by hand.

**2. Windows targets → native `windows-msvc` on Windows runners.**
`x86_64` and `i686` build directly. `aarch64-pc-windows-msvc` cross-compiles from an x64
Windows runner because MSVC ships the ARM64 toolchain — it is not cross-compilable from
Linux, so it stays on a Windows runner.

**3. macOS targets → native macOS runners.**
Apple's SDK licence makes Linux-hosted cross-compilation impractical. Apple-silicon
runners build both `aarch64-apple-darwin` and `x86_64-apple-darwin`, then `lipo` produces
a universal binary for the `.app`/`.dmg`.

**4. Android → `cargo-apk` with the NDK**, building all four ABIs
(`aarch64`, `armv7`, `x86_64`, `i686` linux-android) into one APK. The NDK supplies the C
toolchain that bundled SQLite needs. Release APKs are signed in CI from repository
secrets; unsigned debug APKs are produced for PR builds.

**5. The agent is built the same way but static-first**: `*-unknown-linux-musl` where
available, giving a single dependency-free binary per architecture — important because the
agent is installed on arbitrary servers whose glibc version is unknown.

**Scripts mirror CI exactly.** `scripts/build-*.sh` are the same commands CI runs, so a
developer can reproduce any artifact locally. CI calls the scripts rather than duplicating
their logic.

## Alternatives considered

* **Zig as the universal linker (`cargo-zigbuild`).** Genuinely attractive — one small
  toolchain for Linux *and* macOS cross-compilation. Rejected as the primary mechanism
  because it is a less common path for the C dependencies here (bundled SQLite, ring) and
  failures surface as obscure link errors. It is documented in
  `docs/CROSS_COMPILATION.md` as a supported local fallback.
* **Build everything on native runners.** Cleanest in theory; rejected because it needs
  Linux ARM64/ARMv7 runners, which are slow or unavailable on hosted CI, for no benefit
  over `cross`.
* **Assemble sysroots manually per target.** Rejected: this is what `cross` already
  maintains, and hand-rolled sysroots rot silently.
* **`cargo-ndk` instead of `cargo-apk`.** `cargo-ndk` is better when a Gradle project owns
  the app; `cargo-apk` is better when Rust owns the app, which is our case (Slint's
  Android backend drives `NativeActivity`). Revisit if the APK ever needs Java/Kotlin
  components such as FCM push.

## Consequences

**Positive**

* Every Linux target — including the two the brief cares most about, ARM64 and ARMv7 —
  builds on a standard x64 runner, so the CI matrix is cheap and fast.
* Local reproduction is one script per platform.
* musl agents install on any Linux server regardless of distro or glibc age.

**Negative**

* Three distinct runner types (Linux, Windows, macOS) plus Docker: the pipeline has real
  complexity, and a failure in one leg must not block the others — hence the split
  `native` / `cross` / `android` jobs with independent artifact uploads.
* `cross` requires Docker in CI and locally, which is a real prerequisite for contributors
  targeting ARM.
* Windows ARM64 and macOS cannot be produced on a Linux developer machine at all; those
  artifacts come from CI only.
* Signing keys for Android release APKs (and eventually macOS notarisation) must be
  managed as CI secrets, adding an operational burden documented in `docs/BUILDING.md`.
