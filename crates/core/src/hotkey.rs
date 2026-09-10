// Global hotkey via a low-level keyboard hook, per amendment A1.
//
// `RegisterHotKey` cannot implement brief 5.1's auto-detect press style, because
// WM_HOTKEY only fires on key-down: there is no key-up message, so press duration is
// unmeasurable. WH_KEYBOARD_LL sees both edges.
//
// The hook callback belongs to Windows, not to us. It must do nothing but classify the
// event and hand it off: Windows silently unhooks a callback that exceeds
// LowLevelHooksTimeout (300ms by default), and a dropped hook fails silently.

use anyhow::{anyhow, Result};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Begin capturing. `preset` names a preset when a preset-specific binding started
    /// it, and is `None` for the main binding, which uses whatever the tray has selected.
    Start { preset: Option<String> },
    /// Stop capturing and process.
    Stop,
    /// Brief 5.6: re-paste the previous raw transcript.
    PasteRaw,
}

/// What a binding does. Brief 5.3 gives each preset an optional hotkey of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Dictate with the preset selected in the tray.
    Record,
    /// Dictate with a named preset, regardless of the tray selection.
    RecordPreset(String),
    PasteRaw,
}

/// A binding and what it triggers.
#[derive(Debug, Clone)]
pub struct Bound {
    pub binding: Binding,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Binding {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: u16,
}

impl Binding {
    /// Parses strings like "Ctrl+Alt+Space".
    pub fn parse(spec: &str) -> Result<Self> {
        let mut binding = Binding::default();
        for part in spec.split('+') {
            let part = part.trim();
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => binding.ctrl = true,
                "alt" => binding.alt = true,
                "shift" => binding.shift = true,
                "win" | "super" | "meta" => binding.win = true,
                "" => continue,
                other => {
                    if binding.key != 0 {
                        return Err(anyhow!("hotkey '{spec}' names more than one main key"));
                    }
                    binding.key = key_code(other)
                        .ok_or_else(|| anyhow!("hotkey '{spec}': unknown key '{part}'"))?;
                }
            }
        }
        if binding.key == 0 {
            return Err(anyhow!("hotkey '{spec}' has no main key"));
        }
        Ok(binding)
    }
}

fn key_code(name: &str) -> Option<u16> {
    Some(match name {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "capslock" => 0x14,
        other => {
            let mut chars = other.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                // Function keys.
                if let Some(rest) = other.strip_prefix('f') {
                    let n: u8 = rest.parse().ok()?;
                    if (1..=24).contains(&n) {
                        return Some(0x70 + n as u16 - 1);
                    }
                }
                return None;
            }
            if first.is_ascii_alphanumeric() {
                first.to_ascii_uppercase() as u16
            } else {
                return None;
            }
        }
    })
}

/// What the hook thread sends to the state machine: which binding matched, and whether
/// this was the press or the release.
struct RawKey {
    /// Index into `BINDINGS`.
    which: usize,
    down: bool,
}

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
pub fn run(mut bindings: Vec<Bound>, tap_threshold: Duration, events: Sender<Event>) {
    // Most modifiers first. Without this, a binding of Ctrl+Space registered before
    // Ctrl+Shift+Space would swallow the latter, since the hook takes the first match.
    bindings.sort_by_key(|b| {
        let m = &b.binding;
        std::cmp::Reverse(
            m.ctrl as u8 + m.alt as u8 + m.shift as u8 + m.win as u8,
        )
    });

    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<RawKey>();
    let _ = HOOK_TX.set(raw_tx);
    let _ = BINDINGS.set(bindings.clone());

    std::thread::spawn(move || {
        state_machine(bindings, tap_threshold, raw_rx, events);
    });

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

/// Brief 5.1: one binding, two behaviors, no mode setting.
///
/// A press shorter than the threshold latches recording on until the next press. A
/// longer press records only while held.
fn state_machine(
    bindings: Vec<Bound>,
    tap_threshold: Duration,
    raw: std::sync::mpsc::Receiver<RawKey>,
    events: Sender<Event>,
) {
    enum State {
        Idle,
        /// Key still down; we do not yet know whether this is a tap or a hold. Carries
        /// the binding that started it, so its release is the one that matters.
        Pressed { at: Instant, which: usize },
        /// It was a tap, so recording stays on until the next press.
        Latched,
    }

    let mut state = State::Idle;
    // Key repeat sends a stream of key-downs while held; only the first is a press.
    let mut down_binding: Option<usize> = None;

    for event in raw {
        let Some(bound) = bindings.get(event.which) else {
            continue;
        };

        if bound.action == Action::PasteRaw {
            if event.down && down_binding != Some(event.which) {
                eprintln!("hotkey: {} -> paste raw", describe(&bound.binding));
                let _ = events.send(Event::PasteRaw);
            }
            down_binding = if event.down { Some(event.which) } else { None };
            continue;
        }

        let preset = match &bound.action {
            Action::RecordPreset(name) => Some(name.clone()),
            _ => None,
        };

        if event.down {
            if down_binding == Some(event.which) {
                continue;
            }
            down_binding = Some(event.which);
            state = match state {
                // Any recording binding stops a latched recording, so a mistaken second
                // preset key does not start a second capture on top of the first.
                State::Latched => {
                    let _ = events.send(Event::Stop);
                    State::Idle
                }
                State::Idle => {
                    eprintln!(
                        "hotkey: {} -> start{}",
                        describe(&bound.binding),
                        match &preset {
                            Some(p) => format!(" (preset {p})"),
                            None => String::new(),
                        }
                    );
                    let _ = events.send(Event::Start { preset });
                    State::Pressed {
                        at: Instant::now(),
                        which: event.which,
                    }
                }
                other => other,
            };
        } else {
            down_binding = None;
            if let State::Pressed { at, which } = state {
                // Only the binding that started this recording ends it.
                if which != event.which {
                    state = State::Pressed { at, which };
                    continue;
                }
                state = if at.elapsed() >= tap_threshold {
                    let _ = events.send(Event::Stop);
                    State::Idle
                } else {
                    State::Latched
                };
            }
        }
    }
}

/// Renders a binding the way the user wrote it, for log lines.
pub fn describe(b: &Binding) -> String {
    let mut parts = Vec::new();
    if b.ctrl {
        parts.push("Ctrl".to_string());
    }
    if b.alt {
        parts.push("Alt".to_string());
    }
    if b.shift {
        parts.push("Shift".to_string());
    }
    if b.win {
        parts.push("Win".to_string());
    }
    parts.push(match b.key {
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        k if (0x70..=0x87).contains(&k) => format!("F{}", k - 0x70 + 1),
        k => char::from_u32(k as u32)
            .map(|c| c.to_string())
            .unwrap_or_else(|| format!("0x{k:02X}")),
    });
    parts.join("+")
}
