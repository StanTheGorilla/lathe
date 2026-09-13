// Launch at login. Brief section 5.7.
//
// Each platform has one place the user already knows to look for this -- the Startup
// list in Windows Settings, Login Items on macOS, the autostart folder on a Linux
// desktop -- and each module below writes exactly there and nowhere else.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use self::linux as platform;

use anyhow::Result;

pub fn is_enabled() -> bool {
    platform::is_enabled()
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    platform::set_enabled(enabled)
}
