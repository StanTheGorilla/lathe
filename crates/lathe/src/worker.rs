// The dictation worker. Owns audio capture, the models, and the paste chain.
//
// Everything slow happens here so the event loop stays responsive and the tray keeps
// updating. Panics are caught at the stage boundary per amendment A2: with no UI on the
// hot path, a silent process death is the worst possible failure mode, and the error cue
// is the only channel the user has.

use lathe_core::audio::Recorder;
use lathe_core::config::Config;
use lathe_core::cues::{Cue, Player};
use lathe_core::engine::{Engine, Processed, Rejected};
use lathe_core::hotkey;
use lathe_core::paste;
use serde::Serialize;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::tray::State;

/// Brief 6.9: a reference clip, so the benchmark measures the same thing every time.
const REFERENCE_CLIP: &[u8] = include_bytes!("../../../assets/jfk.wav");

pub enum Msg {
    Hotkey(hotkey::Event),
    Benchmark(Sender<Result<BenchReport, String>>),
}

#[derive(Serialize)]
pub struct BenchReport {
    /// Both models together. They are not timed separately here, because splitting
    /// them would mean unloading between runs; the log reports each individually.
    pub load_ms: u128,
    pub vad_ms: u128,
    pub asr_ms: u128,
    pub cleanup_ms: u128,
    pub realtime_factor: f32,
}

pub struct Context {
    pub app: AppHandle,
    pub config: Arc<Mutex<Config>>,
    pub rx: Receiver<Msg>,
}

struct Ui {
    app: AppHandle,
    cues: Player,
}

impl Ui {
    fn state(&self, state: State) {
        if let Some(tray) = self.app.tray_by_id("lathe") {
            let _ = tray.set_icon(Some(crate::tray_image(state)));
            let hotkey = self
                .app
                .try_state::<crate::AppState>()
                .map(|s| s.hotkey_label.clone())
                .unwrap_or_default();
            let _ = tray.set_tooltip(Some(state.tooltip(&hotkey)));
        }
    }

    fn cue(&self, cue: Cue) {
        self.cues.play(cue);
    }

    /// Brief 5.2: the error cue, a tray state change and a toast, together.
    ///
    /// `title` says what class of thing went wrong, because a toast that reads only
    /// "Lathe" tells the user nothing they can act on.
    fn error(&self, title: &str, message: &str) {
        eprintln!("error: {message}");
        self.cue(Cue::Error);
        self.state(State::Error);
        crate::notify_user(title, message);
    }

    /// Nothing went wrong; there was simply nothing to do. Sounds the cue so the user
    /// knows the key registered, but does not put the tray into the error state or
    /// claim a failure.
    fn nothing_to_do(&self, title: &str, message: &str) {
        eprintln!("{message}");
        self.cue(Cue::Error);
        self.state(State::Idle);
        crate::notify_user(title, message);
    }
}

pub fn spawn(ctx: Context) {
    std::thread::spawn(move || run(ctx));
}

fn run(ctx: Context) {
    let (cue_device, cue_volume) = {
        let config = ctx.config.lock().unwrap();
        (config.audio.output_device.clone(), config.cues.volume)
    };

    let ui = Ui {
        app: ctx.app.clone(),
        cues: Player::new(cue_device, cue_volume),
    };

    let mut engine = match Engine::new() {
        Ok(e) => e,
        Err(e) => {
            ui.error("Lathe could not start the GPU backend", &format!("{e:#}"));
            return;
        }
    };

    // Brief 6.4. Opened once; a failure here disables history rather than the app.
    let history = match lathe_core::config::history_path() {
        Ok(path) => match lathe_core::history::History::open(
            &path,
            ctx.config.lock().unwrap().history.limit,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("history unavailable: {e:#}");
                None
            }
        },
        Err(e) => {
            eprintln!("history unavailable: {e:#}");
            None
        }
    };

    // Brief 5.6: the last raw transcript, for the paste-raw hotkey.
    let mut last_raw: Option<String> = None;

    loop {
        // The timeout doubles as the idle-unload poll.
        let msg = match ctx.rx.recv_timeout(Duration::from_secs(30)) {
            Ok(msg) => Some(msg),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };

        let config = ctx.config.lock().unwrap().clone();

        let Some(msg) = msg else {
            if engine.unload_if_idle(&config) {
                ui.state(State::Idle);
            }
            continue;
        };

        match msg {
            Msg::Hotkey(hotkey::Event::Start { preset }) => {
                dictate(
                    &mut engine,
                    &config,
                    &ui,
                    &ctx.rx,
                    &mut last_raw,
                    history.as_ref(),
                    preset.as_deref(),
                );
            }
            Msg::Hotkey(hotkey::Event::Stop) => {
                // A stop with no recording in flight; nothing to do.
            }
            Msg::Hotkey(hotkey::Event::PasteRaw) => match &last_raw {
                Some(raw) => {
                    if let Err(e) = deliver(&config, raw, true) {
                        ui.error("Lathe could not paste", &format!("{e:#}"));
                    } else if config.cues.tick_on_paste {
                        ui.cue(Cue::Tick);
                    }
                }
                None => ui.nothing_to_do(
                    "Nothing to paste yet",
                    "Ctrl+Shift+Space re-pastes your last dictation, and there has not \n                     been one yet. Ctrl+Alt+Space starts dictating.",
                ),
            },
            Msg::Benchmark(reply) => {
                let _ = reply.send(benchmark(&mut engine, &config, &ui));
            }
        }
    }
}

fn benchmark(engine: &mut Engine, config: &Config, ui: &Ui) -> Result<BenchReport, String> {
    ui.state(State::Processing);

    let preset = config.active().clone();
    let started = Instant::now();
    let already_loaded = engine.loaded(config.languages.current());
    engine
        .ensure_loaded(config, config.languages.current(), &|_| {})
        .map_err(|e| format!("{e:#}"))?;
    let load_ms = if already_loaded {
        0
    } else {
        started.elapsed().as_millis()
    };

    let pcm = lathe_core::audio::decode_wav_bytes(REFERENCE_CLIP).map_err(|e| format!("{e:#}"))?;
    let audio_secs = pcm.len() as f32 / lathe_core::audio::TARGET_RATE as f32;

    let result = engine
        .process(config, &preset, &pcm)
        .map_err(|e| format!("{e:#}"))?;

    ui.state(State::Idle);

    match result {
        Processed::Done(outcome) => {
            let total = outcome.asr_ms + outcome.cleanup_ms;
            Ok(BenchReport {
                load_ms,
                vad_ms: outcome.vad_ms,
                asr_ms: outcome.asr_ms,
                cleanup_ms: outcome.cleanup_ms,
                realtime_factor: if total == 0 {
                    0.0
                } else {
                    audio_secs / (total as f32 / 1000.0)
                },
            })
        }
        Processed::Rejected(_) => Err("the reference clip was rejected by speech detection".into()),
    }
}

fn dictate(
    engine: &mut Engine,
    config: &Config,
    ui: &Ui,
    rx: &Receiver<Msg>,
    last_raw: &mut Option<String>,
    history: Option<&lathe_core::history::History>,
    // Set when a preset-specific hotkey started this dictation.
    preset_override: Option<&str>,
) {
    // Start capturing before anything else. Models load while the user is already
    // talking, so a cold first dictation costs latency but never loses words.
    let recorder = match Recorder::start(&config.audio.input_device) {
        Ok(r) => r,
        Err(e) => {
            ui.error("Lathe could not open the microphone", &format!("{e:#}"));
            return;
        }
    };
    // Quieten music and video for as long as this binding lives. Dropped at the end of
    // the function, which restores every volume even if a stage below fails or panics.
    // Started after the recorder so the cue is not itself ducked on the way out.
    let ducker = if config.audio.duck_others {
        match lathe_core::ducking::Ducker::start(config.audio.duck_level) {
            Ok(d) => {
                if d.count() > 0 {
                    eprintln!("ducked {} audio session(s)", d.count());
                }
                Some(d)
            }
            Err(e) => {
                // Not worth failing a dictation over. Say so and carry on.
                eprintln!("could not duck other audio: {e:#}");
                None
            }
        }
    } else {
        None
    };

    // A preset hotkey overrides the tray selection for this dictation only. Resolved
    // before the tray state, because whether the models are loaded depends on which
    // language this dictation is in.
    let preset = preset_override
        .and_then(|name| config.preset(name))
        .unwrap_or_else(|| config.active())
        .clone();

    ui.cue(Cue::Start);
    let ready = engine.loaded(config.languages.current());
    ui.state(if ready {
        State::Recording
    } else {
        State::Loading
    });

    if !ready {
        if let Err(e) = engine.ensure_loaded(config, config.languages.current(), &|_| {}) {
            ui.error("Lathe could not load the models", &format!("{e:#}"));
            return;
        }
        ui.state(State::Recording);
    }

    // Wait for the release, or the hard cap from brief 5.1.
    let cap = Duration::from_secs(config.max_record_secs);
    loop {
        let remaining = cap.saturating_sub(recorder.elapsed());
        if remaining.is_zero() {
            eprintln!("hit the {}s recording cap", config.max_record_secs);
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(Msg::Hotkey(hotkey::Event::Stop)) => break,
            Ok(Msg::Benchmark(reply)) => {
                let _ = reply.send(Err("a dictation is in progress".into()));
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    ui.cue(Cue::Stop);
    ui.state(State::Processing);

    // Give other applications their volume back now rather than at the end of the
    // function: recording is over, and processing takes long enough that leaving music
    // quiet through it would be noticeable. The stop cue has already been queued, so it
    // is not drowned by whatever comes back.
    drop(ducker);

    let recording = match recorder.finish(config.audio.input_gain) {
        Ok(r) => r,
        Err(e) => {
            ui.error("Lathe could not finish recording", &format!("{e:#}"));
            return;
        }
    };
    if recording.xruns > 0 {
        eprintln!(
            "warning: {} stream error(s) during capture; audio may have gaps",
            recording.xruns
        );
    }
    eprintln!(
        "captured {:.2}s from {} -- peak {:.1} dBFS",
        recording.pcm.len() as f32 / lathe_core::audio::TARGET_RATE as f32,
        recording.device,
        recording.peak_db
    );

    // Amendment A2: unwind and report rather than abort, so the error cue can sound.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.process(config, &preset, &recording.pcm)
    }));

    let outcome = match result {
        Err(_) => {
            ui.error(
                "Lathe hit a bug",
                &format!(
                    "The dictation pipeline panicked. The details are in {}.",
                    crate::log_path_for_display()
                ),
            );
            return;
        }
        Ok(Err(e)) => {
            ui.error("Lathe could not transcribe", &format!("{e:#}"));
            return;
        }
        Ok(Ok(processed)) => processed,
    };

    match outcome {
        Processed::Rejected(Rejected::NoSpeech) => {
            // Brief 6.1. Not a failure: the gate did its job. The user still needs to
            // know nothing is coming, and why, or a silent no-op looks like a crash.
            ui.nothing_to_do(
                "Lathe heard no speech",
                "Nothing was transcribed. Check the microphone is the right one and \n                 that its level is not too low, in Settings under Audio.",
            );
        }
        Processed::Rejected(Rejected::EmptyTranscript) => {
            ui.nothing_to_do(
                "Lathe transcribed nothing",
                "Speech was detected but the model returned no text.",
            );
        }
        Processed::Done(outcome) => {
            *last_raw = Some(outcome.raw.clone());

            // Brief 6.4. Recorded before pasting, so a paste that fails still leaves the
            // transcript recoverable -- which is most of the point of keeping it.
            if config.history.enabled {
                if let Some(history) = history {
                    if let Err(e) = history.record(
                        &outcome.preset,
                        &outcome.raw,
                        &outcome.cleaned,
                        outcome.audio_secs,
                        outcome.asr_ms,
                        outcome.cleanup_ms,
                    ) {
                        eprintln!("could not record history: {e:#}");
                    }
                }
            }

            eprintln!(
                "{} | {:.2}s audio | vad {}ms | asr {}ms | cleanup {}ms",
                outcome.preset,
                outcome.audio_secs,
                outcome.vad_ms,
                outcome.asr_ms,
                outcome.cleanup_ms
            );

            // Brief 4.2: an empty string is a valid result. Nothing is pasted, the raw
            // transcript is kept, and the error cue says why nothing appeared.
            if outcome.cleaned.is_empty() {
                // Brief 4.2: an empty result is valid, not a failure. Filler-only
                // speech normalises to nothing.
                ui.nothing_to_do(
                    "Nothing left after cleanup",
                    "What you said normalised to an empty string, so nothing was pasted. \n                     Ctrl+Shift+Space pastes it uncleaned.",
                );
                return;
            }

            match deliver(config, &outcome.cleaned, preset.auto_paste) {
                Ok(()) => {
                    if config.cues.tick_on_paste {
                        ui.cue(Cue::Tick);
                    }
                    ui.state(State::Idle);
                }
                Err(e) => ui.error("Lathe could not paste", &format!("{e:#}")),
            }
        }
    }
}

/// Brief 5.6. With auto_paste off the text only reaches the clipboard.
fn deliver(config: &Config, text: &str, auto_paste: bool) -> anyhow::Result<()> {
    if !auto_paste {
        return paste::copy_only(text);
    }
    let method = paste::choose(text, config.output.clipboard_threshold);
    paste::paste(
        text,
        method,
        config.output.clipboard_restore_delay_ms,
        config.output.keep_on_clipboard,
    )
}
