// Linux: every playing application is a sink input on PulseAudio or PipeWire, and
// `pactl` speaks to both. Shelling out costs two process spawns per dictation and saves
// a C library dependency and a hundred lines of callback plumbing.

use anyhow::{anyhow, Context as _, Result};
use std::process::Command;

pub(super) const AVAILABLE: bool = true;

/// A sink input whose volume we changed, and what it was before, one raw value per
/// channel so a balance survives the round trip.
struct Ducked {
    id: u32,
    original: Vec<u32>,
}

pub(super) struct Ducker {
    ducked: Vec<Ducked>,
}

/// One entry of `pactl list sink-inputs`.
#[derive(Debug, PartialEq, Eq)]
struct SinkInput {
    id: u32,
    pid: Option<u32>,
    volume: Vec<u32>,
}

/// Parses the output of `pactl list sink-inputs`. Tolerant of fields it does not know,
/// since the listing varies between PulseAudio and PipeWire.
fn parse_sink_inputs(listing: &str) -> Vec<SinkInput> {
    let mut inputs: Vec<SinkInput> = Vec::new();
    for line in listing.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed.strip_prefix("Sink Input #") {
            if let Ok(id) = id.trim().parse() {
                inputs.push(SinkInput {
                    id,
                    pid: None,
                    volume: Vec::new(),
                });
            }
            continue;
        }
        let Some(current) = inputs.last_mut() else {
            continue;
        };
        if let Some(rest) = trimmed.strip_prefix("Volume:") {
            // "front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB"
            current.volume = rest
                .split(',')
                .filter_map(|channel| {
                    let (_, value) = channel.split_once(':')?;
                    value.trim().split('/').next()?.trim().parse().ok()
                })
                .collect();
        } else if let Some(rest) = trimmed.strip_prefix("application.process.id = ") {
            current.pid = rest.trim().trim_matches('"').parse().ok();
        }
    }
    inputs
}

fn pactl(args: &[&str]) -> Result<String> {
    let output = Command::new("pactl")
        .args(args)
        .output()
        .context("running pactl (is pulseaudio-utils installed?)")?;
    if !output.status.success() {
        return Err(anyhow!(
            "pactl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn set_volume(id: u32, volume: &[u32]) -> Result<()> {
    let id = id.to_string();
    let mut args = vec!["set-sink-input-volume", id.as_str()];
    let rendered: Vec<String> = volume.iter().map(u32::to_string).collect();
    args.extend(rendered.iter().map(String::as_str));
    pactl(&args).map(|_| ())
}

impl Ducker {
    pub(super) fn start(level: f32) -> Result<Self> {
        let own_pid = std::process::id();
        let listing = pactl(&["list", "sink-inputs"])?;
        let mut ducked = Vec::new();
        for input in parse_sink_inputs(&listing) {
            // Never duck ourselves: the cues play through a stream on this same device,
            // and turning them down is the opposite of what this is for.
            if input.pid == Some(own_pid) || input.volume.is_empty() {
                continue;
            }
            // Scale rather than set: an application already playing quietly should not
            // get louder because we ducked it.
            let scaled: Vec<u32> = input
                .volume
                .iter()
                .map(|&v| (v as f32 * level).round() as u32)
                .collect();
            if set_volume(input.id, &scaled).is_ok() {
                ducked.push(Ducked {
                    id: input.id,
                    original: input.volume,
                });
            }
        }
        Ok(Self { ducked })
    }

    pub(super) fn count(&self) -> usize {
        self.ducked.len()
    }
}

impl Drop for Ducker {
    fn drop(&mut self) {
        for entry in &self.ducked {
            let _ = set_volume(entry.id, &entry.original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_id_pid_and_per_channel_volume() {
        let listing = "\
Sink Input #57
\tDriver: PipeWire
\tSink: 0
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 32768 / 50% / -18.06 dB
\t        balance -0.50
\tProperties:
\t\tapplication.name = \"Firefox\"
\t\tapplication.process.id = \"4242\"

Sink Input #58
\tVolume: mono: 13107 / 20% / -41.94 dB
\tProperties:
\t\tapplication.name = \"mpv\"
";
        assert_eq!(
            parse_sink_inputs(listing),
            vec![
                SinkInput {
                    id: 57,
                    pid: Some(4242),
                    volume: vec![65536, 32768],
                },
                SinkInput {
                    id: 58,
                    pid: None,
                    volume: vec![13107],
                },
            ]
        );
    }
}
