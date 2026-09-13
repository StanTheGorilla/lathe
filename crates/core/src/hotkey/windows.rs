// The Windows hook, per amendment A1.
//
// The hook callback belongs to Windows, not to us. It must do nothing but classify the
// event and hand it off: Windows silently unhooks a callback that exceeds
// LowLevelHooksTimeout (300ms by default), and a dropped hook fails silently.

use super::{Binding, Bound, RawKey};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

/// How the Windows key is written back to the user.
pub(super) const SUPER_LABEL: &str = "Win";

static HOOK_TX: OnceLock<Sender<RawKey>> = OnceLock::new();
static BINDINGS: OnceLock<Vec<Bound>> = OnceLock::new();

fn modifiers_held(binding: &Binding) -> bool {
    // Read the modifier state at event time rather than tracking it ourselves, so the
    // hook cannot drift out of sync after a focus change or a missed key-up.
    let down = |vk: VIRTUAL_KEY| unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 };
    down(VK_CONTROL) == binding.ctrl
        && down(VK_MENU) == binding.alt
        && down(VK_SHIFT) == binding.shift
        && down(VK_LWIN) == binding.win
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let vk = info.vkCode as u16;
        let msg = wparam.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

        if let Some(bindings) = BINDINGS.get() {
            // First match wins. More specific combinations are sorted ahead of less
            // specific ones at registration, so Ctrl+Shift+Space is tested before
            // Ctrl+Space and the two do not shadow each other.
            if down || up {
                if let Some(which) = bindings
                    .iter()
                    .position(|b| vk == b.binding.key && modifiers_held(&b.binding))
                {
                    if let Some(tx) = HOOK_TX.get() {
                        let _ = tx.send(RawKey { which, down });
                    }
                    // Swallow it so it never reaches the focused application.
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Installs the hook and runs the message loop. Never returns.
///
/// A WH_KEYBOARD_LL hook only delivers events to a thread that pumps messages, so this
/// owns a thread for the lifetime of the process.
pub(super) fn listen(bindings: Vec<Bound>, raw_tx: Sender<RawKey>) {
    let _ = HOOK_TX.set(raw_tx);
    let _ = BINDINGS.set(bindings);

    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
        if hook.is_err() {
            eprintln!("failed to install the keyboard hook: {:?}", hook.err());
            return;
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
