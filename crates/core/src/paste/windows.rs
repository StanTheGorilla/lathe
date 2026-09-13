// SendInput for both paths: Unicode key events for typing, a Ctrl+V chord for pasting.

use anyhow::Result;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
    VK_V,
};

pub(super) const CAN_TYPE: bool = true;

pub(super) fn modifiers_up() -> bool {
    let down = |vk: VIRTUAL_KEY| unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 };
    !(down(VK_CONTROL) || down(VK_MENU) || down(VK_SHIFT) || down(VK_LWIN))
}

/// Types the text as Unicode key events. No clipboard involvement at all.
pub(super) fn type_unicode(text: &str) -> Result<()> {
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

pub(super) fn send_paste_chord() -> Result<()> {
    let inputs = [
        key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    send(&inputs)
}
