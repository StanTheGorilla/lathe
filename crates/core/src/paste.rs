// Text output, brief section 5.6.
//
// Two paths, chosen by length. SendInput types the text directly and never touches the
// clipboard; the clipboard path is faster for long text but must put back whatever was
// there before, because silently destroying the user's clipboard on every dictation is
// not acceptable.

use anyhow::{Context as _, Result};
use std::thread;
use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_V,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    SendInput,
    Clipboard,
}

/// Brief 5.6: clipboard above the threshold, SendInput below.
pub fn choose(text: &str, clipboard_threshold: usize) -> Method {
    if text.chars().count() > clipboard_threshold {
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
    match method {
        Method::SendInput => {
            type_unicode(text)?;
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
    let mut clipboard = arboard::Clipboard::new().context("opening clipboard")?;
    clipboard.set_text(text).context("writing clipboard")?;
    Ok(())
}

/// Types the text as Unicode key events. No clipboard involvement at all.
fn type_unicode(text: &str) -> Result<()> {
    // UTF-16 code units, so characters outside the BMP go out as their surrogate pair.
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut inputs: Vec<INPUT> = Vec::with_capacity(units.len() * 2);

    for unit in units {
        inputs.push(unicode_input(unit, KEYBD_EVENT_FLAGS(0)));
        inputs.push(unicode_input(unit, KEYEVENTF_KEYUP));
    }

    send(&inputs)
}

fn unicode_input(unit: u16, extra: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | extra,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: VIRTUAL_KEY, extra: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: extra,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    // SendInput takes the whole batch atomically, but very large batches can be
    // rejected, so send in chunks.
    for chunk in inputs.chunks(512) {
        let sent = unsafe { SendInput(chunk, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != chunk.len() {
            anyhow::bail!(
                "SendInput accepted {sent} of {} events (input may be blocked by an \
                 elevated window)",
                chunk.len()
            );
        }
    }
    Ok(())
}

fn paste_via_clipboard(text: &str, restore_delay_ms: u64, keep_on_clipboard: bool) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("opening clipboard")?;

    // Only text is preserved. Restoring arbitrary formats would need the raw Win32
    // clipboard API and a full format enumeration; if the previous contents were an
    // image or files, we deliberately leave them alone rather than half-restore them.
    let previous = if keep_on_clipboard {
        None
    } else {
        clipboard.get_text().ok()
    };

    clipboard.set_text(text).context("writing clipboard")?;
    send_ctrl_v()?;

    if let Some(previous) = previous {
        // Give the target application time to read the clipboard before putting the old
        // contents back. Too short and the paste lands empty.
        thread::sleep(Duration::from_millis(restore_delay_ms));
        let mut clipboard = arboard::Clipboard::new().context("reopening clipboard")?;
        clipboard
            .set_text(previous)
            .context("restoring clipboard")?;
    }

    Ok(())
}

fn send_ctrl_v() -> Result<()> {
    let inputs = [
        key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    send(&inputs)
}
