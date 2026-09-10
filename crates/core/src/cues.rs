// Audio cues, brief section 5.2. Synthesized at runtime with a raised-cosine envelope
// so there is no click. No bundled wav files.
//
// The error cue is load-bearing: with no UI on the hot path, it is the only way the
// user learns that something broke.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Rising two-tone blip, recording started.
    Start,
    /// Falling two-tone blip, recording stopped and processing began.
    Stop,
    /// Single short tick, text pasted. Off by default.
    Tick,
    /// Low double-tone. The only failure channel the user has.
    Error,
}

/// A note that is struck rather than beeped: when it starts, how high, how long, and how
/// fast it dies away.
struct Note {
    /// Seconds from the start of the cue. Notes overlap deliberately -- the second is
    /// struck while the first is still ringing, so the pair reads as one sound.
    at: f32,
    freq: f32,
    secs: f32,
    /// Time constant of the fundamental's decay, in seconds.
    decay: f32,
}

/// Partials, as (frequency multiple, amplitude, how much faster than the fundamental it
/// dies away).
///
/// A bare sine with a soft attack is what a microwave beep is made of, and it reads as
/// cheap however cleanly it is rendered. What makes a sound feel struck is a stack of
/// partials over the fundamental, each dying away faster than it does -- the high ones
/// almost immediately, which is what the ear reads as the strike itself.
///
/// Whole-number ratios, and nothing above the fourth harmonic.
///
/// An earlier version used inharmonic ratios -- 3.01, 4.16, 5.43, 9.11, 12.40 -- because
/// that is what real struck bars and bells do. It was reported as having a sharp top that
/// could be heard as its own separate note, and that is exactly the consequence:
/// non-integer partials do not fuse, so the ear resolves them individually instead of
/// hearing one timbre. Whole-number partials merge into a single perceived pitch.
///
/// The ceiling matters as much as the ratios. The old stack reached 3kHz, which is where
/// human hearing is most sensitive, so its top partial cut through even at an amplitude
/// of 0.015. This one stops at the fourth harmonic -- 1.3kHz over the top note -- and
/// rolls off steeply, which leaves nothing in the range that reads as sharp.
const PARTIALS: [(f32, f32, f32); 4] = [
    (1.0, 1.00, 1.0),
    (2.0, 0.30, 1.8),
    (3.0, 0.10, 2.8),
    (4.0, 0.03, 4.0),
];

/// Start and stop, as struck notes. Mirror images of each other: A3 up to E4, and back.
///
/// Two notes 55ms apart, ringing for 225ms in total.
///
/// The pitch came down twice on report -- 660/990Hz read as a beep, then 294/440Hz was
/// still too high. The spacing is what carries the tempo: two strikes 27ms apart read as
/// one hurried event, where 55ms reads as a deliberate gesture.
///
/// Still bounded, for one reason that has not changed: the microphone is already live
/// when the start cue plays, so every millisecond of ring is a millisecond of sound the
/// recogniser gets to hear.
fn notes(cue: Cue) -> &'static [Note] {
    match cue {
        Cue::Start => &[
            Note { at: 0.0, freq: 220.00, secs: 0.090, decay: 0.065 },
            Note { at: 0.055, freq: 329.63, secs: 0.170, decay: 0.105 },
        ],
        Cue::Stop => &[
            Note { at: 0.0, freq: 329.63, secs: 0.090, decay: 0.065 },
            Note { at: 0.055, freq: 220.00, secs: 0.170, decay: 0.105 },
        ],
        _ => &[],
    }
}

/// (frequency Hz, duration seconds) pairs, played back to back. A zero frequency is a
/// silent gap, which is what separates the error cue's pulses into a distinct rhythm.
///
/// Only the error and tick cues are built this way now. The error cue was originally two
/// low tones the same length as the stop cue, and in use it was not reliably
/// distinguishable from it -- reported as "I don't know if those are different, really".
/// It is much lower, more than three times longer, and deliberately *rhythmic*: three
/// separated pulses read as an alarm rather than a blip, and rhythm survives small
/// speakers and low volume better than pitch does. It is left exactly as tuned.
fn tones(cue: Cue) -> &'static [(f32, f32)] {
    match cue {
        Cue::Tick => &[(880.0, 0.025)],
        Cue::Error => &[
            (300.0, 0.090),
            (0.0, 0.055),
            (300.0, 0.090),
            (0.0, 0.055),
            (196.0, 0.170),
        ],
        // Start and stop are struck, not beeped; see `notes`.
        _ => &[],
    }
}

/// Renders a cue to mono f32 at `rate`.
///
/// Two shapes, because the cues do two different jobs. Start and stop are struck notes
/// that ring and decay. Error and tick are plain tones, each under a raised-cosine (Hann)
/// envelope over its whole duration, which removes the discontinuity at both ends -- that
/// discontinuity is the click.
pub fn render(cue: Cue, rate: u32, volume: f32) -> Vec<f32> {
    let volume = volume.clamp(0.0, 1.0);
    let notes = notes(cue);
    if !notes.is_empty() {
        return render_struck(notes, rate, volume);
    }

    let mut out = Vec::new();
    let mut phase = 0.0f32;

    for (freq, secs) in tones(cue) {
        let samples = (secs * rate as f32) as usize;

        // A zero frequency is a gap, not a tone.
        if *freq <= 0.0 {
            out.extend(std::iter::repeat_n(0.0, samples));
            continue;
        }

        let step = std::f32::consts::TAU * freq / rate as f32;
        for i in 0..samples {
            let t = i as f32 / samples as f32;
            let envelope = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
            out.push(phase.sin() * envelope * volume);
            phase += step;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
    }
    out
}

/// Overlapping struck notes, summed and then normalised so the loudest sample is exactly
/// `volume`.
///
/// Normalising rather than scaling each partial matters: the partials sum, so a note
/// built this way peaks well above 1.0 before it is brought back down. Without the
/// normalisation the cue's loudness would drift every time a partial was retuned, and the
/// volume setting would stop meaning anything.
fn render_struck(notes: &[Note], rate: u32, volume: f32) -> Vec<f32> {
    /// How long the onset takes to reach full amplitude, and the single biggest control
    /// over whether the cue feels soft or hard.
    ///
    /// 8ms erased the transient entirely and read as mushy; 1.5ms is faster than the ear
    /// can resolve, which puts the whole partial stack on in one step and reads as a hard
    /// edge. 6ms keeps a definite onset with the edge taken off it.
    const ATTACK: f32 = 0.0060;
    /// Forced fade at the very end. The exponential decay is close to silence by then but
    /// never reaches it, and "close to silence" is still a click.
    const TAIL: f32 = 0.006;

    // Each note's span in samples. Derived once and reused for both the buffer length and
    // the write offsets: computing the total from `(at + secs) * rate` instead rounds down
    // below `at * rate + secs * rate`, and the last note then writes one past the end.
    let spans: Vec<(usize, usize)> = notes
        .iter()
        .map(|n| {
            (
                (n.at * rate as f32) as usize,
                (n.secs * rate as f32) as usize,
            )
        })
        .collect();

    let total = spans.iter().map(|(at, len)| at + len).max().unwrap_or(0);
    let mut out = vec![0.0f32; total];

    for (note, (offset, samples)) in notes.iter().zip(&spans) {
        let (offset, samples) = (*offset, *samples);
        let attack = (ATTACK * rate as f32).max(1.0);
        let tail = (TAIL * rate as f32).max(1.0);

        // One phase per partial: they run at different frequencies, so they cannot share.
        let mut phases = [0.0f32; PARTIALS.len()];
        for i in 0..samples {
            let t = i as f32 / rate as f32;
            let mut sample = 0.0;
            for (p, (mult, amp, fade)) in PARTIALS.iter().enumerate() {
                sample += phases[p].sin() * amp * (-t / (note.decay / fade)).exp();
                phases[p] += std::f32::consts::TAU * note.freq * mult / rate as f32;
                if phases[p] > std::f32::consts::TAU {
                    phases[p] -= std::f32::consts::TAU;
                }
            }

            if (i as f32) < attack {
                sample *= 0.5 - 0.5 * (std::f32::consts::PI * (i as f32 / attack)).cos();
            }
            // Distance to the *last* index, not to one past it, so the final sample lands
            // on exactly zero rather than merely near it. Near-silence is still a click.
            let left = (samples - 1 - i) as f32;
            if left < tail {
                sample *= 0.5 - 0.5 * (std::f32::consts::PI * (left / tail)).cos();
            }
            out[offset + i] += sample;
        }
    }

    let peak = out.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    if peak > 0.0 {
        for sample in &mut out {
            *sample *= volume / peak;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Amendment A20: the error cue was once mistakable for the stop cue, and the user
    /// had no way to tell a failure from a finished dictation.
    ///
    /// This asserted only that the error cue was three times longer. That held until the
    /// stop cue was slowed down on request, and a pure length ratio was the wrong thing
    /// to pin anyway: A20's own finding was that *rhythm* is what carries the difference,
    /// because it survives low volume and small speakers where pitch does not. So both
    /// properties are checked here -- the error cue is still clearly the longer of the
    /// two, and it is still the only one broken into separate pulses.
    #[test]
    fn the_error_cue_cannot_be_mistaken_for_the_stop_cue() {
        let rate = 48_000;
        let stop = render(Cue::Stop, rate, 1.0);
        let error = render(Cue::Error, rate, 1.0);

        assert!(
            error.len() > stop.len() * 2,
            "the error cue must stay clearly the longer of the two: {} vs {} samples",
            error.len(),
            stop.len()
        );

        // A run of silence in the middle is a gap between pulses. The stop cue is one
        // continuous gesture and must not have one; the error cue is three pulses and
        // must.
        let gaps = |pcm: &[f32]| {
            let quiet = (0.02 * rate as f32) as usize;
            pcm.windows(quiet)
                .filter(|w| w.iter().all(|s| s.abs() < 1e-4))
                .count()
        };
        assert!(gaps(&error) > 0, "the error cue must keep its pulse rhythm");
        assert_eq!(gaps(&stop), 0, "the stop cue must be one continuous sound");
    }

    #[test]
    fn cues_start_and_end_at_silence() {
        // A non-zero first or last sample is a click.
        for cue in [Cue::Start, Cue::Stop, Cue::Tick, Cue::Error] {
            let rendered = render(cue, 48_000, 1.0);
            assert!(rendered.first().unwrap().abs() < 1e-6, "{cue:?} clicks on");
            assert!(rendered.last().unwrap().abs() < 1e-6, "{cue:?} clicks off");
        }
    }
}

/// Plays cues on a dedicated thread so nothing on the hot path waits for audio output.
pub struct Player {
    tx: mpsc::Sender<Cue>,
}

impl Player {
    pub fn new(device_hint: String, volume: f32) -> Self {
        let (tx, rx) = mpsc::channel::<Cue>();
        std::thread::spawn(move || {
            for cue in rx {
                if let Err(e) = play_blocking(&device_hint, cue, volume) {
                    eprintln!("cue playback failed: {e}");
                }
            }
        });
        Self { tx }
    }

    pub fn play(&self, cue: Cue) {
        let _ = self.tx.send(cue);
    }
}

/// The output stream is opened per cue rather than held open. Holding it open keeps the
/// audio device active and burns CPU while idle, which brief section 3 rules out; the
/// cost is roughly 20ms of device setup before the tone sounds.
fn play_blocking(device_hint: &str, cue: Cue, volume: f32) -> Result<()> {
    let host = cpal::default_host();
    let device = pick_output(&host, device_hint)?;
    let supported = device.default_output_config()?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    // Silence after the cue, so the decay is never truncated.
    //
    // The `done` signal below fires when the stream callback has *consumed* the last
    // sample, not when the speaker has played it: between the two sit the device's own
    // buffers, tens of milliseconds deep. Dropping the stream at that point cuts off
    // whatever is still queued, which on a sound that ends in a decay means chopping the
    // quietest, most fragile part of it. Padding with silence means the audible tail is
    // long gone by the time anything is torn down.
    const FLUSH_MS: usize = 120;

    let mut rendered = render(cue, rate, volume);
    let secs = rendered.len() as f32 / rate as f32;
    rendered.extend(std::iter::repeat_n(0.0, rate as usize * FLUSH_MS / 1000));
    let samples = Arc::new(rendered);
    let (done_tx, done_rx) = mpsc::channel();

    let stream = match format {
        SampleFormat::I8 => out_stream::<i8>(&device, &config, samples, channels, done_tx)?,
        SampleFormat::I16 => out_stream::<i16>(&device, &config, samples, channels, done_tx)?,
        SampleFormat::I32 => out_stream::<i32>(&device, &config, samples, channels, done_tx)?,
        SampleFormat::F32 => out_stream::<f32>(&device, &config, samples, channels, done_tx)?,
        other => return Err(anyhow!("unsupported output sample format {other:?}")),
    };
    stream.play()?;

    // Wait for the buffer to drain, with a generous ceiling so a misbehaving device
    // cannot wedge the cue thread.
    // `secs` is the audible part; the padding above and this ceiling both have to fit.
    let budget = Duration::from_secs_f32(secs + FLUSH_MS as f32 / 1000.0 + 0.5);
    let _ = done_rx.recv_timeout(budget);
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}

fn pick_output(host: &cpal::Host, hint: &str) -> Result<cpal::Device> {
    if !hint.is_empty() {
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if let Ok(desc) = device.description() {
                    if desc.name().to_lowercase().contains(&hint.to_lowercase()) {
                        return Ok(device);
                    }
                }
            }
        }
        eprintln!("cue output device '{hint}' not found, using the system default");
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("no default output device"))
}

fn out_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    samples: Arc<Vec<f32>>,
    channels: usize,
    done: mpsc::Sender<()>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut cursor = 0usize;
    let mut signalled = false;
    let stream = device.build_output_stream(
        config.clone(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels) {
                let value = samples.get(cursor).copied().unwrap_or(0.0);
                if cursor < samples.len() {
                    cursor += 1;
                }
                for slot in frame.iter_mut() {
                    *slot = T::from_sample(value);
                }
            }
            if cursor >= samples.len() && !signalled {
                signalled = true;
                let _ = done.send(());
            }
        },
        |e| eprintln!("cue stream error: {e}"),
        None,
    )?;
    Ok(stream)
}
