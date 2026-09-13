// A virtual keyboard through uinput, used only to press Ctrl+V. It sends key codes,
// not characters, so there is no typed path on Linux: the text always goes through the
// clipboard. Needs write access to /dev/uinput, which the docs cover with a udev rule.

use anyhow::{Context as _, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

pub(super) const CAN_TYPE: bool = false;

pub(super) fn modifiers_up() -> bool {
    crate::hotkey::linux_modifiers_up()
}

pub(super) fn type_unicode(_text: &str) -> Result<()> {
    anyhow::bail!("typing text directly is not available on Linux; use the clipboard path")
}

/// One virtual keyboard for the life of the process. A freshly created device takes
/// the compositor a moment to notice; creating it once and keeping it means only the
/// first paste pays that wait.
fn keyboard() -> Result<std::sync::MutexGuard<'static, VirtualDevice>> {
    static KEYBOARD: OnceLock<Mutex<VirtualDevice>> = OnceLock::new();
    if KEYBOARD.get().is_none() {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_LEFTCTRL);
        keys.insert(KeyCode::KEY_V);
        let device = VirtualDevice::builder()
            .context("opening /dev/uinput (is the udev rule from the docs in place?)")?
            .name("Lathe paste")
            .with_keys(&keys)
            .context("declaring the virtual keyboard's keys")?
            .build()
            .context("creating the virtual keyboard")?;
        // libinput enumerates new devices asynchronously; a chord sent before it has
        // finished is dropped on the floor.
        thread::sleep(Duration::from_millis(300));
        let _ = KEYBOARD.set(Mutex::new(device));
    }
    Ok(KEYBOARD
        .get()
        .expect("set above")
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

fn key(code: KeyCode, down: bool) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, down as i32)
}

pub(super) fn send_paste_chord() -> Result<()> {
    let mut keyboard = keyboard()?;
    // Each key gets its own report, the way a physical keyboard sends them; some
    // toolkits ignore a modifier and a key that arrive in the same one.
    for event in [
        key(KeyCode::KEY_LEFTCTRL, true),
        key(KeyCode::KEY_V, true),
        key(KeyCode::KEY_V, false),
        key(KeyCode::KEY_LEFTCTRL, false),
    ] {
        keyboard.emit(&[event]).context("sending the paste chord")?;
    }
    Ok(())
}
