// Lathe core. Everything on the hot path lives here so the daemon never has to reach
// into the settings frontend, per brief section 3.

pub mod asr;
pub mod asr_backend;
pub mod audio;
pub mod autostart;
pub mod cleanup;
pub mod config;
pub mod cues;
pub mod download;
pub mod ducking;
pub mod engine;
pub mod history;
pub mod hotkey;
pub mod paste;
pub mod update;
// Moved to its own crate so its tests run without the native stack; re-exported so
// every `crate::vocabulary` path keeps working.
pub use lathe_text::vocabulary;
