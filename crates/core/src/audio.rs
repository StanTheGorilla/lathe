// Capture from the default input device and hand back 16kHz mono f32, which is the
// only shape whisper.cpp accepts. Brief section 5.1.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use std::sync::mpsc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const TARGET_RATE: u32 = 16_000;

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();

    let default_in = host
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_string());

    println!("input devices:");
    for device in host.input_devices()? {
        let desc = match device.description() {
            Ok(d) => d,
            Err(e) => {
                println!("  (undescribable device: {e})");
                continue;
            }
        };
        let marker = if Some(desc.name()) == default_in.as_deref() {
            " [default]"
        } else {
            ""
        };
        match device.default_input_config() {
            Ok(cfg) => println!(
                "  {}{} -- {} Hz, {} ch, {:?}",
                desc.name(),
                marker,
                cfg.sample_rate(),
                cfg.channels(),
                cfg.sample_format()
            ),
            Err(e) => println!("  {}{} -- no default config: {e}", desc.name(), marker),
        }
    }

    println!("output devices:");
    for device in host.output_devices()? {
        if let Ok(desc) = device.description() {
            println!("  {}", desc.name());
        }
    }

    Ok(())
}

/// Records until Enter is pressed, or until `max_secs` elapses, whichever comes first.
/// The daemon replaces this with the tap/hold hotkey from amendment A1; here it only
/// needs to bound the clip.
pub fn record(max_secs: u64) -> Result<Vec<f32>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;

    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let supported = device.default_input_config()?;
    let src_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    eprintln!("input: {name} -- {src_rate} Hz, {channels} ch, {format:?}");

    // Chunks leave the audio callback through a channel. The callback must not lock a
    // shared accumulator or grow a multi-second Vec: reallocating that Vec memcpies
    // megabytes while holding the lock, which overruns the WASAPI callback deadline and
    // makes the driver drop buffers. Sending a small per-callback chunk keeps the
    // callback's work proportional to the buffer it was handed.
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
    let xruns = Arc::new(AtomicUsize::new(0));
    let err_xruns = Arc::clone(&xruns);
    let err_fn = move |e: cpal::Error| {
        err_xruns.fetch_add(1, Ordering::Relaxed);
        eprintln!("stream error: {e}");
    };

    let stream: Stream = match format {
        SampleFormat::I8 => build_stream::<i8>(&device, &config, audio_tx, err_fn)?,
        SampleFormat::I16 => build_stream::<i16>(&device, &config, audio_tx, err_fn)?,
        SampleFormat::I32 => build_stream::<i32>(&device, &config, audio_tx, err_fn)?,
        SampleFormat::F32 => build_stream::<f32>(&device, &config, audio_tx, err_fn)?,
        other => return Err(anyhow!("unsupported input sample format {other:?}")),
    };

    stream.play()?;
    eprintln!("recording -- press Enter to stop (hard cap {max_secs}s)");

    let (stop_tx, stop_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = stop_tx.send(());
    });

    // Drain the audio channel while waiting so chunks never pile up unbounded.
    let mut interleaved: Vec<f32> = Vec::with_capacity(src_rate as usize * channels * 8);
    let deadline = std::time::Instant::now() + Duration::from_secs(max_secs);
    loop {
        while let Ok(chunk) = audio_rx.try_recv() {
            interleaved.extend_from_slice(&chunk);
        }
        match stop_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("hit the {max_secs}s cap");
            break;
        }
    }

    drop(stream);
    while let Ok(chunk) = audio_rx.try_recv() {
        interleaved.extend_from_slice(&chunk);
    }

    if interleaved.is_empty() {
        return Err(anyhow!("captured no samples"));
    }

    let mono = downmix(&interleaved, channels);
    let secs = mono.len() as f32 / src_rate as f32;

    // Brief 5.7 puts a level meter in settings. There is no settings UI yet and no
    // window is allowed during capture, so report the level after the fact: a mic that
    // is too quiet is the most likely reason for a disappointing transcript.
    let (peak, rms) = levels(&mono);
    eprintln!(
        "captured {:.2}s -- peak {:.1} dBFS, rms {:.1} dBFS{}",
        secs,
        peak,
        rms,
        level_hint(peak)
    );
    let dropped = xruns.load(Ordering::Relaxed);
    if dropped > 0 {
        eprintln!("warning: {dropped} stream error(s); audio may have gaps");
    }

    resample(mono, src_rate, TARGET_RATE)
}

fn levels(pcm: &[f32]) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    for s in pcm {
        peak = peak.max(s.abs());
        sum += (*s as f64) * (*s as f64);
    }
    let rms = (sum / pcm.len().max(1) as f64).sqrt() as f32;
    let db = |v: f32| if v <= 0.0 { -120.0 } else { 20.0 * v.log10() };
    (db(peak), db(rms))
}

fn level_hint(peak_db: f32) -> &'static str {
    if peak_db < -40.0 {
        "  (very quiet -- check the mic is selected and unmuted)"
    } else if peak_db < -24.0 {
        "  (quiet -- consider raising input gain)"
    } else if peak_db > -1.0 {
        "  (clipping)"
    } else {
        ""
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    tx: mpsc::Sender<Vec<f32>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config.clone(),
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // One allocation sized to the buffer we were handed, then a move across the
            // channel. No lock, and no growth of a long-lived buffer.
            let mut chunk = Vec::with_capacity(data.len());
            chunk.extend(data.iter().map(|s| s.to_sample::<f32>()));
            let _ = tx.send(chunk);
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// WASAPI applies AUTOCONVERTPCM to render streams only, so a capture stream always
/// arrives at the device's native rate and we resample here.
fn resample(input: Vec<f32>, from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to {
        return Ok(input);
    }

    use rubato::audioadapter::Adapter;
    use rubato::audioadapter_buffers::direct::InterleavedSlice;
    use rubato::{Fft, FixedSync, Resampler};

    let frames = input.len();
    let mut resampler = Fft::<f32>::new(from as usize, to as usize, 1024, 1, FixedSync::Both)?;
    let adapter = InterleavedSlice::new(&input, 1, frames)?;
    let out = resampler.process_all(&adapter, frames, None)?;

    let n = out.frames();
    let mut mono = Vec::with_capacity(n);
    for i in 0..n {
        mono.push(out.read_sample(0, i).unwrap_or(0.0));
    }
    Ok(mono)
}

pub fn write_wav(path: &std::path::Path, pcm: &[f32], rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for s in pcm {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;
    Ok(())
}

pub fn read_wav(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };

    let mono = downmix(&raw, spec.channels as usize);
    resample(mono, spec.sample_rate, TARGET_RATE)
}

/// Start/stop capture for the daemon, where recording is bounded by the hotkey rather
/// than by a blocking read.
///
/// `cpal::Stream` is not `Send`, so a `Recorder` must be created and finished on the
/// same thread. The daemon keeps all capture on its worker thread.
pub struct Recorder {
    _stream: Stream,
    pub(crate) rx: mpsc::Receiver<Vec<f32>>,
    src_rate: u32,
    channels: usize,
    xruns: Arc<AtomicUsize>,
    started: std::time::Instant,
    device_name: String,
}

pub struct Recording {
    pub pcm: Vec<f32>,
    pub peak_db: f32,
    pub rms_db: f32,
    pub xruns: usize,
    pub device: String,
}

fn pick_input(host: &cpal::Host, hint: &str) -> Result<cpal::Device> {
    if !hint.is_empty() {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if let Ok(desc) = device.description() {
                    if desc.name().to_lowercase().contains(&hint.to_lowercase()) {
                        return Ok(device);
                    }
                }
            }
        }
        eprintln!("input device '{hint}' not found, using the system default");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))
}

impl Recorder {
    pub fn start(device_hint: &str) -> Result<Self> {
        let host = cpal::default_host();
        let device = pick_input(&host, device_hint)?;
        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let supported = device.default_input_config()?;
        let src_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();

        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let xruns = Arc::new(AtomicUsize::new(0));
        let err_xruns = Arc::clone(&xruns);
        let err_fn = move |e: cpal::Error| {
            err_xruns.fetch_add(1, Ordering::Relaxed);
            eprintln!("stream error: {e}");
        };

        let stream = match format {
            SampleFormat::I8 => build_stream::<i8>(&device, &config, tx, err_fn)?,
            SampleFormat::I16 => build_stream::<i16>(&device, &config, tx, err_fn)?,
            SampleFormat::I32 => build_stream::<i32>(&device, &config, tx, err_fn)?,
            SampleFormat::F32 => build_stream::<f32>(&device, &config, tx, err_fn)?,
            other => return Err(anyhow!("unsupported input sample format {other:?}")),
        };
        stream.play()?;

        Ok(Self {
            _stream: stream,
            rx,
            src_rate,
            channels,
            xruns,
            started: std::time::Instant::now(),
            device_name,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stops capture and returns 16kHz mono f32.
    pub fn finish(self, gain: f32) -> Result<Recording> {
        let mut interleaved: Vec<f32> = Vec::new();
        drop(self._stream);
        while let Ok(chunk) = self.rx.try_recv() {
            interleaved.extend_from_slice(&chunk);
        }
        if interleaved.is_empty() {
            return Err(anyhow!("captured no samples"));
        }

        let mut mono = downmix(&interleaved, self.channels);
        if (gain - 1.0).abs() > f32::EPSILON {
            for sample in &mut mono {
                *sample = (*sample * gain).clamp(-1.0, 1.0);
            }
        }

        let (peak_db, rms_db) = levels(&mono);
        let pcm = resample(mono, self.src_rate, TARGET_RATE)?;

        Ok(Recording {
            pcm,
            peak_db,
            rms_db,
            xruns: self.xruns.load(Ordering::Relaxed),
            device: self.device_name,
        })
    }
}

/// Live input level for the meter in brief 5.7.
///
/// This is the only real-time visual the brief allows, and it exists only in the
/// settings window. It runs on its own thread that owns the stream, because
/// `cpal::Stream` is not `Send`, and it holds the microphone open only while running.
pub struct LevelMeter {
    stop: Arc<AtomicBool>,
    level: Arc<Mutex<(f32, f32)>>,
}

impl LevelMeter {
    pub fn start(device_hint: &str) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(Mutex::new((-120.0f32, -120.0f32)));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        let hint = device_hint.to_string();
        let thread_stop = Arc::clone(&stop);
        let thread_level = Arc::clone(&level);

        std::thread::spawn(move || {
            let recorder = match Recorder::start(&hint) {
                Ok(r) => {
                    let _ = ready_tx.send(Ok(()));
                    r
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };

            // Drain continuously and keep only the level, never the audio itself.
            while !thread_stop.load(Ordering::Relaxed) {
                let mut chunk_peak = 0.0f32;
                let mut sum = 0.0f64;
                let mut count = 0usize;
                while let Ok(chunk) = recorder.rx.try_recv() {
                    for s in &chunk {
                        chunk_peak = chunk_peak.max(s.abs());
                        sum += (*s as f64) * (*s as f64);
                    }
                    count += chunk.len();
                }
                if count > 0 {
                    let rms = (sum / count as f64).sqrt() as f32;
                    let db = |v: f32| if v <= 0.0 { -120.0 } else { 20.0 * v.log10() };
                    *thread_level.lock().unwrap() = (db(chunk_peak), db(rms));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("timed out opening the input device"))??;

        Ok(Self { stop, level })
    }

    /// (peak dBFS, rms dBFS)
    pub fn level(&self) -> (f32, f32) {
        *self.level.lock().unwrap()
    }
}

impl Drop for LevelMeter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Structured device information for the settings window.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
}

fn describe(device: &cpal::Device, input: bool, default_name: Option<&str>) -> Option<DeviceInfo> {
    let desc = device.description().ok()?;
    let config = if input {
        device.default_input_config().ok()
    } else {
        device.default_output_config().ok()
    };
    Some(DeviceInfo {
        is_default: default_name == Some(desc.name()),
        name: desc.name().to_string(),
        sample_rate: config.as_ref().map(|c| c.sample_rate()).unwrap_or(0),
        channels: config.as_ref().map(|c| c.channels()).unwrap_or(0),
    })
}

pub fn input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default = host
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_string());
    Ok(host
        .input_devices()?
        .filter_map(|d| describe(&d, true, default.as_deref()))
        .collect())
}

pub fn output_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_string());
    Ok(host
        .output_devices()?
        .filter_map(|d| describe(&d, false, default.as_deref()))
        .collect())
}

/// Decodes an in-memory wav to 16kHz mono f32. Used for the bundled benchmark clip.
pub fn decode_wav_bytes(bytes: &[u8]) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono = downmix(&raw, spec.channels as usize);
    resample(mono, spec.sample_rate, TARGET_RATE)
}
