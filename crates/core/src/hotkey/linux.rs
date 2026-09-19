// The Linux listener: the keyboards under /dev/input, read directly.
//
// X11 has a global key grab and Wayland has nothing at all -- each compositor decides
// for itself whether a background process may hear a key, and most say no. Reading the
// input devices works under both, at the cost of a one-time permission: the user has to
// be in the `input` group (or whatever group the distribution gives /dev/input/event*).
//
// Reading is not intercepting. The hotkey still reaches the focused application, which
// is why the Linux default is a chord nothing else uses. Swallowing would mean grabbing
// every keyboard exclusively and re-emitting everything but the hotkey, and a bug in
// that path is a dead keyboard.

use super::{Binding, Bindings, Matcher, RawKey};
use evdev::{Device, EventSummary, KeyCode};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

/// How the Super key is written back to the user.
pub(super) const SUPER_LABEL: &str = "Super";

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const SUPER: u8 = 8;

/// Modifiers currently held across every keyboard, as seen by this listener. Shared
/// with the paste path, which must not type while the hotkey's modifiers are down.
static HELD: AtomicU8 = AtomicU8::new(0);

/// True when no modifier is held, according to the events read so far. Before the
/// listener has started, or when it could not open any device, this is true.
pub(crate) fn modifiers_up() -> bool {
    HELD.load(Ordering::Acquire) == 0
}

fn modifier_bit(key: KeyCode) -> Option<u8> {
    Some(match key {
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => CTRL,
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => ALT,
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => SHIFT,
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => SUPER,
        _ => return None,
    })
}

fn modifiers_match(held: u8, binding: &Binding) -> bool {
    (held & CTRL != 0) == binding.ctrl
        && (held & ALT != 0) == binding.alt
        && (held & SHIFT != 0) == binding.shift
        && (held & SUPER != 0) == binding.win
}

/// Windows virtual-key code to Linux input event code.
pub(crate) fn native_key(vk: u16) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match vk {
        0x20 => K::KEY_SPACE,
        0x0D => K::KEY_ENTER,
        0x09 => K::KEY_TAB,
        0x1B => K::KEY_ESC,
        0x08 => K::KEY_BACKSPACE,
        0x2D => K::KEY_INSERT,
        0x2E => K::KEY_DELETE,
        0x24 => K::KEY_HOME,
        0x23 => K::KEY_END,
        0x21 => K::KEY_PAGEUP,
        0x22 => K::KEY_PAGEDOWN,
        0x25 => K::KEY_LEFT,
        0x26 => K::KEY_UP,
        0x27 => K::KEY_RIGHT,
        0x28 => K::KEY_DOWN,
        0x14 => K::KEY_CAPSLOCK,
        0x70 => K::KEY_F1,
        0x71 => K::KEY_F2,
        0x72 => K::KEY_F3,
        0x73 => K::KEY_F4,
        0x74 => K::KEY_F5,
        0x75 => K::KEY_F6,
        0x76 => K::KEY_F7,
        0x77 => K::KEY_F8,
        0x78 => K::KEY_F9,
        0x79 => K::KEY_F10,
        0x7A => K::KEY_F11,
        0x7B => K::KEY_F12,
        0x7C => K::KEY_F13,
        0x7D => K::KEY_F14,
        0x7E => K::KEY_F15,
        0x7F => K::KEY_F16,
        0x80 => K::KEY_F17,
        0x81 => K::KEY_F18,
        0x82 => K::KEY_F19,
        0x83 => K::KEY_F20,
        0x84 => K::KEY_F21,
        0x85 => K::KEY_F22,
        0x86 => K::KEY_F23,
        0x87 => K::KEY_F24,
        0x30 => K::KEY_0,
        0x31 => K::KEY_1,
        0x32 => K::KEY_2,
        0x33 => K::KEY_3,
        0x34 => K::KEY_4,
        0x35 => K::KEY_5,
        0x36 => K::KEY_6,
        0x37 => K::KEY_7,
        0x38 => K::KEY_8,
        0x39 => K::KEY_9,
        0x41 => K::KEY_A,
        0x42 => K::KEY_B,
        0x43 => K::KEY_C,
        0x44 => K::KEY_D,
        0x45 => K::KEY_E,
        0x46 => K::KEY_F,
        0x47 => K::KEY_G,
        0x48 => K::KEY_H,
        0x49 => K::KEY_I,
        0x4A => K::KEY_J,
        0x4B => K::KEY_K,
        0x4C => K::KEY_L,
        0x4D => K::KEY_M,
        0x4E => K::KEY_N,
        0x4F => K::KEY_O,
        0x50 => K::KEY_P,
        0x51 => K::KEY_Q,
        0x52 => K::KEY_R,
        0x53 => K::KEY_S,
        0x54 => K::KEY_T,
        0x55 => K::KEY_U,
        0x56 => K::KEY_V,
        0x57 => K::KEY_W,
        0x58 => K::KEY_X,
        0x59 => K::KEY_Y,
        0x5A => K::KEY_Z,
        _ => return None,
    })
}

/// A device that can produce letters is a keyboard. Mice, touchpads and power buttons
/// also live under /dev/input and are skipped.
fn is_keyboard(device: &Device) -> bool {
    device
        .supported_keys()
        .map(|keys| keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_SPACE))
        .unwrap_or(false)
}

/// Whether at least one keyboard under /dev/input can be opened. `evdev::enumerate`
/// only yields devices that open, so a user outside the `input` group sees none.
pub fn keyboard_readable() -> bool {
    evdev::enumerate().any(|(_, device)| is_keyboard(&device))
}

/// Reads one keyboard until it goes away.
fn read_device(mut device: Device, bindings: Bindings, tx: Sender<RawKey>) {
    let name = device.name().unwrap_or("unnamed keyboard").to_string();
    // Per keyboard: a key goes up on the device it went down on.
    let mut matcher = Matcher::new();
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(e) => {
                // Unplugged, most likely. Nothing to do but stop reading it.
                eprintln!("hotkey: stopped reading {name}: {e}");
                return;
            }
        };
        for event in events {
            let EventSummary::Key(_, key, value) = event.destructure() else {
                continue;
            };
            // 0 released, 1 pressed, 2 autorepeat. Repeats are forwarded as presses;
            // the state machine already ignores a press of a key that is down.
            let down = value != 0;
            if let Some(bit) = modifier_bit(key) {
                if down {
                    HELD.fetch_or(bit, Ordering::AcqRel);
                } else {
                    HELD.fetch_and(!bit, Ordering::AcqRel);
                }
                continue;
            }
            let held = HELD.load(Ordering::Acquire);
            let matched = matcher.resolve(key, down, || {
                bindings.read().iter().position(|b| {
                    native_key(b.binding.key) == Some(key) && modifiers_match(held, &b.binding)
                })
            });
            if let Some(which) = matched {
                let _ = tx.send(RawKey { which, down });
            }
        }
    }
}

/// Opens every keyboard and reads them until the process ends. Returns only when no
/// keyboard is left to read.
pub(super) fn listen(bindings: Bindings, raw_tx: Sender<RawKey>) {
    let mut readers = Vec::new();
    for (path, device) in evdev::enumerate() {
        if !is_keyboard(&device) {
            continue;
        }
        eprintln!(
            "hotkey: reading {} ({})",
            device.name().unwrap_or("unnamed keyboard"),
            path.display()
        );
        let bindings = bindings.clone();
        let tx = raw_tx.clone();
        readers.push(std::thread::spawn(move || read_device(device, bindings, tx)));
    }

    if readers.is_empty() {
        eprintln!(
            "hotkey: no readable keyboard under /dev/input; add your user to the input \
             group (sudo usermod -aG input $USER) and log in again"
        );
        return;
    }
    for reader in readers {
        let _ = reader.join();
    }
}
