//! Finds a newer Clayo on GitHub and installs it with `scripts/install.ps1`, the same script
//! the README's install command runs. The script waits for this Clayo to quit, swaps the folder
//! and starts the new one, so Clayo only has to start it and quit. The HTTP request itself is the
//! app's (WinHTTP); this is everything around it.

use std::fmt;
use std::path::Path;
use std::process::Command;

/// The GitHub repo releases come from, set at build time (PORTING.md D12).
pub const REPO: &str = match option_env!("CLAYO_UPDATE_REPO") {
    Some(r) => r,
    None => "YogiOnCode/Clayo",
};

/// GitHub refuses requests without a User-Agent.
pub const USER_AGENT: &str = "Clayo";
pub const TIMEOUT_SECS: u64 = 15;

/// A dev build would update its own target folder with the release, so it never checks.
pub const ENABLED: bool = !cfg!(debug_assertions);

/// A release version, compared on its three parts: 1.0.0 and 1.0.0.0 are the same release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            build,
        }
    }

    /// Two to four dot-separated numbers, as .NET's `Version.TryParse` takes them; a fourth
    /// part is dropped.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if !(2..=4).contains(&parts.len()) {
            return None;
        }
        let mut n = [0u32; 4];
        for (slot, p) in n.iter_mut().zip(&parts) {
            let p = p.trim();
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *slot = p.parse().ok().filter(|v| *v <= i32::MAX as u32)?;
        }
        Some(Self::new(n[0], n[1], n[2]))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)
    }
}

/// The latest-release endpoint, the only request Clayo makes.
pub fn latest_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// The script as it was released with that version, not as main has it now: a push to main
/// never reaches anyone's Update button.
pub fn script_url(version: Version) -> String {
    format!("https://raw.githubusercontent.com/{REPO}/v{version}/scripts/install.ps1")
}

/// The release's page on GitHub: its notes are what's new in that version.
pub fn release_url(version: Version) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// A tag like "v1.0.1", when it is a later version than `current`.
pub fn newer(tag: Option<&str>, current: Version) -> Option<Version> {
    let latest = Version::parse(tag?.trim_start_matches(['v', 'V']))?;
    (latest > current).then_some(latest)
}

/// `tag_name` of the latest-release JSON GitHub sent; None when it isn't that.
pub fn tag_from_json(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("tag_name")?.as_str().map(str::to_string)
}

/// The release in that JSON when it is newer than `current` (pass
/// `Version::parse(env!("CARGO_PKG_VERSION"))`).
pub fn newer_from_json(body: &str, current: Version) -> Option<Version> {
    newer(tag_from_json(body).as_deref(), current)
}

/// The install script in a PowerShell window you can watch; the caller then quits. On a failure
/// the window waits, or it would close before the error could be read. `CLAYO_DIR` makes it
/// update this Clayo where it is, not a copy in the default folder.
pub fn install_command(version: Version, clayo_dir: &Path) -> Command {
    let mut c = Command::new("powershell.exe");
    c.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &format!(
            "try {{ irm '{}' | iex }} catch {{ Write-Host $_ -ForegroundColor Red; pause }}",
            script_url(version)
        ),
    ]);
    c.env("CLAYO_DIR", clayo_dir);
    c
}
