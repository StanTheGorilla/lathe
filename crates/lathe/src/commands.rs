// Tauri commands. Brief section 3: this is the whole surface between the settings
// window and the resident core.

use lathe_core::audio;
use lathe_core::config::Config;
use lathe_core::secrets::{self, KeyStatus, KeyStore, OsKeyStore};
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
    let removed: Vec<String> = {
        let old = state.config.lock().unwrap();
        old.providers
            .iter()
            .filter(|p| config.provider(&p.id).is_none())
            .map(|p| p.id.clone())
            .collect()
    };
    *state.config.lock().unwrap() = config;
    // A provider removed and saved takes its key with it: a key for nothing is a key
    // nobody remembers is there.
    for id in removed {
        if let Err(e) = OsKeyStore.delete(&id) {
            eprintln!("providers: could not remove the key of '{id}': {e:#}");
        }
    }
    Ok(())
}

/// Provider ids are made by the settings window; anything else is refused before it
/// becomes the name of a credential.
fn provider_id(id: &str) -> Reply<&str> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(id)
    } else {
        Err(format!("'{id}' is not a provider id"))
    }
}

/// Whether a provider has a key, and its last four characters. Never the key: the
/// window gets that only from `reveal_provider_key`, when the user asks to see it.
#[tauri::command]
pub fn provider_key_status(id: String) -> Reply<KeyStatus> {
    let key = OsKeyStore.get(provider_id(&id)?).map_err(fail)?;
    Ok(KeyStatus::of(key.as_deref()))
}

/// Saves a key straight to the credential store, and checks it is really there.
#[tauri::command]
pub fn set_provider_key(id: String, key: String) -> Reply<KeyStatus> {
    let id = provider_id(&id)?;
    let key = key.trim();
    if key.is_empty() {
        OsKeyStore.delete(id).map_err(fail)?;
    } else {
        secrets::set_verified(&OsKeyStore, id, key).map_err(fail)?;
    }
    Ok(KeyStatus::of(Some(key)))
}

/// The key itself, for the Show button. The window hides it again after 30 seconds.
#[tauri::command]
pub fn reveal_provider_key(id: String) -> Reply<String> {
    OsKeyStore
        .get(provider_id(&id)?)
        .map_err(fail)?
        .ok_or_else(|| "no key is saved for this provider".to_string())
}

#[tauri::command]
pub fn delete_provider_key(id: String) -> Reply<()> {
    OsKeyStore.delete(provider_id(&id)?).map_err(fail)
}

/// The provider as saved. The key is read from the credential store by id, and goes
/// only to the address in the saved config: an address typed into the window but not
/// saved is refused, so nothing in the window can point the saved key somewhere else.
/// The window saves before it asks, so a mismatch means that save failed.
fn saved_provider(
    provider: &lathe_core::config::Provider,
    state: &AppState,
) -> Reply<lathe_core::config::Provider> {
    provider_id(&provider.id)?;
    let saved = state.config.lock().unwrap().provider(&provider.id).cloned();
    match saved {
        Some(saved)
            if saved.base_url.trim() == provider.base_url.trim() && saved.api == provider.api =>
        {
            Ok(saved)
        }
        _ => Err("The address is not saved yet, and only a saved address gets the key. \
                  Try again in a moment."
            .into()),
    }
}

/// One small request to a model, so a provider can be checked before a dictation
/// depends on it.
#[tauri::command]
pub async fn test_cloud_model(
    provider: lathe_core::config::Provider,
    model: String,
    kind: lathe_core::config::CloudKind,
    state: State<'_, AppState>,
) -> Reply<String> {
    let provider = saved_provider(&provider, &state)?;
    tauri::async_runtime::spawn_blocking(move || {
        use lathe_core::config::CloudKind;
        match kind {
            CloudKind::Cleanup => {
                let preset = Config::default().active().clone();
                let out = lathe_core::engine::cloud_clean(
                    &provider,
                    &model,
                    &preset,
                    "um so this is a test of the the cleanup model",
                    "en",
                    &[],
                    &[],
                )?;
                Ok(format!("It answered: {out}"))
            }
            CloudKind::Speech => {
                // One second of silence: enough to prove the address, model and key.
                let silence = vec![0.0f32; lathe_core::audio::TARGET_RATE as usize];
                let out = lathe_core::engine::cloud_transcribe(&provider, &model, &silence, "en", &[])?;
                Ok(if out.is_empty() {
                    "It answered (with nothing, as expected for a second of silence).".into()
                } else {
                    format!("It answered: {out}")
                })
            }
        }
    })
    .await
    .map_err(fail)?
    .map_err(|e: anyhow::Error| fail(e))
}

/// The models a provider offers, from its own list, for the window to pick from.
/// Same rule as Test: the saved address, the stored key.
#[tauri::command]
pub async fn list_provider_models(
    provider: lathe_core::config::Provider,
    state: State<'_, AppState>,
) -> Reply<Vec<lathe_core::cloud::Listed>> {
    let provider = saved_provider(&provider, &state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let key = OsKeyStore.get(&provider.id)?;
        lathe_core::cloud::list_models(&provider, key)
    })
    .await
    .map_err(fail)?
    .map_err(|e: anyhow::Error| fail(e))
}

#[tauri::command]
pub fn config_path(state: State<'_, AppState>) -> Reply<String> {
    Ok(state.config_path.display().to_string())
}

/// The vocabulary sets a fresh config starts with, so the settings window can offer to
/// restore one without carrying its own copy of the list.
#[tauri::command]
pub fn default_vocabulary_sets() -> Vec<lathe_core::vocabulary::Set> {
    lathe_core::vocabulary::Vocabulary::default().sets
}

#[derive(Serialize)]
pub struct Adapter {
    id: i32,
    name: String,
    vram_free: usize,
    vram_total: usize,
    /// "discrete", "integrated", "cpu" or "other", so the picker can say which is which.
    kind: String,
    /// True for the one automatic selection would choose.
    preferred: bool,
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
    let found = lathe_core::asr::list_adapters();
    let preferred = lathe_core::asr::best_adapter(&found).map(|a| a.id);
    let adapters = found
        .iter()
        .map(|a| Adapter {
            id: a.id,
            name: a.name.clone(),
            vram_free: 0,
            vram_total: a.vram_total,
            kind: a.kind.label().to_string(),
            preferred: Some(a.id) == preferred,
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
/// Saves a hand-corrected transcript and says which words it swapped one for one, so
/// the window can offer each to the vocabulary as a spoken form.
#[tauri::command]
pub async fn history_correct(
    id: i64,
    cleaned: String,
    state: State<'_, AppState>,
) -> Reply<Vec<(String, String)>> {
    let store = history(&state)?;
    let before = store
        .get(id)
        .map_err(fail)?
        .ok_or_else(|| "that dictation is no longer in the history".to_string())?;
    store.correct(id, &cleaned).map_err(fail)?;
    Ok(lathe_core::history::word_changes(&before.cleaned, &cleaned))
}

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

/// Amendment A30: what the settings window has to know about the platform it is on.
#[derive(Serialize)]
pub struct Platform {
    /// "windows", "macos" or "linux".
    os: &'static str,
    /// The name of the Windows/Command/Super key as this platform writes it.
    super_key: &'static str,
    /// Whether other applications can be quietened while recording.
    ducking: bool,
}

#[tauri::command]
pub fn platform() -> Reply<Platform> {
    Ok(Platform {
        os: std::env::consts::OS,
        super_key: lathe_core::hotkey::super_label(),
        ducking: lathe_core::ducking::available(),
    })
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
                // The window may be closed by the time it fails; the log keeps it.
                eprintln!("download of {file} failed: {e:#}");
                p.error = format!("{e:#}");
            }
        }
    });

    Ok(())
}

/// What a change of models directory would have to move.
#[derive(Serialize)]
pub struct MovePlan {
    files: u32,
    bytes: u64,
    /// True when the chosen directory is the one already in use.
    same: bool,
}

#[tauri::command]
pub fn plan_models_move(to: String, state: State<'_, AppState>) -> Reply<MovePlan> {
    let from = state.config.lock().unwrap().models.dir.clone();
    let to = std::path::PathBuf::from(to);
    if from == to {
        return Ok(MovePlan {
            files: 0,
            bytes: 0,
            same: true,
        });
    }
    let present = lathe_core::download::present_in(&from);
    Ok(MovePlan {
        files: present.len() as u32,
        bytes: present.iter().map(|(_, size)| *size).sum(),
        same: false,
    })
}

/// Opens the native folder picker. Returns None if the user dismissed it.
#[tauri::command]
pub async fn pick_models_dir(app: tauri::AppHandle, current: String) -> Reply<Option<String>> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = mpsc::channel();
    let mut picker = app.dialog().file().set_title("Where to keep the models");
    let start = std::path::PathBuf::from(&current);
    if start.is_dir() {
        picker = picker.set_directory(start);
    }
    picker.pick_folder(move |chosen| {
        let _ = tx.send(chosen);
    });

    let chosen = tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(fail)?;

    Ok(chosen
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

/// Points the app at a new models directory, optionally moving what is already there.
///
/// The directory is validated by creating it, the same way the downloader does: asking
/// the filesystem is the only honest test of whether a path is writable.
#[tauri::command]
pub fn set_models_dir(dir: String, move_existing: bool, state: State<'_, AppState>) -> Reply<()> {
    let to = std::path::PathBuf::from(&dir);
    if to.as_os_str().is_empty() {
        return Err("choose a folder first".into());
    }
    std::fs::create_dir_all(&to)
        .map_err(|e| format!("{} cannot be used: {e}", to.display()))?;

    let from = state.config.lock().unwrap().models.dir.clone();
    if from == to {
        return Ok(());
    }

    if move_existing {
        let running = state.download.lock().unwrap();
        if running.as_ref().is_some_and(|p| !p.finished) {
            return Err("wait for the download in progress to finish first".into());
        }
    }

    let mut config = state.config.lock().unwrap().clone();
    config.models.dir = to.clone();
    save_config(config, state.clone())?;

    if move_existing {
        let shared = std::sync::Arc::clone(&state.download);
        let cancel = std::sync::Arc::clone(&state.download_cancel);
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        *shared.lock().unwrap() = Some(DownloadProgress::default());

        std::thread::spawn(move || {
            let cancel_flag = std::sync::Arc::clone(&cancel);
            let result = lathe_core::download::relocate(
                &from,
                &to,
                |file, done, total| {
                    if let Some(p) = shared.lock().unwrap().as_mut() {
                        p.file = file.to_string();
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
    }

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

/// Amendment A31: the running version and what the last update check found.
#[tauri::command]
pub fn update_status(state: State<'_, AppState>) -> Reply<crate::update::Status> {
    Ok(state.update.lock().unwrap().clone())
}

/// The About screen's "Check now". Works whether or not the daily check is on.
#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Reply<crate::update::Status> {
    tauri::async_runtime::spawn_blocking(move || crate::update::check_now(&app))
        .await
        .map_err(fail)
}

#[tauri::command]
pub fn open_release_page(url: String) -> Reply<()> {
    crate::update::open_release_page(&url);
    Ok(())
}

/// The About screen's "Update now": fetches the installer in the background. Progress
/// comes back through `update_status`.
#[tauri::command]
pub fn download_update(app: tauri::AppHandle) -> Reply<()> {
    crate::update::start_download(&app)
}

/// "Restart to update": runs the downloaded installer and quits.
#[tauri::command]
pub fn install_update(app: tauri::AppHandle) -> Reply<()> {
    crate::update::install(&app)
}

/// What the platform still needs from the user before dictation can work: the
/// Accessibility permission on macOS, the input group and the udev rule on Linux.
/// Empty on Windows and on a machine that is set up.
#[tauri::command]
pub fn setup_status() -> Reply<Vec<crate::setup::Problem>> {
    Ok(crate::setup::problems())
}

/// The section the core wants shown, if it opened the window for one. Cleared on
/// reading, so a later plain open lands on the default.
#[tauri::command]
pub fn take_section(state: State<'_, AppState>) -> Reply<Option<&'static str>> {
    Ok(state.open_at.lock().unwrap().take())
}
