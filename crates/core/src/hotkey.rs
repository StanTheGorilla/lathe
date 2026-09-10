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
use std::sync::{Arc, Mutex, OnceLock};
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

/// How the dictate binding behaves.
///
/// Brief 5.1 chose `Auto` and deliberately offered no setting. It is still the default,
/// but the threshold surprises people who only ever do one of the two: a slow hold gets
/// read as a hold, a hurried one as a tap, and nothing on screen explains the
/// difference. `Hold` and `Toggle` each commit to one behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A press shorter than the threshold latches; a longer one is push-to-talk.
    #[default]
    Auto,
    /// Records only while the key is held, whatever the press length.
    Hold,
    /// Press to start, press again to stop. Releasing the key does nothing.
    Toggle,
}

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
pub fn run(
    mut bindings: Vec<Bound>,
    mode: Arc<Mutex<Mode>>,
    tap_threshold: Duration,
    events: Sender<Event>,
) {
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
        state_machine(bindings, mode, tap_threshold, raw_rx, events);
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

/// One binding, three possible behaviours; see `Mode`.
/// `mode` is shared rather than copied: changing the press style in settings has to
/// take effect without restarting the app, or it reads as the setting doing nothing.
fn state_machine(
    bindings: Vec<Bound>,
    mode: Arc<Mutex<Mode>>,
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

        let mode = *mode.lock().unwrap();

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
                    // Toggle latches immediately, so the release below finds no
                    // `Pressed` state and is ignored.
                    match mode {
                        Mode::Toggle => State::Latched,
                        _ => State::Pressed {
                            at: Instant::now(),
                            which: event.which,
                        },
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
                let hold = match mode {
                    Mode::Hold => true,
                    Mode::Toggle => false,
                    Mode::Auto => at.elapsed() >= tap_threshold,
                };
                state = if hold {
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

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_millis(400);

    fn bound() -> Vec<Bound> {
        vec![Bound {
            binding: Binding {
                ctrl: true,
                key: 0x20,
                ..Default::default()
            },
            action: Action::Record,
        }]
    }

    /// Drives the state machine with a script of (down, pause-after) and collects what
    /// it emitted. The pause is real time, because the auto mode measures real time.
    fn play(mode: Mode, script: &[(bool, Duration)]) -> Vec<Event> {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();

        let worker = std::thread::spawn(move || {
            state_machine(bound(), Arc::new(Mutex::new(mode)), THRESHOLD, raw_rx, ev_tx);
        });

        for (down, pause) in script {
            raw_tx.send(RawKey { which: 0, down: *down }).unwrap();
            std::thread::sleep(*pause);
        }
        drop(raw_tx);
        worker.join().unwrap();

        ev_rx.into_iter().collect()
    }

    const NONE: Duration = Duration::from_millis(0);
    const BRIEF: Duration = Duration::from_millis(20);
    const LONG: Duration = Duration::from_millis(500);

    fn start() -> Event {
        Event::Start { preset: None }
    }

    #[test]
    fn auto_reads_a_long_press_as_push_to_talk() {
        let events = play(Mode::Auto, &[(true, LONG), (false, BRIEF)]);
        assert_eq!(events, vec![start(), Event::Stop]);
    }

    #[test]
    fn auto_reads_a_quick_tap_as_a_latch() {
        // Press and release quickly: recording stays on, so only Start so far.
        let events = play(Mode::Auto, &[(true, NONE), (false, BRIEF)]);
        assert_eq!(events, vec![start()]);
    }

    #[test]
    fn auto_stops_a_latched_recording_on_the_next_press() {
        let events = play(
            Mode::Auto,
            &[(true, NONE), (false, BRIEF), (true, NONE), (false, BRIEF)],
        );
        assert_eq!(events, vec![start(), Event::Stop]);
    }

    /// The complaint that produced the mode setting: a hurried hold gets latched, and
    /// the user is left recording with no idea why.
    #[test]
    fn hold_stops_on_release_even_when_the_press_was_quick() {
        let events = play(Mode::Hold, &[(true, NONE), (false, BRIEF)]);
        assert_eq!(events, vec![start(), Event::Stop]);
    }

    #[test]
    fn hold_stops_on_release_after_a_long_press_too() {
        let events = play(Mode::Hold, &[(true, LONG), (false, BRIEF)]);
        assert_eq!(events, vec![start(), Event::Stop]);
    }

    #[test]
    fn toggle_ignores_the_release_however_long_the_press() {
        let events = play(Mode::Toggle, &[(true, LONG), (false, BRIEF)]);
        assert_eq!(events, vec![start()], "a held key must not stop a toggle");
    }

    #[test]
    fn toggle_stops_on_the_second_press() {
        let events = play(
            Mode::Toggle,
            &[(true, NONE), (false, BRIEF), (true, NONE), (false, BRIEF)],
        );
        assert_eq!(events, vec![start(), Event::Stop]);
    }

    /// Key repeat fires a stream of key-downs while a key is held. Only the first is a
    /// press, or a held toggle would start and stop dozens of times a second.
    #[test]
    fn key_repeat_does_not_retrigger() {
        let events = play(
            Mode::Toggle,
            &[(true, NONE), (true, NONE), (true, NONE), (false, BRIEF)],
        );
        assert_eq!(events, vec![start()]);
    }
}
