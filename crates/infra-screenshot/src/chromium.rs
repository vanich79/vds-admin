//! The headless-browser screenshot provider.
//!
//! Drives a locally installed Chrome, Chromium, Edge or Brave through its command line.
//! See `docs/adr/004-screenshot-architecture.md` for why this rather than Playwright:
//! bundling a browser would add ~150 MB to every artifact and a Node runtime to the
//! build, which is untenable for an APK and for ARM boards.
//!
//! The honest consequence is that previews depend on the user having a Chromium-family
//! browser. When they do not, [`is_available`](ScreenshotProvider::is_available) returns
//! false and the feature reports itself unavailable rather than failing repeatedly.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vds_domain::ids::ProviderId;
use vds_domain::ports::{ScreenshotError, ScreenshotProvider};
use vds_domain::screenshot::{CaptureRequest, CapturedImage, ScreenshotCapabilities};

/// The provider's stable identifier.
pub const PROVIDER_ID: &str = "chromium_cli";

/// Executable names to look for on `PATH`.
const EXECUTABLE_NAMES: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    // Arch, Fedora and the AUR each name Brave differently, and the plain `brave` is the
    // one Arch's own package installs — which is where this list was found wanting.
    "brave",
    "brave-browser",
    "brave-browser-stable",
    "microsoft-edge",
    "microsoft-edge-stable",
    "vivaldi",
    "vivaldi-stable",
    "chromium-freeworld",
    "chrome",
];

/// Fixed locations worth checking when the executable is not on `PATH`.
///
/// Windows and macOS install browsers outside `PATH` as a matter of course, so relying
/// on `PATH` alone would report "no browser" on most desktops.
#[cfg(target_os = "windows")]
const WELL_KNOWN_PATHS: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
];

#[cfg(target_os = "macos")]
const WELL_KNOWN_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const WELL_KNOWN_PATHS: &[&str] = &[
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/brave",
    "/usr/bin/brave-browser",
    "/usr/bin/microsoft-edge-stable",
    "/usr/bin/vivaldi-stable",
    "/usr/lib/chromium/chromium",
    "/opt/brave-bin/brave",
    "/opt/google/chrome/chrome",
    "/snap/bin/chromium",
    "/snap/bin/brave",
    // System-wide Flatpaks. The per-user ones live under `$HOME` and are found by
    // `user_flatpak_paths` instead, since a constant cannot know the home directory.
    "/var/lib/flatpak/exports/bin/org.chromium.Chromium",
    "/var/lib/flatpak/exports/bin/com.google.Chrome",
    "/var/lib/flatpak/exports/bin/com.brave.Browser",
    "/var/lib/flatpak/exports/bin/com.microsoft.Edge",
];

/// Flatpaks installed for this user rather than for the machine.
///
/// `flatpak install --user` is the default when a desktop's software centre offers it, so
/// this is not an exotic case — it is where a browser most often is on a machine whose
/// owner never used `sudo` to get it.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn user_flatpak_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let exports = home.join(".local/share/flatpak/exports/bin");
    [
        "org.chromium.Chromium",
        "com.google.Chrome",
        "com.brave.Browser",
        "com.microsoft.Edge",
    ]
    .iter()
    .map(|id| exports.join(id))
    .collect()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn user_flatpak_paths() -> Vec<PathBuf> {
    Vec::new()
}

/// Captures pages with a local headless browser.
#[derive(Debug, Clone)]
pub struct ChromiumScreenshotProvider {
    executable: Option<PathBuf>,
}

impl ChromiumScreenshotProvider {
    /// Finds a browser automatically.
    pub fn discover() -> Self {
        Self {
            executable: find_browser(),
        }
    }

    /// Uses a specific executable, from settings.
    pub fn with_executable(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            executable: path.exists().then_some(path),
        }
    }

    /// The browser that will be used, if any.
    pub fn executable(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    /// The arguments a capture runs with.
    ///
    /// Split out so the flag set — the part most likely to break on a browser update —
    /// is testable without spawning anything.
    fn arguments(&self, request: &CaptureRequest, output: &Path) -> Vec<String> {
        vec![
            "--headless".to_owned(),
            // Software rendering: monitoring machines are frequently headless servers
            // with no GPU, and the GPU path fails obscurely there.
            "--disable-gpu".to_owned(),
            "--no-sandbox".to_owned(),
            "--disable-dev-shm-usage".to_owned(),
            // Nothing about a preview should touch the user's real browser profile.
            "--incognito".to_owned(),
            "--no-first-run".to_owned(),
            "--disable-extensions".to_owned(),
            // Never sit at a credential or certificate prompt: this is unattended.
            "--disable-features=Translate,MediaRouter".to_owned(),
            "--hide-scrollbars".to_owned(),
            "--mute-audio".to_owned(),
            format!(
                "--window-size={},{}",
                request.viewport_width, request.viewport_height
            ),
            format!(
                "--virtual-time-budget={}",
                request.timeout_secs.saturating_mul(1_000).min(20_000)
            ),
            format!("--screenshot={}", output.display()),
            request.url.clone(),
        ]
    }
}

impl Default for ChromiumScreenshotProvider {
    fn default() -> Self {
        Self::discover()
    }
}

#[async_trait]
impl ScreenshotProvider for ChromiumScreenshotProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        "Local headless browser"
    }

    fn capabilities(&self) -> ScreenshotCapabilities {
        ScreenshotCapabilities {
            // `--screenshot` captures the viewport only. Full-page capture needs the
            // DevTools protocol, which is a different provider.
            supports_full_page: false,
            supports_custom_viewport: true,
            max_viewport_width: 3_840,
            max_viewport_height: 2_160,
        }
    }

    async fn is_available(&self) -> bool {
        self.executable.is_some()
    }

    async fn capture(&self, request: &CaptureRequest) -> Result<CapturedImage, ScreenshotError> {
        let Some(executable) = &self.executable else {
            return Err(ScreenshotError::BackendUnavailable(
                "no Chromium-family browser was found on this machine".to_owned(),
            ));
        };

        // A unique directory per capture: concurrent captures must not overwrite each
        // other's output, and the browser needs a scratch profile it can write to.
        let workspace = tempfile::Builder::new()
            .prefix("vds-screenshot-")
            .tempdir()
            .map_err(|e| ScreenshotError::Backend(format!("could not create a workspace: {e}")))?;
        let output = workspace.path().join("capture.png");

        let mut command = tokio::process::Command::new(executable);
        command
            .args(self.arguments(request, &output))
            .arg(format!("--user-data-dir={}", workspace.path().display()))
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        let timeout = Duration::from_secs(u64::from(request.timeout_secs.max(1)));
        let result = tokio::time::timeout(timeout, command.output()).await;

        let output_status = match result {
            Ok(Ok(status)) => status,
            Ok(Err(err)) => {
                return Err(ScreenshotError::Backend(format!(
                    "could not run the browser: {err}"
                )));
            }
            Err(_) => {
                return Err(ScreenshotError::Timeout {
                    seconds: timeout.as_secs(),
                });
            }
        };

        // The browser exits non-zero for a page that failed to load, but it also writes a
        // perfectly good screenshot of an error page in some versions. The file's
        // existence is the real test.
        if !output.exists() {
            let stderr = String::from_utf8_lossy(&output_status.stderr);
            let reason = stderr
                .lines()
                .last()
                .unwrap_or("the browser produced no image")
                .trim();
            return Err(ScreenshotError::Navigation(reason.to_owned()));
        }

        let png = tokio::fs::read(&output)
            .await
            .map_err(|e| ScreenshotError::Backend(format!("could not read the capture: {e}")))?;

        if png.is_empty() {
            return Err(ScreenshotError::InvalidImage(
                "the capture is empty".to_owned(),
            ));
        }

        let (width, height) = crate::image_ops::png_dimensions(&png)
            .unwrap_or((request.viewport_width, request.viewport_height));

        Ok(CapturedImage { png, width, height })
    }
}

/// Looks for a usable browser.
fn find_browser() -> Option<PathBuf> {
    for path in WELL_KNOWN_PATHS {
        let path = Path::new(path);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    // A Flatpak installed for this user rather than the machine. Checked before `PATH`
    // for the same reason as the fixed locations: it is an exact answer, and searching
    // every directory on `PATH` for a dozen names is not.
    for path in user_flatpak_paths() {
        if path.exists() {
            return Some(path);
        }
    }

    // Then `PATH`, which is how it is found on most Linux systems.
    let search_path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&search_path) {
        for name in EXECUTABLE_NAMES {
            for candidate in executable_candidates(&directory, name) {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

/// Filenames to try for one executable name.
fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(target_os = "windows") {
        vec![directory.join(format!("{name}.exe")), directory.join(name)]
    } else {
        vec![directory.join(name)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ids::WebsiteId;

    fn request() -> CaptureRequest {
        CaptureRequest::new(WebsiteId::new(), "https://example.com/")
    }

    #[test]
    fn the_flag_set_contains_what_headless_capture_needs() {
        let provider = ChromiumScreenshotProvider {
            executable: Some(PathBuf::from("chrome")),
        };
        let arguments = provider.arguments(&request(), Path::new("/tmp/out.png"));
        let joined = arguments.join(" ");

        assert!(joined.contains("--headless"));
        assert!(joined.contains("--screenshot=/tmp/out.png"));
        assert!(joined.contains("--window-size=1280,800"));
        // Servers rarely have a GPU, and the GPU path fails obscurely on them.
        assert!(joined.contains("--disable-gpu"));
        // The URL must be last: Chromium treats the first positional argument as the URL.
        assert_eq!(
            arguments.last().map(String::as_str),
            Some("https://example.com/")
        );
    }

    #[test]
    fn captures_never_touch_the_users_browser_profile() {
        let provider = ChromiumScreenshotProvider {
            executable: Some(PathBuf::from("chrome")),
        };
        let joined = provider
            .arguments(&request(), Path::new("/tmp/out.png"))
            .join(" ");
        assert!(joined.contains("--incognito"));
        assert!(joined.contains("--disable-extensions"));
    }

    #[test]
    fn a_custom_viewport_reaches_the_command_line() {
        let provider = ChromiumScreenshotProvider {
            executable: Some(PathBuf::from("chrome")),
        };
        let mut request = request();
        request.viewport_width = 1_920;
        request.viewport_height = 1_080;

        let joined = provider
            .arguments(&request, Path::new("/tmp/out.png"))
            .join(" ");
        assert!(joined.contains("--window-size=1920,1080"));
    }

    #[test]
    fn the_virtual_time_budget_is_bounded_so_a_slow_page_cannot_hang_the_browser() {
        let provider = ChromiumScreenshotProvider {
            executable: Some(PathBuf::from("chrome")),
        };
        let mut request = request();
        request.timeout_secs = 3_600;

        let budget = provider
            .arguments(&request, Path::new("/tmp/out.png"))
            .into_iter()
            .find(|a| a.starts_with("--virtual-time-budget="))
            .expect("budget present");
        assert_eq!(budget, "--virtual-time-budget=20000");
    }

    #[tokio::test]
    async fn a_machine_without_a_browser_reports_unavailable_rather_than_failing_oddly() {
        let provider = ChromiumScreenshotProvider { executable: None };
        assert!(!provider.is_available().await);

        let err = provider.capture(&request()).await.expect_err("must fail");
        assert!(
            matches!(err, ScreenshotError::BackendUnavailable(_)),
            "got {err:?}"
        );
        // And it is not worth retrying: the browser will not appear on its own.
        assert!(!err.is_retryable());
    }

    #[test]
    fn a_configured_path_that_does_not_exist_is_treated_as_no_browser() {
        // Better than failing every capture with "no such file".
        let provider = ChromiumScreenshotProvider::with_executable("/definitely/not/a/browser");
        assert!(provider.executable().is_none());
    }

    #[test]
    fn discovery_does_not_panic_whatever_the_machine_looks_like() {
        // It may or may not find a browser here; it must not fall over either way.
        let provider = ChromiumScreenshotProvider::discover();
        let _ = provider.executable();
    }

    #[test]
    fn the_provider_advertises_only_what_the_cli_can_do() {
        let provider = ChromiumScreenshotProvider::discover();
        let capabilities = provider.capabilities();
        // `--screenshot` is viewport-only; claiming full-page would make the UI offer
        // something that silently does not happen.
        assert!(!capabilities.supports_full_page);
        assert!(capabilities.supports_custom_viewport);
    }

    #[test]
    fn windows_discovery_also_tries_the_exe_suffix() {
        let candidates = executable_candidates(Path::new("/usr/bin"), "chromium");
        if cfg!(target_os = "windows") {
            assert!(
                candidates
                    .iter()
                    .any(|c| c.to_string_lossy().ends_with(".exe"))
            );
        } else {
            assert_eq!(candidates.len(), 1);
        }
    }

    #[test]
    fn the_provider_id_is_stable() {
        assert_eq!(PROVIDER_ID, "chromium_cli");
    }

    /// Captures a real page with a real browser.
    ///
    /// Ignored by default because it needs a browser installed and takes seconds. Run
    /// with `cargo test -p vds-infra-screenshot -- --ignored` on a desktop.
    #[tokio::test]
    #[ignore = "requires a locally installed browser"]
    async fn a_real_browser_captures_a_real_page() {
        let provider = ChromiumScreenshotProvider::discover();
        if !provider.is_available().await {
            return;
        }

        let mut request = CaptureRequest::new(WebsiteId::new(), "about:blank");
        request.timeout_secs = 30;

        let image = provider.capture(&request).await.expect("captures");
        assert!(!image.png.is_empty());
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(&image.png[1..4], b"PNG");
    }
}
