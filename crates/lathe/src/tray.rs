// Tray icon, brief section 8.
//
// This is the primary status channel. With no window and no overlay allowed on the hot
// path, the icon plus the audio cues are the entire feedback surface, so the five
// states have to be distinguishable at 16px.
//
// Icons are drawn here rather than shipped as files: they are five circles, and a
// generator keeps them consistent with the palette in one place.

// Returns raw RGBA; the caller wraps it in whatever image type its toolkit wants.

const SIZE: u32 = 32;

// Brief section 8 palette.
const INK: [u8; 3] = [0x14, 0x14, 0x13];
const PAPER: [u8; 3] = [0xfa, 0xf9, 0xf5];
const ASH: [u8; 3] = [0xb0, 0xae, 0xa5];
const CLAY: [u8; 3] = [0xd9, 0x77, 0x57];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Loading,
    Recording,
    Processing,
    Error,
}

impl State {
    /// The tooltip names the dictate binding in every idle state, because the tray is
    /// the only surface always in front of the user and the binding is the one thing
    /// they must know to use the app at all.
    pub fn tooltip(self, hotkey: &str) -> String {
        match self {
            State::Idle => format!("Lathe -- ready. {hotkey} to dictate"),
            State::Loading => "Lathe -- loading models".to_string(),
            State::Recording => "Lathe -- recording".to_string(),
            State::Processing => "Lathe -- processing".to_string(),
            State::Error => "Lathe -- error, see the last notification".to_string(),
        }
    }
}

/// Distance from a pixel centre to a point, in pixels.
fn distance(x: u32, y: u32, cx: f32, cy: f32) -> f32 {
    let dx = x as f32 - cx;
    let dy = y as f32 - cy;
    (dx * dx + dy * dy).sqrt()
}

/// Antialiased coverage of a disc of `radius` centred on the icon.
fn disc(x: u32, y: u32, radius: f32) -> f32 {
    let c = SIZE as f32 / 2.0 - 0.5;
    (0.5 - (distance(x, y, c, c) - radius)).clamp(0.0, 1.0)
}

pub const ICON_SIZE: u32 = SIZE;

pub fn icon_rgba(state: State) -> Vec<u8> {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let outer = SIZE as f32 / 2.0 - 2.0;
    let stroke = 3.0f32;

    let colour = match state {
        State::Idle | State::Loading | State::Error => INK,
        State::Recording => CLAY,
        State::Processing => ASH,
    };
    let filled = matches!(state, State::Recording | State::Processing | State::Error);

    for y in 0..SIZE {
        for x in 0..SIZE {
            // Filled states are a solid disc; outline states are a ring.
            let mut alpha = if filled {
                disc(x, y, outer)
            } else {
                disc(x, y, outer) - disc(x, y, outer - stroke)
            };

            // Loading is the outline plus a centre dot, per brief section 8.
            if state == State::Loading {
                alpha = alpha.max(disc(x, y, 3.5));
            }

            // Error is a filled mark with a notch bitten out of the upper right, so its
            // silhouette differs from the other filled states at 16px.
            if state == State::Error {
                let notch = distance(x, y, SIZE as f32 * 0.78, SIZE as f32 * 0.22);
                alpha *= (notch - SIZE as f32 * 0.24).clamp(0.0, 1.0);
            }

            if alpha > 0.0 {
                let i = ((y * SIZE + x) * 4) as usize;
                rgba[i] = colour[0];
                rgba[i + 1] = colour[1];
                rgba[i + 2] = colour[2];
                rgba[i + 3] = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }

    // The two solid status states get a paper core, so the mark still reads as a ring
    // rather than a blob and stays legible on a light or dark taskbar.
    if matches!(state, State::Recording | State::Processing) {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let core = disc(x, y, outer - stroke * 1.6);
                if core > 0.0 {
                    let i = ((y * SIZE + x) * 4) as usize;
                    for c in 0..3 {
                        rgba[i + c] = ((PAPER[c] as f32) * core
                            + (rgba[i + c] as f32) * (1.0 - core))
                            as u8;
                    }
                }
            }
        }
    }

    rgba
}
