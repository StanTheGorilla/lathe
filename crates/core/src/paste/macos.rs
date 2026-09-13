// CGEvents for both paths: keyboard events carrying a Unicode string for typing, a
// Cmd+V chord for pasting. Posting events needs the same Accessibility permission the
// hotkey tap does.

use anyhow::{anyhow, Result};
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

pub(super) const CAN_TYPE: bool = true;

/// Virtual key code of V on the ANSI layout. The chord is positional, so this is the
/// physical key whatever the layout, which is how Cmd+V behaves for the user too.
const KEY_V: u16 = 0x09;

/// A keyboard event carrying a string is limited to this many UTF-16 units; anything
/// longer is silently truncated by the system.
const UNITS_PER_EVENT: usize = 20;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> u64;
}

pub(super) fn modifiers_up() -> bool {
    let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::CombinedSessionState) };
    let held = |flag: CGEventFlags| flags & flag.bits() != 0;
    !(held(CGEventFlags::CGEventFlagControl)
        || held(CGEventFlags::CGEventFlagAlternate)
        || held(CGEventFlags::CGEventFlagShift)
        || held(CGEventFlags::CGEventFlagCommand))
}

fn source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|()| anyhow!("could not create an event source"))
}

fn key_event(source: &CGEventSource, keycode: u16, down: bool) -> Result<CGEvent> {
    CGEvent::new_keyboard_event(source.clone(), keycode, down)
        .map_err(|()| anyhow!("could not create a keyboard event"))
}

/// Types the text as keyboard events carrying Unicode. No clipboard involvement.
pub(super) fn type_unicode(text: &str) -> Result<()> {
    let source = source()?;
    let units: Vec<u16> = text.encode_utf16().collect();
    for chunk in units.chunks(UNITS_PER_EVENT) {
        // Both edges carry the string; applications read it from whichever they handle.
        for down in [true, false] {
            let event = key_event(&source, 0, down)?;
            event.set_string_from_utf16_unchecked(chunk);
            event.post(CGEventTapLocation::HID);
        }
    }
    Ok(())
}

pub(super) fn send_paste_chord() -> Result<()> {
    let source = source()?;
    for down in [true, false] {
        let event = key_event(&source, KEY_V, down)?;
        event.set_flags(CGEventFlags::CGEventFlagCommand);
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}
