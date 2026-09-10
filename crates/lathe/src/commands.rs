// Tauri commands. Brief section 3: this is the whole surface between the settings
// window and the resident core.

use lathe_core::audio;
use lathe_core::config::Config;
use serde::Serialize;
use std::sync::mpsc;
use tauri::State;

use crate::worker::{BenchReport, Msg};
use crate::AppState;

type Reply<T> = Result<T, String>;

fn fail(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[tauri::command]
pub fn load_config(state: State<'_, AppState>) -> Reply<Config> {
    Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
pub fn save_config(config: Config, state: State<'_, AppState>) -> Reply<()> {
    // Written to disk first. The file is the source of truth, and the watcher will fire
    // and reload it -- writing memory first would let the two diverge if the write fails.
    let text = config.to_toml().map_err(fail)?;
    std::fs::write(&state.config_path, text).map_err(fail)?;
    *state.config.lock().unwrap() = config;
    Ok(())
}

#[tauri::command]
pub fn config_path(state: State<'_, AppState>) -> Reply<String> {
    Ok(state.config_path.display().to_string())
}

#[derive(Serialize)]
pub struct Adapter {
    id: i32,
    name: String,
    vram_free: usize,
    vram_total: usize,
}

#[derive(Serialize)]
pub struct Device {
    name: String,
    sample_rate: u32,
    channels: u16,
    is_default: bool,
}

#[derive(Serialize)]
pub struct Devices {
    adapters: Vec<Adapter>,
    inputs: Vec<Device>,
    outputs: Vec<Device>,
}

impl From<audio::DeviceInfo> for Device {
    fn from(d: audio::DeviceInfo) -> Self {
        Self {
            name: d.name,
            sample_rate: d.sample_rate,
            channels: d.channels,
            is_default: d.is_default,
        }
    }
}

/// Async because enumerating Vulkan adapters initialises the backend, and enumerating
/// audio devices talks to WASAPI. Neither belongs on the main thread.
#[tauri::command]
pub async fn list_devices() -> Reply<Devices> {
    tauri::async_runtime::spawn_blocking(enumerate)
        .await
        .map_err(fail)?
}

fn enumerate() -> Reply<Devices> {
    let adapters = lathe_core::asr::list_adapters()
        .into_iter()
        .map(|(id, name, vram_total)| Adapter {
            id,
            name,
            vram_free: 0,
            vram_total,
        })
        .collect();

    Ok(Devices {
        adapters,
        inputs: audio::input_devices()
            .map_err(fail)?
            .into_iter()
            .map(Into::into)
            .collect(),
        outputs: audio::output_devices()
            .map_err(fail)?
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

#[derive(Serialize)]
pub struct ModelFile {
    role: String,
    name: String,
    present: bool,
    size: u64,
}

#[derive(Serialize)]
pub struct ModelStatus {
    dir: String,
    files: Vec<ModelFile>,
    total_bytes: u64,
    /// The backend the speech model will run on, and whether it does anything with the
    /// vocabulary's recogniser biasing. The Vocabulary screen says so rather than
    /// offering a control that silently does nothing.
    speech_backend: String,
    speech_biases: bool,
}

#[tauri::command]
pub fn model_status(state: State<'_, AppState>) -> Reply<ModelStatus> {
    let config = state.config.lock().unwrap().clone();
    let entries = [
        ("Speech", config.models.whisper.clone(), config.whisper_path()),
        ("Speech detection", config.models.vad.clone(), config.vad_path()),
        ("Cleanup", config.models.cleanup.clone(), config.cleanup_path()),
    ];

    let mut files = Vec::new();
    let mut total = 0u64;
    for (role, name, path) in entries {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        total += size;
        files.push(ModelFile {
            role: role.to_string(),
            name,
            present: path.exists(),
            size,
        });
    }

    let speech_backend =
        lathe_core::asr::backend_of(&config.whisper_path()).unwrap_or_default();

    Ok(ModelStatus {
        dir: config.models.dir.display().to_string(),
        files,
        total_bytes: total,
        speech_biases: lathe_core::asr::backend_biases(&speech_backend),
        speech_backend,
    })
}

/// Async: opening a capture device can take a moment, and the main thread must stay free.
#[tauri::command]
pub async fn start_level_meter(state: State<'_, AppState>) -> Reply<()> {
    let hint = state.config.lock().unwrap().audio.input_device.clone();
    let meter = tauri::async_runtime::spawn_blocking(move || audio::LevelMeter::start(&hint))
        .await
        .map_err(fail)?
        .map_err(fail)?;
    *state.meter.lock().unwrap() = Some(meter);
    Ok(())
}

#[tauri::command]
pub fn stop_level_meter(state: State<'_, AppState>) -> Reply<()> {
    // Dropping it stops the thread and releases the microphone.
    *state.meter.lock().unwrap() = None;
    Ok(())
}

#[derive(Serialize)]
pub struct Level {
    peak_db: f32,
    rms_db: f32,
}

#[tauri::command]
pub fn input_level(state: State<'_, AppState>) -> Reply<Level> {
    let guard = state.meter.lock().unwrap();
    let meter = guard.as_ref().ok_or("the level meter is not running")?;
    let (peak_db, rms_db) = meter.level();
    Ok(Level { peak_db, rms_db })
}

/// Brief 6.9. Runs on the worker so it uses the models that are already resident rather
/// than loading a second copy onto the GPU.
///
/// Async, and the wait happens on a blocking thread. A synchronous Tauri command runs on
/// the main thread, so waiting there freezes the window for the whole benchmark -- which
/// it did, complete with the "not responding" title.
#[tauri::command]
pub async fn run_benchmark(state: State<'_, AppState>) -> Reply<BenchReport> {
    let (tx, rx) = mpsc::channel();
    // Clone the sender so nothing borrowed from state is held across the await.
    let worker = state.worker.clone();
    worker.send(Msg::Benchmark(tx)).map_err(fail)?;

    tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(180))
    })
    .await
    .map_err(fail)?
    .map_err(|_| "the benchmark timed out".to_string())?
    .map_err(|e| e.to_string())
}

/// Opens the history store for a one-off query.
///
/// The worker holds its own connection for writing. SQLite handles two connections to
/// one file, and opening per query keeps the settings window from having to coordinate
/// with the worker for what is a read.
fn history(state: &AppState) -> Result<lathe_core::history::History, String> {
    let path = lathe_core::config::history_path().map_err(fail)?;
    let limit = state.config.lock().unwrap().history.limit;
    lathe_core::history::History::open(&path, limit).map_err(fail)
}

#[tauri::command]
pub async fn history_recent(
    search: String,
    limit: usize,
    state: State<'_, AppState>,
) -> Reply<Vec<lathe_core::history::Item>> {
    let store = history(&state)?;
    store.recent(limit.min(500), &search).map_err(fail)
}

#[tauri::command]
pub async fn history_stats(state: State<'_, AppState>) -> Reply<lathe_core::history::Stats> {
    history(&state)?.stats().map_err(fail)
}

#[tauri::command]
pub async fn history_wipe(state: State<'_, AppState>) -> Reply<()> {
    history(&state)?.wipe().map_err(fail)
}

/// Brief 6.4: one-click re-paste of an earlier dictation.
#[tauri::command]
pub async fn history_paste(id: i64, raw: bool, state: State<'_, AppState>) -> Reply<()> {
    let store = history(&state)?;
    let item = store
        .get(id)
        .map_err(fail)?
        .ok_or_else(|| "that dictation is no longer in the history".to_string())?;
    let text = if raw { item.raw } else { item.cleaned };
    let config = state.config.lock().unwrap().clone();

    // Give focus back to whatever the user was in before they opened settings; pasting
    // into the settings window itself would be useless.
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let method = lathe_core::paste::choose(&text, config.output.clipboard_threshold);
        lathe_core::paste::paste(
            &text,
            method,
            config.output.clipboard_restore_delay_ms,
            config.output.keep_on_clipboard,
        )
    })
    .await
    .map_err(fail)?
    .map_err(fail)
}

/// Brief 5.7: launch at login.
#[tauri::command]
pub fn autostart_enabled() -> Reply<bool> {
    Ok(lathe_core::autostart::is_enabled())
}

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Reply<()> {
    lathe_core::autostart::set_enabled(enabled).map_err(fail)
}

/// Brief 5.7: the model manager. What can be fetched, and what is already here.
#[derive(Serialize)]
pub struct Downloadable {
    file: String,
    label: String,
    role: String,
    approx_bytes: u64,
    required: bool,
    present: bool,
}

#[tauri::command]
pub fn downloadable_models(state: State<'_, AppState>) -> Reply<Vec<Downloadable>> {
    let dir = state.config.lock().unwrap().models.dir.clone();
    Ok(lathe_core::download::KNOWN
        .iter()
        .map(|k| Downloadable {
            file: k.file.to_string(),
            label: k.label.to_string(),
            role: k.role.to_string(),
            approx_bytes: k.approx_bytes,
            required: k.required,
            present: dir.join(k.file).exists(),
        })
        .collect())
}

/// Progress for whichever download is running, polled by the settings window.
#[derive(Serialize, Clone, Default)]
pub struct DownloadProgress {
    file: String,
    done: u64,
    total: u64,
    finished: bool,
    error: String,
}

#[tauri::command]
pub fn download_progress(state: State<'_, AppState>) -> Reply<Option<DownloadProgress>> {
    Ok(state.download.lock().unwrap().clone())
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>) -> Reply<()> {
    state
        .download_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Starts a download on a background thread and returns immediately. Progress is polled
/// rather than pushed: one file at a time, and a poll is simpler than an event stream
/// the window has to subscribe to and tear down.
#[tauri::command]
pub fn start_download(file: String, state: State<'_, AppState>) -> Reply<()> {
    let entry = lathe_core::download::known(&file)
        .ok_or_else(|| format!("'{file}' is not a model this app knows how to fetch"))?;

    {
        let running = state.download.lock().unwrap();
        if running.as_ref().is_some_and(|p| !p.finished) {
            return Err("a download is already running".into());
        }
    }

    let dir = state.config.lock().unwrap().models.dir.clone();
    let shared = std::sync::Arc::clone(&state.download);
    let cancel = std::sync::Arc::clone(&state.download_cancel);
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);

    *shared.lock().unwrap() = Some(DownloadProgress {
        file: file.clone(),
        ..Default::default()
    });

    std::thread::spawn(move || {
        let cancel_flag = std::sync::Arc::clone(&cancel);
        let result = lathe_core::download::fetch(
            entry,
            &dir,
            |done, total| {
                if let Some(p) = shared.lock().unwrap().as_mut() {
                    p.done = done;
                    p.total = total;
                }
            },
            &move || cancel_flag.load(std::sync::atomic::Ordering::Relaxed),
        );

        if let Some(p) = shared.lock().unwrap().as_mut() {
            p.finished = true;
            if let Err(e) = result {
                p.error = format!("{e:#}");
            }
        }
    });

    Ok(())
}

/// What binding Lathe last received.
///
/// Exists because a user pressing a key and hearing a beep has no way to tell which
/// binding fired, and guessing from the sound is exactly what went wrong. Only bound
/// combinations are ever recorded; this is not a key log.
#[tauri::command]
pub fn last_hotkey(state: State<'_, AppState>) -> Reply<Option<(String, String)>> {
    Ok(state.last_hotkey.lock().unwrap().clone())
}
