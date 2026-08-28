# ADR-004 — Screenshots: headless-browser CLI provider behind a capability-gated port

* **Status:** Accepted
* **Date:** 2026-08-26

## Context

Website cards should show a visual preview. Requirements: capture must never block the
UI, results must be cached with an explicit capture time, stale captures must be labelled
rather than passed off as current, and the mechanism must not make the Android APK or the
ARM builds impractical.

The obvious candidate, Playwright, would drag a Node.js runtime and a ~150 MB bundled
browser into a native application. That is unacceptable for an APK and for an ARMv7 SBC,
and it would make the build depend on a second package ecosystem.

## Decision

**Port:**

```rust
#[async_trait]
pub trait ScreenshotProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn capabilities(&self) -> ScreenshotCapabilities;
    async fn is_available(&self) -> bool;
    async fn capture(&self, req: CaptureRequest) -> Result<CapturedImage, ScreenshotError>;
}
```

**MVP implementation — `ChromiumCliProvider`:** drives a *locally installed* Chrome,
Chromium, Edge or Brave through its headless CLI
(`--headless --disable-gpu --screenshot=<path> --window-size=WxH`), discovering the
binary at runtime across the standard install locations for each OS, overridable in
settings. Zero build-time dependencies, zero bundled megabytes, and it degrades honestly:
if no browser is found, `is_available()` returns `false`, the capability is reported as
unsupported, and the UI hides previews instead of showing errors.

**Other implementations:** `UnavailableProvider` (Android and headless hosts — always
reports unsupported), `DemoScreenshotProvider` (development only, see below), and
`RemoteBrowserProvider` as the designed future answer for Android — an HTTP screenshot
service, which is the only sane way to get previews on a phone.

**Service policy** — `application::screenshots::ScreenshotService`:

* capture runs on the shared scheduler at the **lowest** priority with a concurrency limit
  of 1–2, so it can never contend with availability checks;
* results are stored as a full PNG plus a downscaled thumbnail, with `captured_at`,
  `status` and a content hash;
* refresh interval is configurable (hourly / 6 h / 24 h / manual) and captures are *not*
  triggered by opening the app;
* mobile loads thumbnails first and the full image only when a card is opened;
* the UI always renders the age of a cached capture ("Captured 4 hours ago"), and renders
  distinct states for "website offline" and "capture failed — retry".

**Demo provider isolation:** `DemoScreenshotProvider` and `DemoAnalyticsProvider` are
behind the `demo-providers` Cargo feature, which is off by default and is not enabled in
any release profile. They cannot be selected at runtime in a production build.

## Alternatives considered

* **Playwright / Puppeteer.** Rejected: Node runtime + bundled browser, hostile to APK
  size and ARM builds.
* **Bundle a headless browser (CEF or similar).** Rejected for the same size reasons,
  multiplied across nine desktop targets.
* **Third-party screenshot API as the default.** Rejected as a default: it sends every
  monitored URL to an external service, which is a privacy regression for a tool that is
  otherwise fully standalone. It remains available as an `ExternalScreenshotProvider`
  the user can opt into.
* **Render previews with the app's own renderer.** Not viable — Slint is not a web engine.

## Consequences

**Positive**

* No new toolchain, no bundled binaries, no growth in artifact size.
* The capability system means a missing browser is a *feature being unavailable*, not a
  crash or an error state.
* Cache + age labelling satisfies the honesty requirement about stale images.

**Negative**

* Preview availability depends on the user's machine having a Chromium-family browser.
  For desktop this is close to universal; for Android and headless servers it is not —
  which is exactly why `RemoteBrowserProvider` is in the design.
* Driving a browser by CLI gives coarse control: no per-request cookie/auth injection, no
  waiting on custom selectors. Sufficient for previews; anything richer would justify a
  CDP-based provider, addable as a fourth implementation.
* Spawning a browser process is heavy (~1–2 s, hundreds of MB peak). Contained by the
  low-priority queue, the concurrency cap of 1–2, and infrequent refresh defaults.
