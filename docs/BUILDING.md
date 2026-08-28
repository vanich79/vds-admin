# Building

Building the application and the agent, for development and for release.

For the toolchain setup behind each target, see
[CROSS_COMPILATION.md](CROSS_COMPILATION.md). For why the matrix is split the way it is,
see [ADR-006](adr/006-cross-platform-build.md).

---

## 1. Development builds

```sh
cargo build                          # everything, debug
cargo run -p vds-admin               # the application
cargo build -p vds-agent --release   # the agent
```

### Prerequisites

A stable Rust toolchain, 2024 edition (1.85 or newer). Then:

| Host | Extra |
|---|---|
| Linux | `libfontconfig1-dev`, `libxkbcommon-dev` |
| Windows | nothing |
| macOS | Xcode command-line tools |

Deliberately no cmake, no NASM, no OpenSSL headers, no Node. `rustls` uses `ring` rather
than `aws-lc-rs` and `reqwest` is configured with `rustls-no-provider` precisely to keep
that list short — see [ADR-001](adr/001-technology-stack.md).

### Cargo features

| Feature | Crate | Effect |
|---|---|---|
| `demo-providers` | `infra-analytics`, `infra-screenshot` | Fabricated analytics and placeholder screenshots, for development |
| `testing` | `application` | In-memory fakes for every port |

```sh
cargo run -p vds-admin --features demo-providers
```

Release builds never enable `demo-providers`. Fabricated numbers reaching a production
dashboard would be worse than no numbers, so the fence is a compile-time one.

---

## 2. Profiles

| Profile | Used for | Notable settings |
|---|---|---|
| `dev` | development | unoptimised, debug symbols |
| `release` | the application | `opt-level = "z"`, thin LTO, `panic = "abort"`, stripped |
| `agent-release` | the agent | as `release`, tuned further for size |

The application is optimised for size rather than speed because it spends its life waiting
on I/O; the size shows up in the download and in memory, where a user notices it.

`panic = "abort"` is deliberate. There is no unwinding path worth taking in a GUI whose
state lives in a database — and no `panic!` in production code to unwind from, since
Clippy denies it.

---

## 3. Packaging

Every script lives in `scripts/`, and CI calls the same scripts rather than duplicating
their logic. Anything CI produces you can reproduce locally.

```sh
scripts/build-all.sh          # everything this machine can build; reports what it skipped
scripts/build-linux.sh        # tar.gz, .deb, .rpm, AppImage
scripts/build-windows.sh      # portable .zip, NSIS installer
scripts/build-macos.sh        # universal .app and .dmg
scripts/build-android.sh      # APK
scripts/build-agent.sh        # static agent tarballs, every architecture
```

`build-all.sh` refuses to build anything until `cargo fmt --check`, `cargo lint` and
`cargo test --workspace` all pass. A packaged artefact from an unclean tree is worse than
no artefact.

### Linux

```sh
scripts/build-linux.sh --formats tar,deb,appimage
```

| Format | Needs |
|---|---|
| `tar` | nothing |
| `deb` | `cargo install cargo-deb` |
| `rpm` | `cargo install cargo-generate-rpm` |
| `appimage` | [`appimagetool`](https://github.com/AppImage/AppImageKit/releases) |

A missing tool skips its format with a message rather than failing the run.

The GUI is built against glibc, not musl — the opposite of the agent, for the opposite
reason. It links the system graphics stack, which is dynamically loaded and glibc-linked
everywhere that matters. The AppImage is what covers the distribution spread instead.

### Windows

```sh
scripts/build-windows.sh --installer     # needs makensis
```

Run on Windows. Cross-compiling a Slint GUI from Linux to `*-pc-windows-msvc` needs the
MSVC toolchain and the Windows SDK, which cannot be redistributed; the `*-windows-gnu`
target sidesteps that but behaves differently around DPI awareness and font rendering.
Building on the platform is the honest option, and CI has Windows runners.

The installer is per-user by default, so it needs no elevation. Uninstalling deliberately
leaves the database, configuration and stored credentials alone, and says so — removing an
application must not silently destroy your server list.

### macOS

```sh
scripts/build-macos.sh                   # universal by default
scripts/build-macos.sh --arch arm64      # one architecture, faster
```

Universal because both architectures are still in service and a user who downloads the
wrong one gets an error that does not explain itself. `lipo` merges the two builds; it
costs build time, not runtime.

Signing and notarisation happen when `MACOS_SIGNING_IDENTITY` and `MACOS_NOTARY_PROFILE`
are set. Without them the bundle is unsigned and the script says so — Gatekeeper will
refuse to open it on another Mac.

### Android

```sh
scripts/build-android.sh                       # debug, every ABI
scripts/build-android.sh --abi arm64-v8a       # one ABI, much faster
scripts/build-android.sh --release             # needs ANDROID_KEYSTORE
```

Needs the SDK (`ANDROID_HOME`), the NDK r26+ (`ANDROID_NDK_ROOT`) and
`cargo install cargo-apk`. Setup is in [CROSS_COMPILATION.md](CROSS_COMPILATION.md) §5.

Without a signing key the script builds a *debug* APK and says so, rather than an unsigned
release APK. An unsigned release APK cannot be installed, so producing one is a way of
appearing to succeed while delivering nothing.

### The agent

```sh
scripts/build-agent.sh                   # every architecture
scripts/build-agent.sh x86_64 aarch64    # only these
```

Needs [`cross`](https://github.com/cross-rs/cross) for anything but the host:

```sh
cargo install cross --git https://github.com/cross-rs/cross
```

Without it, the host target still builds with plain `cargo` and the rest are skipped with
a message. A developer checking their own change should not need Docker.

Output is one tarball per architecture in `dist/agent/`, each containing the binary, the
systemd unit, the installer and the annotated example configuration, plus a `SHA256SUMS`
covering all of them.

Every agent build is static against musl. It is installed on machines nobody controls —
a 2019 CentOS box, a current Debian, an Alpine container — and a glibc build fails on
anything older with a link error that means nothing to the person installing it.

CI fails if the agent binary exceeds 8 MiB. It is installed on hosts with 512 MB of RAM
and metered links; a binary that quietly triples in size is a regression.

---

## 4. Releasing

```sh
git tag -a v0.2.0 -m "Release 0.2.0"
git push origin v0.2.0
```

`.github/workflows/release.yml` then:

1. runs the full CI suite — a tag cannot ship code that would fail it;
2. builds the desktop application on Linux, Windows and macOS runners;
3. builds the agent for five architectures;
4. builds the Android APK;
5. collects everything, generates one `SHA256SUMS`, signs it when `GPG_SIGNING_KEY` is
   configured;
6. opens a **draft** release.

The release is a draft on purpose. Someone downloads an artefact, runs it, and publishes
by hand. Nothing is uploaded from a developer's machine.

### Repository secrets

| Secret | Without it |
|---|---|
| `GPG_SIGNING_KEY`, `GPG_PASSPHRASE` | Checksums but no signature; [AGENT.md](AGENT.md) §2.2 explains what that does and does not protect |
| `MACOS_SIGNING_IDENTITY`, `MACOS_NOTARY_PROFILE` | Unsigned macOS bundle |
| `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD` | Debug APK instead of a release one |

Each degrades to something honest rather than failing or, worse, producing an artefact
that looks signed and is not.

---

## 5. Reproducing a build

```sh
git checkout v0.2.0
cargo build --release --locked
```

`Cargo.lock` is committed and `--locked` refuses to change it, so a build from a tag uses
exactly the dependency versions that tag was tested with.

Builds are not bit-for-bit reproducible — paths and timestamps get embedded — and this
document is not going to claim otherwise. `SHA256SUMS` in each release is what lets you
verify an artefact came from that release.

---

## 6. Troubleshooting

**`error: linker 'cc' not found`** — install a C toolchain (`build-essential`, Xcode
command-line tools, or the MSVC build tools).

**`Package fontconfig was not found`** — install `libfontconfig1-dev` and
`libxkbcommon-dev`.

**A Slint compile error naming a `.slint` file** — `apps/ui/build.rs` compiles those at
build time. The message points at the line; it is a real syntax error in the markup.

**`cross` cannot start** — it needs a working Docker or Podman. `docker ps` should
succeed as your user.

**Android: `NDK not found`** — `ANDROID_NDK_ROOT` must point at a specific version
directory, not at the parent:

```sh
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/26.3.11579264
```

**The AppImage will not run** — it needs FUSE. Extract it instead:

```sh
./vds-admin.AppImage --appimage-extract && ./squashfs-root/AppRun
```
