// macOS has no public API for another application's output volume, so this never
// ducks anything. It exists so the shared type compiles; `available()` is false.

use anyhow::Result;

pub(super) const AVAILABLE: bool = false;

pub(super) struct Ducker;

impl Ducker {
    pub(super) fn start(_level: f32) -> Result<Self> {
        anyhow::bail!("ducking other applications is not available on macOS")
    }

    pub(super) fn count(&self) -> usize {
        0
    }
}
