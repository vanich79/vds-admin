//! Records the target triple so `/v1/info` can report which build is running.
//!
//! `std::env::consts::ARCH` gives only the architecture; an operator debugging a mixed
//! fleet wants to know whether the binary on that box is the musl one or the glibc one,
//! and that distinction is only in the full triple.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=VDS_AGENT_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
