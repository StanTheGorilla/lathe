// Text output, brief section 5.6.
//
// Two paths, chosen by length. The typed path sends the text as key events and never
// touches the clipboard; the clipboard path is faster for long text but must put back
// whatever was there before, because silently destroying the user's clipboard on every
// dictation is not acceptable.
//
// Each platform supplies the three primitives -- are the modifiers up, type this text,
// press paste -- and everything above them is shared. Linux cannot type: uinput sends
// key codes, not characters, so every Linux dictation goes through the clipboard.

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

use anyhow::{Context as _, Result};
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    SendInput,
    Clipboard,
}

/// Brief 5.6: clipboard above the threshold, SendInput below.
pub fn choose(text: &str, clipboard_threshold: usize) -> Method {
    if !platform::CAN_TYPE || text.chars().count() > clipboard_threshold {
        Method::Clipboard
    } else {
        Method::SendInput
    }
}

/// `keep_on_clipboard` leaves the dictated text on the clipboard afterwards, so it can
/// be pasted again by hand. It overrides the restore behaviour described in brief 5.6:
/// the two cannot both be true, since the clipboard holds one thing. See amendment A12.
pub fn paste(
    text: &str,
    method: Method,
    restore_delay_ms: u64,
    keep_on_clipboard: bool,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    // The hotkey's modifiers are often still physically down when the text arrives: a
    // latched recording stops on the *press* of Ctrl+Space, and a short sentence is
    // typed well within the time it takes to lift the finger. Typed with Ctrl held,
    // every character is a chord to the target app (Ctrl+, opened Windows Terminal's
    // settings). Waiting is safer than injecting key-ups, which would desync the
    // hotkey hook's own reads of the same state.
    if !wait_until(platform::modifiers_up, Duration::from_secs(2)) {
        eprintln!("paste: a modifier key is still held after 2s, pasting anyway");
    }
    match method {
        Method::SendInput => {
            platform::type_unicode(text)?;
            // The SendInput path does not otherwise involve the clipboard at all, so
            // this is the only way short dictations end up there.
            if keep_on_clipboard {
                copy_only(text)?;
            }
            Ok(())
        }
        Method::Clipboard => paste_via_clipboard(text, restore_delay_ms, keep_on_clipboard),
    }
}

/// Copies to the clipboard without pasting. Used when a preset has auto_paste off.
pub fn copy_only(text: &str) -> Result<()> {
    let mut clipboard = clipboard()?;
    clipboard.set_text(text).context("writing clipboard")?;
    Ok(())
}

/// A clipboard handle. Fresh each time on Windows and macOS, where the system holds
/// the contents.
#[cfg(not(target_os = "linux"))]
fn clipboard() -> Result<arboard::Clipboard> {
    arboard::Clipboard::new().context("opening clipboard")
}

/// Under X11 and Wayland the clipboard's contents live in the process that set them
/// and vanish when the handle that set them is dropped. One handle for the life of the
/// process keeps the last dictation pasteable.
#[cfg(target_os = "linux")]
fn clipboard() -> Result<std::sync::MutexGuard<'static, arboard::Clipboard>> {
    use std::sync::{Mutex, OnceLock};
    static CLIPBOARD: OnceLock<Mutex<arboard::Clipboard>> = OnceLock::new();
    if CLIPBOARD.get().is_none() {
        let fresh = arboard::Clipboard::new().context("opening clipboard")?;
        let _ = CLIPBOARD.set(Mutex::new(fresh));
    }
    Ok(CLIPBOARD
        .get()
        .expect("set above")
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// Polls `ready` until it holds or `timeout` passes; true if it held.
fn wait_until(ready: impl Fn() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if ready() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn paste_via_clipboard(text: &str, restore_delay_ms: u64, keep_on_clipboard: bool) -> Result<()> {
    let mut clipboard = clipboard()?;

    // Only text is preserved. Restoring arbitrary formats would need the raw Win32
    // clipboard API and a full format enumeration; if the previous contents were an
    // image or files, we deliberately leave them alone rather than half-restore them.
    let previous = if keep_on_clipboard {
        None
    } else {
        clipboard.get_text().ok()
    };

    clipboard.set_text(text).context("writing clipboard")?;
    platform::send_paste_chord()?;

    if let Some(previous) = previous {
        // Give the target application time to read the clipboard before putting the old
        // contents back. Too short and the paste lands empty.
        thread::sleep(Duration::from_millis(restore_delay_ms));
        clipboard
            .set_text(previous)
            .context("restoring clipboard")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn waits_for_the_condition_and_reports_it_held() {
        let polls = Cell::new(0);
        let held = wait_until(
            || {
                polls.set(polls.get() + 1);
                polls.get() >= 3
            },
            Duration::from_secs(1),
        );
        assert!(held);
        assert_eq!(polls.get(), 3);
    }

    #[test]
    fn gives_up_at_the_timeout() {
        let start = Instant::now();
        assert!(!wait_until(|| false, Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
    }
}
