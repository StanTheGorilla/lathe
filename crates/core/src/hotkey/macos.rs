// The macOS listener: a CGEvent tap on the login session.
//
// An active tap needs the Accessibility permission (System Settings > Privacy &
// Security > Accessibility). Without it `CGEventTapCreate` returns null and Lathe hears
// nothing; the prompt is requested once at startup so the user knows why.
//
// The tap callback belongs to the window server. It must classify and hand off and
// nothing more: a callback that takes too long gets the tap disabled with a
// `TapDisabledByTimeout` event, which is why that event re-enables it below.

use super::{Binding, Bound, RawKey};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::mach_port::CFMachPortRef;
use core_graphics::event::{
    CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    EventField,
};
use core_graphics::sys::CGEventRef;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

/// How the Command key is written back to the user.
pub(super) const SUPER_LABEL: &str = "Cmd";

// core-graphics wraps the tap, but its wrapper cannot delete an event: returning
// `None` from its callback passes the original through. Swallowing the hotkey means
// returning null from the real callback, so the tap is created directly.
type TapCallback = unsafe extern "C" fn(
    proxy: *const c_void,
    kind: CGEventType,
    event: CGEventRef,
    user_info: *const c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOptions,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *const c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

static HOOK_TX: OnceLock<Sender<RawKey>> = OnceLock::new();
/// Each binding with its key translated to the macOS virtual key code; `None` for a
/// key macOS has no code for, which then simply never matches.
static BINDINGS: OnceLock<Vec<(Option<u16>, Bound)>> = OnceLock::new();
/// The tap's mach port, kept so a disabled tap can be switched back on.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Windows virtual-key code to macOS virtual key code (HIToolbox Events.h). The
/// letters follow the ANSI layout; macOS codes are positional, not alphabetical.
pub(crate) fn native_key(vk: u16) -> Option<u16> {
    Some(match vk {
        0x20 => 0x31, // Space
        0x0D => 0x24, // Return
        0x09 => 0x30, // Tab
        0x1B => 0x35, // Escape
        0x08 => 0x33, // Delete (backspace)
        0x2D => 0x72, // Help, the Insert position on an extended keyboard
        0x2E => 0x75, // Forward delete
        0x24 => 0x73, // Home
        0x23 => 0x77, // End
        0x21 => 0x74, // Page up
        0x22 => 0x79, // Page down
        0x25 => 0x7B, // Left
        0x26 => 0x7E, // Up
        0x27 => 0x7C, // Right
        0x28 => 0x7D, // Down
        0x14 => 0x39, // Caps lock
        0x70 => 0x7A, // F1
        0x71 => 0x78, // F2
        0x72 => 0x63, // F3
        0x73 => 0x76, // F4
        0x74 => 0x60, // F5
        0x75 => 0x61, // F6
        0x76 => 0x62, // F7
        0x77 => 0x64, // F8
        0x78 => 0x65, // F9
        0x79 => 0x6D, // F10
        0x7A => 0x67, // F11
        0x7B => 0x6F, // F12
        0x7C => 0x69, // F13
        0x7D => 0x6B, // F14
        0x7E => 0x71, // F15
        0x7F => 0x6A, // F16
        0x80 => 0x40, // F17
        0x81 => 0x4F, // F18
        0x82 => 0x50, // F19
        0x83 => 0x5A, // F20
        0x30 => 0x1D, // 0
        0x31 => 0x12, // 1
        0x32 => 0x13, // 2
        0x33 => 0x14, // 3
        0x34 => 0x15, // 4
        0x35 => 0x17, // 5
        0x36 => 0x16, // 6
        0x37 => 0x1A, // 7
        0x38 => 0x1C, // 8
        0x39 => 0x19, // 9
        0x41 => 0x00, // A
        0x42 => 0x0B, // B
        0x43 => 0x08, // C
        0x44 => 0x02, // D
        0x45 => 0x0E, // E
        0x46 => 0x03, // F
        0x47 => 0x05, // G
        0x48 => 0x04, // H
        0x49 => 0x22, // I
        0x4A => 0x26, // J
        0x4B => 0x28, // K
        0x4C => 0x25, // L
        0x4D => 0x2E, // M
        0x4E => 0x2D, // N
        0x4F => 0x1F, // O
        0x50 => 0x23, // P
        0x51 => 0x0C, // Q
        0x52 => 0x0F, // R
        0x53 => 0x01, // S
        0x54 => 0x11, // T
        0x55 => 0x20, // U
        0x56 => 0x09, // V
        0x57 => 0x0D, // W
        0x58 => 0x07, // X
        0x59 => 0x10, // Y
        0x5A => 0x06, // Z
        _ => return None,
    })
}

fn modifiers_match(flags: u64, binding: &Binding) -> bool {
    let held = |flag: CGEventFlags| flags & flag.bits() != 0;
    held(CGEventFlags::CGEventFlagControl) == binding.ctrl
        && held(CGEventFlags::CGEventFlagAlternate) == binding.alt
        && held(CGEventFlags::CGEventFlagShift) == binding.shift
        && held(CGEventFlags::CGEventFlagCommand) == binding.win
}

unsafe extern "C" fn tap_callback(
    _proxy: *const c_void,
    kind: CGEventType,
    event: CGEventRef,
    _user_info: *const c_void,
) -> CGEventRef {
    let down = match kind {
        CGEventType::KeyDown => true,
        CGEventType::KeyUp => false,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
            let tap = TAP.load(Ordering::Acquire);
            if !tap.is_null() {
                CGEventTapEnable(tap as CFMachPortRef, true);
            }
            return event;
        }
        _ => return event,
    };

    let keycode = CGEventGetIntegerValueField(event, EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    let flags = CGEventGetFlags(event);

    if let Some(bindings) = BINDINGS.get() {
        // First match wins; the bindings arrive sorted most-modifiers-first.
        if let Some(which) = bindings
            .iter()
            .position(|(key, b)| *key == Some(keycode) && modifiers_match(flags, &b.binding))
        {
            if let Some(tx) = HOOK_TX.get() {
                let _ = tx.send(RawKey { which, down });
            }
            // Swallow it so it never reaches the focused application.
            return std::ptr::null_mut();
        }
    }
    event
}

/// Asks the system whether this process may watch the keyboard, showing the standard
/// prompt if not. The answer is only advisory: the tap creation below is what fails.
fn accessibility_trusted() -> bool {
    let options = CFDictionary::from_CFType_pairs(&[(
        CFString::new("AXTrustedCheckOptionPrompt").as_CFType(),
        CFBoolean::true_value().as_CFType(),
    )]);
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
}

/// Installs the tap and runs the run loop. Never returns.
pub(super) fn listen(bindings: Vec<Bound>, raw_tx: Sender<RawKey>) {
    let _ = HOOK_TX.set(raw_tx);
    let _ = BINDINGS.set(
        bindings
            .into_iter()
            .map(|b| (native_key(b.binding.key), b))
            .collect(),
    );

    if !accessibility_trusted() {
        eprintln!(
            "hotkey: Lathe is not trusted for Accessibility; allow it under System Settings > \
             Privacy & Security > Accessibility, then restart"
        );
    }

    let mask = (1u64 << CGEventType::KeyDown as u64) | (1u64 << CGEventType::KeyUp as u64);
    let port = unsafe {
        CGEventTapCreate(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            tap_callback,
            std::ptr::null(),
        )
    };
    if port.is_null() {
        eprintln!("failed to install the keyboard event tap; no hotkey will work");
        return;
    }
    TAP.store(port as *mut c_void, Ordering::Release);

    let port = unsafe { CFMachPort::wrap_under_create_rule(port) };
    let Ok(source) = port.create_runloop_source(0) else {
        eprintln!("failed to attach the keyboard event tap to a run loop");
        return;
    };
    unsafe {
        CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
        CGEventTapEnable(port.as_concrete_TypeRef(), true);
    }
    CFRunLoop::run_current();
}
