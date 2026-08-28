# Cross-compilation

One codebase, thirteen artefacts. This document says what each target needs, which host
can build it, and how to set that up.

The reasoning behind the split is in [ADR-006](adr/006-cross-platform-build.md); the
commands are in [BUILDING.md](BUILDING.md). This is the toolchain reference.

---

## 1. The matrix

| Target | Triple | Build on | Mechanism |
|---|---|---|---|
| Windows x64 | `x86_64-pc-windows-msvc` | Windows | native |
| Windows x86 | `i686-pc-windows-msvc` | Windows | native |
| Windows ARM64 | `aarch64-pc-windows-msvc` | Windows | MSVC cross |
| Linux x64 | `x86_64-unknown-linux-gnu` | any | `cross` |
| Linux x86 | `i686-unknown-linux-gnu` | any | `cross` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | any | `cross` |
| Linux ARMv7 | `armv7-unknown-linux-gnueabihf` | any | `cross` |
| macOS ARM64 | `aarch64-apple-darwin` | macOS | native |
| macOS x64 | `x86_64-apple-darwin` | macOS | native |
| Android arm64-v8a | `aarch64-linux-android` | any | NDK |
| Android armeabi-v7a | `armv7-linux-androideabi` | any | NDK |
| Android x86_64 | `x86_64-linux-android` | any | NDK |
| Android x86 | `i686-linux-android` | any | NDK |

Plus the agent, which is static-first:

| Agent target | Triple |
|---|---|
| x86_64 | `x86_64-unknown-linux-musl` |
| ARM64 | `aarch64-unknown-linux-musl` |
| ARMv7 | `armv7-unknown-linux-musleabihf` |
| ARMv6 | `arm-unknown-linux-musleabihf` |
| x86 | `i686-unknown-linux-musl` |

### Why a linker, and not just `--target`

Rust compiles for any of these from any host. The difficulty is always the **linker** and
the target's C toolchain, and this project needs one because two dependencies contain C:

- `rusqlite` builds SQLite from bundled C source;
- `ring` (through `rustls`) has C and assembly.

Both were chosen knowing this. The alternatives were worse: a system SQLite means a
runtime dependency on whatever version the distribution has, and `aws-lc-rs` needs cmake
and NASM, which is a far heavier prerequisite than a C compiler that CI images already
carry.

### iOS

Not built. Slint supports it, and the architecture has nothing platform-specific in the
way, so it is a question of Apple developer enrolment, provisioning and a review process —
not of code. It is out of scope for this version rather than blocked by it.

---

## 2. Linux, with `cross`

```sh
cargo install cross --git https://github.com/cross-rs/cross
```

`cross` runs the build inside a container that already has the matching GCC
cross-toolchain, so there are no sysroots to assemble by hand:

```sh
cross build --release --target aarch64-unknown-linux-gnu --package vds-admin
cross build --profile agent-release --target armv7-unknown-linux-musleabihf --package vds-agent
```

It needs a working Docker or Podman — `docker ps` must succeed as your user.

### Without `cross`

Install the toolchain and let Cargo use it. `.cargo/config.toml` already names the
linkers:

```toml
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"

[target.armv7-unknown-linux-gnueabihf]
linker = "arm-linux-gnueabihf-gcc"
```

```sh
sudo apt install gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
```

For musl, add `musl-tools` and the matching `musl-gcc` cross-compilers.

### `cargo-zigbuild` as a fallback

```sh
pip install ziglang && cargo install cargo-zigbuild
cargo zigbuild --release --target aarch64-unknown-linux-gnu
```

Genuinely convenient — one small toolchain covering Linux and, with an SDK, macOS. Not the
primary mechanism because it is a less-travelled path for bundled SQLite and `ring`, and
when it fails it fails as an obscure link error. Useful locally; CI uses `cross`.

---

## 3. Windows

Build on Windows. Install the **Visual Studio Build Tools** with the "Desktop development
with C++" workload, then:

```sh
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

ARM64 cross-compiles from an x64 Windows host, because MSVC ships the ARM64 toolchain:

```sh
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

### From Linux

`*-pc-windows-msvc` needs the MSVC toolchain and the Windows SDK, which cannot be
redistributed. `*-pc-windows-gnu` works with MinGW:

```sh
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

Fine for a smoke test. Not what is released: the GNU target behaves differently around
DPI awareness and font rendering, which is exactly the kind of difference a user notices
and a developer does not.

---

## 4. macOS

Build on macOS. Xcode command-line tools:

```sh
xcode-select --install
rustup target add aarch64-apple-darwin x86_64-apple-darwin
scripts/build-macos.sh
```

An Apple-silicon machine builds both and `lipo` merges them into a universal binary.

Cross-compiling from Linux is possible with `osxcross` and an extracted SDK, and the
SDK's licence makes that impractical to do openly. CI uses macOS runners.

---

## 5. Android

The one target with a real setup cost.

### 5.1 SDK and NDK

Either Android Studio, or the command-line tools:

```sh
mkdir -p ~/android && cd ~/android
curl -O https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip commandlinetools-linux-*.zip
mkdir -p cmdline-tools/latest && mv cmdline-tools/* cmdline-tools/latest/ 2>/dev/null || true

export ANDROID_HOME=~/android
export PATH=$PATH:$ANDROID_HOME/cmdline-tools/latest/bin

sdkmanager --install "platform-tools" "platforms;android-34" "ndk;26.3.11579264"
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/26.3.11579264
```

`ANDROID_NDK_ROOT` must point at a **specific version directory**, not at `ndk/`. That
mistake produces a "NDK not found" error that names a path which visibly exists.

### 5.2 Rust and cargo-apk

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi \
                  x86_64-linux-android i686-linux-android
cargo install cargo-apk
```

### 5.3 Building

```sh
scripts/build-android.sh --abi arm64-v8a     # start here; one ABI is much faster
scripts/build-android.sh                     # all four
```

### 5.4 Signing

```sh
keytool -genkeypair -v -keystore release.keystore \
        -alias vds-admin -keyalg RSA -keysize 4096 -validity 10000

export ANDROID_KEYSTORE=$PWD/release.keystore
export ANDROID_KEYSTORE_PASSWORD=...
scripts/build-android.sh --release
```

Keep the keystore. An Android app can only be updated by a build signed with the same key;
lose it and the only way forward is a new package name and every user reinstalling.

Without a key the script builds a debug APK and says so.

### 5.5 NDK and Rust versions

The NDK moves faster than Rust's Android support. r26 works with current stable. r27
changed the layout of some sysroot libraries and needs a newer `cargo-apk`; if a link
error mentions `libgcc` or `libunwind`, that mismatch is why.

---

## 6. The agent

Static against musl, always. It lands on machines whose glibc version is unknown, and a
glibc build fails on anything older with a link error that helps nobody.

```sh
scripts/build-agent.sh                   # every architecture, via cross
scripts/build-agent.sh x86_64            # one
```

Verify a build is genuinely static:

```sh
file dist/agent/vds-agent          # "statically linked"
ldd  dist/agent/vds-agent          # "not a dynamic executable"
```

ARMv6 is built for the original Raspberry Pi and Zero. It costs one line in the matrix,
and those machines are still deployed in the places this application is most useful.

---

## 7. CI

`.github/workflows/ci.yml` runs on every push:

| Job | Runner | Does |
|---|---|---|
| `lint` | Ubuntu | `fmt --check`, `cargo lint`, the layering check, `shellcheck` |
| `test` | Ubuntu, Windows, macOS | the suite, plus release-profile tests on Linux |
| `agent` | Ubuntu | four musl targets via `cross`, and a binary size check |
| `build` | Ubuntu, Windows, macOS | the application |
| `android` | Ubuntu | a debug APK |

`.github/workflows/release.yml` adds the packaging and publishing steps on a tag. Both
call `scripts/build-*.sh` rather than duplicating their logic, so a local build and a CI
build run the same commands.

---

## 8. Troubleshooting

**`linker 'aarch64-linux-gnu-gcc' not found`** — the cross-toolchain is not installed. Use
`cross`, or install the package.

**`cross` hangs or cannot start** — Docker is not running, or your user cannot reach it.
`docker ps` must succeed.

**`cannot find -lsqlite3`** — the bundled build did not run. Check that `rusqlite`'s
`bundled` feature is enabled; it is in the workspace manifest, and a local override can
turn it off.

**Android: `NDK not found` at a path that exists** — `ANDROID_NDK_ROOT` points at the
parent rather than at a version directory. See §5.1.

**Android: link errors mentioning `libgcc` or `libunwind`** — NDK/`cargo-apk` version
mismatch. See §5.5.

**musl build fails on `ring`** — install `musl-tools`, or use `cross`, which has it.

**The Windows GUI opens a console window** — a debug build does this deliberately, so
`println!` goes somewhere visible. Release builds set `windows_subsystem = "windows"`.
