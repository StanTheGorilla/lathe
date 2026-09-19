// Global hotkey, per amendment A1 and amendment A30.
//
// `RegisterHotKey` and its equivalents cannot implement brief 5.1's auto-detect press
// style, because they only report key-down: there is no key-up message, so press
// duration is unmeasurable. Each platform therefore watches the keyboard itself --
// a low-level hook on Windows, an event tap on macOS, the input devices on Linux --
// and reports both edges here. The state machine that turns edges into events is
// shared; only the listening differs.

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
#[cfg(target_os = "linux")]
pub(crate) use self::linux::modifiers_up as linux_modifiers_up;
#[cfg(target_os = "linux")]
pub use self::linux::keyboard_readable as linux_keyboard_readable;
#[cfg(target_os = "macos")]
pub use self::macos::accessibility_trusted as macos_accessibility_trusted;

use anyhow::{anyhow, Result};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub binding: Binding,
    pub action: Action,
}

/// The bindings in force, shared by the platform listener, the state machine and the
/// config watcher. Swapping the list is what makes a hotkey change take effect without
/// a restart: the listener is installed once and reads through this on every key.
#[derive(Clone)]
pub struct Bindings(Arc<RwLock<Vec<Bound>>>);

impl Bindings {
    pub fn new(list: Vec<Bound>) -> Self {
        let this = Bindings(Arc::new(RwLock::new(Vec::new())));
        this.set(list);
        this
    }

    /// Replaces the list, and says whether that changed anything. Most modifiers
    /// first: without this, a binding of Ctrl+Space registered before
    /// Ctrl+Shift+Space would swallow the latter, since the listener takes the first
    /// match.
    pub fn set(&self, mut list: Vec<Bound>) -> bool {
        list.sort_by_key(|b| {
            let m = &b.binding;
            std::cmp::Reverse(m.ctrl as u8 + m.alt as u8 + m.shift as u8 + m.win as u8)
        });
        let mut current = self.0.write().unwrap();
        if *current == list {
            return false;
        }
        *current = list;
        true
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Vec<Bound>> {
        self.0.read().unwrap()
    }

    /// The rendered binding for an action, for the tray and the toasts.
    pub fn label(&self, action: &Action) -> Option<String> {
        self.read()
            .iter()
            .find(|b| b.action == *action)
            .map(|b| describe(&b.binding))
    }
}

/// Pairs each key-up with the key-down that matched, so a release counts for the
/// binding that started it whatever the modifiers are doing by then. Without this,
/// letting go of Ctrl a moment before Space makes the Space release look like an
/// unbound key, the hold never ends, and the next press is read as key repeat.
///
/// `K` is the platform's own key identity; repeats of a held key report the binding
/// it matched first.
pub(crate) struct Matcher<K> {
    down: Vec<(usize, K)>,
}

impl<K: PartialEq + Copy> Matcher<K> {
    pub(crate) const fn new() -> Self {
        Matcher { down: Vec::new() }
    }

    /// Which binding this edge belongs to, if any. `find` is consulted only for a
    /// fresh press; a key already down answers for itself.
    pub(crate) fn resolve(
        &mut self,
        key: K,
        down: bool,
        find: impl FnOnce() -> Option<usize>,
    ) -> Option<usize> {
        if let Some(i) = self.down.iter().position(|(_, k)| *k == key) {
            let (which, _) = self.down[i];
            if !down {
                self.down.remove(i);
            }
            return Some(which);
        }
        if !down {
            return None;
        }
        let which = find()?;
        self.down.push((which, key));
        Some(which)
    }
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
                "alt" | "option" | "opt" => binding.alt = true,
                "shift" => binding.shift = true,
                "win" | "super" | "meta" | "cmd" | "command" => binding.win = true,
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

/// Keys are held as Windows virtual-key codes on every platform, because that is what
/// the config strings have always parsed to; the other platforms translate at the edge.
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

/// What the platform listener sends to the state machine: which binding matched, and
/// whether this was the press or the release.
pub(crate) struct RawKey {
    /// Index into the bindings.
    which: usize,
    down: bool,
}

/// Starts listening and never returns.
///
/// Every platform's listener wants a thread of its own for the lifetime of the process
/// -- a message pump on Windows, a run loop on macOS, blocking reads on Linux.
pub fn run(
    bindings: Bindings,
    mode: Arc<Mutex<Mode>>,
    tap_threshold: Duration,
    events: Sender<Event>,
) {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<RawKey>();

    let for_state_machine = bindings.clone();
    std::thread::spawn(move || {
        state_machine(for_state_machine, mode, tap_threshold, raw_rx, events);
    });

    platform::listen(bindings, raw_tx);
}

/// One binding, three possible behaviours; see `Mode`.
/// `mode` is shared rather than copied: changing the press style in settings has to
/// take effect without restarting the app, or it reads as the setting doing nothing.
fn state_machine(
    bindings: Bindings,
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
        let Some(bound) = bindings.read().get(event.which).cloned() else {
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

/// What this platform calls the Windows/Command/Super key when writing a binding out.
pub fn super_label() -> &'static str {
    platform::SUPER_LABEL
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
        parts.push(platform::SUPER_LABEL.to_string());
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
            state_machine(
                Bindings::new(bound()),
                Arc::new(Mutex::new(mode)),
                THRESHOLD,
                raw_rx,
                ev_tx,
            );
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

    /// Drives a matcher the way a platform listener does: `held` stands in for the
    /// modifier check at the moment of each edge.
    fn edge(m: &mut Matcher<u16>, key: u16, down: bool, held: bool) -> Option<usize> {
        m.resolve(key, down, || if held && key == 0x20 { Some(0) } else { None })
    }

    /// The release-order bug: Ctrl lifted a moment before Space. The modifier check
    /// fails on the Space release, but it is still the end of the same press.
    #[test]
    fn a_release_counts_even_when_the_modifiers_lifted_first() {
        let mut m = Matcher::new();
        assert_eq!(edge(&mut m, 0x20, true, true), Some(0));
        assert_eq!(edge(&mut m, 0x20, false, false), Some(0));
        // And the next press is a fresh press, not a repeat.
        assert_eq!(edge(&mut m, 0x20, true, true), Some(0));
        assert_eq!(edge(&mut m, 0x20, false, true), Some(0));
    }

    #[test]
    fn a_bare_release_or_an_unbound_key_is_nobody_s() {
        let mut m = Matcher::new();
        assert_eq!(edge(&mut m, 0x20, false, true), None, "release with no press");
        assert_eq!(edge(&mut m, 0x41, true, true), None, "a key no binding uses");
        assert_eq!(edge(&mut m, 0x20, true, false), None, "Space without Ctrl");
        assert_eq!(edge(&mut m, 0x20, false, false), None);
    }

    /// Key repeat arrives as more key-downs; each answers for the press it belongs to,
    /// even if a modifier has changed underneath it.
    #[test]
    fn repeats_belong_to_the_press_that_started_them() {
        let mut m = Matcher::new();
        assert_eq!(edge(&mut m, 0x20, true, true), Some(0));
        assert_eq!(edge(&mut m, 0x20, true, false), Some(0));
        assert_eq!(edge(&mut m, 0x20, false, false), Some(0));
    }
}
