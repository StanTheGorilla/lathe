// Quieten everything else while dictating.
//
// Not in the brief; requested during dogfooding (amendment A16). Talking over music
// means the microphone picks the music up and the recogniser transcribes it, and it
// means you cannot hear the cues.
//
// Windows and Linux both expose every application's output stream with a volume of its
// own. macOS does not: Core Audio has no public per-application volume, so there the
// feature is absent and the settings window says so.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as platform;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use self::macos as platform;

use anyhow::Result;

/// Whether this platform can duck at all. The worker skips the attempt and the settings
/// window hides the control when it cannot.
pub fn available() -> bool {
    platform::AVAILABLE
}

/// Holds other applications quiet for as long as it is alive.
///
/// Restoring happens in `Drop`, so an error or a panic mid-dictation cannot leave the
/// user's music turned down with no way back short of the volume mixer.
pub struct Ducker(platform::Ducker);

impl Ducker {
    /// Scales every other application's audio session to `level` (0.0 silent, 1.0
    /// unchanged). Returns a `Ducker` that restores them when dropped.
    pub fn start(level: f32) -> Result<Self> {
        platform::Ducker::start(level.clamp(0.0, 1.0)).map(Self)
    }

    pub fn count(&self) -> usize {
        self.0.count()
    }
}
