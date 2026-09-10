// Lathe. Brief section 9 phases 3 and 4.
//
// The application starts with no windows at all. Tauri is present, but WebView2 is not
// instantiated until the user opens settings from the tray, and closing that window
// destroys it again -- brief section 3 calls this the single most important structural
// decision in the app, and section 10 forbids starting the webview at launch.
//
// Threads:
//   main    -- Tauri's event loop, which owns the tray and any settings window
//   hook    -- WH_KEYBOARD_LL plus its own message pump, per amendment A1
//   worker  -- audio capture, models, and the paste chain
//
// The worker never touches the frontend.

#![windows_subsystem = "windows"]

mod commands;
mod tray;
mod worker;

use anyhow::Result;
use lathe_core::config::Config;
use lathe_core::hotkey::{self, Action, Binding, Bound};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Shared with the Tauri commands and the worker.
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub config_path: std::path::PathBuf,
    pub worker: mpsc::Sender<worker::Msg>,
    pub meter: Mutex<Option<lathe_core::audio::LevelMeter>>,
    /// Progress of the model download in flight, if any.
    pub download: Arc<Mutex<Option<commands::DownloadProgress>>>,
    pub download_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Rendered dictate binding, shown in the tray tooltip.
    pub hotkey_label: String,
    /// The last binding the hook matched, for the hotkey tester in settings. This is
    /// only ever written for combinations Lathe is bound to -- it is not a log of keys.
    pub last_hotkey: Arc<Mutex<Option<(String, String)>>>,
}

/// The flags that print something and exit. Only these want a console.
const CLI_FLAGS: [&str; 4] = ["--devices", "--config-path", "--help", "-h"];

/// A windows-subsystem binary has no console of its own. Borrow the launching
/// terminal's, but *only* for the flags above.
///
/// The tray run must never do this. It is a background app that outlives the shell that
/// started it, and whisper and ggml between them emit a few hundred lines per dictation:
/// attaching there dumps all of it into whatever terminal happened to be the parent,
/// interleaved with that terminal's own output, long after the launch. The tray run's
/// stderr therefore goes nowhere; run one of the flags above from a shell to inspect the
/// machine, or launch the binary with stderr redirected to capture a dictation.
fn attach_console_for_cli(args: &[String]) {
    if !args.iter().any(|a| CLI_FLAGS.contains(&a.as_str())) {
        return;
    }
    unsafe {
        use windows::Win32::System::Console::{
            AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
        };
        let redirected = GetStdHandle(STD_OUTPUT_HANDLE)
            .map(|h| !h.is_invalid() && !h.0.is_null())
            .unwrap_or(false);
        if !redirected {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Rotated at this size. Big enough for a long session's worth of ggml chatter, small
/// enough that a tray process running for months cannot fill a disk.
const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Sends the tray run's output to a file beside config.toml.
///
/// Without this the tray run has no diagnostic channel at all: it attaches to no
/// console, and the toast shown when the pipeline fails tells the user the log has the
/// details. Redirecting the process's standard handles rather than installing a logger
/// catches everything -- `eprintln!` from here, and whatever the native runtimes write
/// through the same handles -- without touching a single call site.
///
/// Returns the path so the caller can say where it went. Failing to open it is not worth
/// refusing to start over; the app simply keeps its silence.
fn redirect_output_to_log() -> Option<std::path::PathBuf> {
    use std::os::windows::io::AsRawHandle;

    let path = lathe_core::config::config_dir().ok()?.join("lathe.log");
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > LOG_MAX_BYTES {
        // One generation back is enough to survive a crash-and-restart.
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }

    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .ok()?;

    unsafe {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
        let handle = HANDLE(file.as_raw_handle());
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
    }
    // The standard handles now point at this file, so it has to outlive main.
    std::mem::forget(file);

    eprintln!(
        "\n===== Lathe {} started {} =====",
        env!("CARGO_PKG_VERSION"),
        local_now()
    );
    Some(path)
}

/// Where the log lives, for a message shown to someone who has to go and open it.
pub fn log_path_for_display() -> String {
    lathe_core::config::config_dir()
        .map(|d| d.join("lathe.log").display().to_string())
        .unwrap_or_else(|_| "the Lathe log".into())
}

/// Local wall-clock time, for the log header. Worth a line of Win32 rather than a date
/// crate: this is the only place in the app that formats a time for a human.
fn local_now() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    attach_console_for_cli(&args);
    if args.iter().any(|a| a == "--devices") {
        if let Err(e) = print_devices() {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--config-path") {
        match lathe_core::config::config_path() {
            Ok(p) => println!("{}", p.display()),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("lathe -- push-to-talk dictation");
        println!();
        println!("  (no arguments)   run in the tray");
        println!("  --devices        list Vulkan adapters and audio devices");
        println!("  --settings       open the settings window (forwards to a running copy)");
        println!("  --config-path    print the path to config.toml");
        return;
    }

    // Every CLI flag has returned by now, so this is the tray run.
    redirect_output_to_log();

    if let Err(e) = run(&args) {
        notify_user("Lathe failed to start", &format!("{e:#}"));
        eprintln!("fatal: {e:?}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<()> {
    // Both libraries log to stderr by default and there is usually no console.
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());

    let (config, config_path, first_run) = Config::load_or_create()?;

    // Brief 5.1 and 5.3: the main binding, the paste-raw binding, and one optional
    // binding per preset. A preset with an unparseable hotkey is reported and skipped
    // rather than taking the whole app down over a typo in a config file.
    let mut bindings = vec![Bound {
        binding: Binding::parse(&config.hotkey)?,
        action: Action::Record,
    }];

    // Optional. An empty or unparseable paste-raw binding means "not bound", not a
    // reason to refuse to start: losing a convenience should never cost the whole app.
    match config.paste_raw_hotkey.trim() {
        "" => {}
        spec => match Binding::parse(spec) {
            Ok(binding) => bindings.push(Bound {
                binding,
                action: Action::PasteRaw,
            }),
            Err(e) => eprintln!("paste-raw hotkey unusable, leaving it unbound: {e:#}"),
        },
    }
    for preset in &config.presets {
        let Some(spec) = preset.hotkey.as_deref().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        match Binding::parse(spec) {
            Ok(binding) => bindings.push(Bound {
                binding,
                action: Action::RecordPreset(preset.name.clone()),
            }),
            Err(e) => eprintln!("preset '{}' has an unusable hotkey: {e:#}", preset.name),
        }
    }

    let hotkey_label = hotkey::describe(&bindings[0].binding);
    let paste_raw_label = bindings
        .iter()
        .find(|b| b.action == Action::PasteRaw)
        .map(|b| hotkey::describe(&b.binding))
        .unwrap_or_else(|| "unbound".to_string());

    let tap_threshold = Duration::from_millis(config.tap_threshold_ms);
    let hotkey_mode = Arc::new(Mutex::new(config.hotkey_mode));
    let preset_names: Vec<String> = config.presets.iter().map(|p| p.name.clone()).collect();

    let config = Arc::new(Mutex::new(config));
    let (worker_tx, worker_rx) = mpsc::channel::<worker::Msg>();
    let last_hotkey: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));

    // The hook thread speaks in key events; everything reaching the worker is a Msg.
    {
        let worker_tx = worker_tx.clone();
        let (hotkey_tx, hotkey_rx) = mpsc::channel::<hotkey::Event>();
        let seen = Arc::clone(&last_hotkey);
        let labels = bindings
            .iter()
            .map(|b| (b.action.clone(), hotkey::describe(&b.binding)))
            .collect::<Vec<_>>();
        std::thread::spawn(move || {
            for event in hotkey_rx {
                // Record what arrived before forwarding it, so the settings window can
                // show the user which binding Lathe is actually receiving.
                let (combo, what) = match &event {
                    hotkey::Event::PasteRaw => (
                        labels
                            .iter()
                            .find(|(a, _)| *a == Action::PasteRaw)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_default(),
                        "paste last, uncleaned".to_string(),
                    ),
                    hotkey::Event::Start { preset } => (
                        labels
                            .iter()
                            .find(|(a, _)| match (a, preset) {
                                (Action::RecordPreset(n), Some(p)) => n == p,
                                (Action::Record, None) => true,
                                _ => false,
                            })
                            .map(|(_, l)| l.clone())
                            .unwrap_or_default(),
                        match preset {
                            Some(p) => format!("start dictating ({p})"),
                            None => "start dictating".to_string(),
                        },
                    ),
                    hotkey::Event::Stop => (String::new(), String::new()),
                };
                if !combo.is_empty() {
                    *seen.lock().unwrap() = Some((combo, what));
                }
                if worker_tx.send(worker::Msg::Hotkey(event)).is_err() {
                    return;
                }
            }
        });
        let mode_for_hook = Arc::clone(&hotkey_mode);
        std::thread::spawn(move || {
            hotkey::run(bindings, mode_for_hook, tap_threshold, hotkey_tx);
        });
    }

    let state = AppState {
        config: Arc::clone(&config),
        config_path: config_path.clone(),
        worker: worker_tx,
        meter: Mutex::new(None),
        download: Arc::new(Mutex::new(None)),
        download_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        hotkey_label: hotkey_label.clone(),
        last_hotkey: Arc::clone(&last_hotkey),
    };

    let open_settings_at_start = args.iter().any(|a| a == "--settings");
    let first_run_label = hotkey_label.clone();

    tauri::Builder::default()
        // One instance only. Two copies would install two keyboard hooks and handle
        // every hotkey twice, and would fight over the microphone. A second launch
        // forwards its arguments here and exits.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if argv.iter().any(|a| a == "--settings") {
                open_settings(app);
            }
        }))
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::load_config,
            commands::save_config,
            commands::config_path,
            commands::list_devices,
            commands::model_status,
            commands::start_level_meter,
            commands::stop_level_meter,
            commands::input_level,
            commands::run_benchmark,
            commands::history_recent,
            commands::history_stats,
            commands::history_wipe,
            commands::history_paste,
            commands::autostart_enabled,
            commands::set_autostart,
            commands::downloadable_models,
            commands::download_progress,
            commands::cancel_download,
            commands::start_download,
            commands::plan_models_move,
            commands::pick_models_dir,
            commands::set_models_dir,
            commands::last_hotkey,
        ])
        .setup(move |app| {
            build_tray(app.handle(), &preset_names, &hotkey_label, &paste_raw_label)?;

            worker::spawn(worker::Context {
                app: app.handle().clone(),
                config: Arc::clone(&config),
                rx: worker_rx,
            });

            watch_config(
                config_path,
                Arc::clone(&config),
                Arc::clone(&hotkey_mode),
                app.handle().clone(),
            );

            if open_settings_at_start {
                open_settings(app.handle());
            }

            // On the very first run the tray icon is the only thing that appeared, and
            // nothing has told the user which key starts a dictation. Section 8 rules
            // out an onboarding tour; one notification naming the binding is not that.
            if first_run {
                notify_user(
                    "Lathe is running",
                    &format!(
                        "Hold {first_run_label} to dictate, or tap it to start and tap \n                         again to stop. The tray icon shows what it is doing."
                    ),
                );
            }
            Ok(())
        })
        .build(tauri::generate_context!())?
        .run(|_app, event| {
            // With no windows open at startup, and none after settings is closed, Tauri
            // would otherwise consider the app finished and exit.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });

    Ok(())
}

fn build_tray(
    app: &AppHandle,
    preset_names: &[String],
    hotkey: &str,
    paste_raw: &str,
) -> Result<()> {
    let menu = Menu::new(app)?;

    // Disabled, so it reads as a label rather than an action. The user has to be able
    // to find the binding without opening settings or reading documentation.
    menu.append(&MenuItem::with_id(
        app,
        "binding",
        format!("{hotkey}  dictate"),
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "binding-raw",
        format!("{paste_raw}  paste last, uncleaned"),
        false,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    for name in preset_names {
        menu.append(&MenuItem::with_id(app, format!("preset:{name}"), name, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit Lathe", true, None::<&str>)?)?;

    TrayIconBuilder::with_id("lathe")
        .icon(tray_image(tray::State::Idle))
        .tooltip(tray::State::Idle.tooltip(hotkey))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if id == "quit" {
                app.exit(0);
            } else if id == "settings" {
                open_settings(app);
            } else if let Some(name) = id.strip_prefix("preset:") {
                if let Some(state) = app.try_state::<AppState>() {
                    state.config.lock().unwrap().active_preset = name.to_string();
                    eprintln!("active preset: {name}");
                }
            }
        })
        .build(app)?;

    Ok(())
}

pub fn tray_image(state: tray::State) -> Image<'static> {
    Image::new_owned(tray::icon_rgba(state), tray::ICON_SIZE, tray::ICON_SIZE)
}

/// Brief section 3: created only when asked for, and destroyed on close. This is the
/// only place a webview is ever instantiated.
pub fn open_settings(app: &AppHandle) {
    if let Some(existing) = app.get_webview_window("settings") {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Lathe")
        .inner_size(720.0, 560.0)
        .min_inner_size(720.0, 560.0)
        .resizable(true)
        .build();

    match built {
        Ok(window) => {
            // The builder's inner_size is not honoured here, so set it explicitly.
            // Brief section 8 asks for roughly 720x560.
            let _ = window.set_size(tauri::LogicalSize::new(720.0, 560.0));
            let _ = window.center();
            let _ = window.set_focus();
        }
        Err(e) => {
            eprintln!("could not open settings: {e}");
            notify_user("Lathe", &format!("Could not open settings: {e}"));
        }
    }
}

/// Applies reloads coming from the config file watcher.
fn watch_config(
    path: std::path::PathBuf,
    config: Arc<Mutex<Config>>,
    hotkey_mode: Arc<Mutex<lathe_core::hotkey::Mode>>,
    app: AppHandle,
) {
    let reloads = lathe_core::config::watch(path);
    std::thread::spawn(move || {
        for result in reloads {
            match result {
                Ok(reloaded) => {
                    let mut current = config.lock().unwrap();
                    let hotkey_changed = current.hotkey != reloaded.hotkey
                        || current.paste_raw_hotkey != reloaded.paste_raw_hotkey;
                    // The bindings themselves still need a restart, but the press style
                    // is read live by the state machine.
                    *hotkey_mode.lock().unwrap() = reloaded.hotkey_mode;
                    *current = reloaded;
                    drop(current);
                    eprintln!("config reloaded");
                    let _ = app.emit_to("settings", "config-reloaded", ());
                    if hotkey_changed {
                        notify_user("Lathe", "Hotkey change needs a restart to take effect.");
                    }
                }
                Err(e) => {
                    eprintln!("config reload failed: {e}");
                    notify_user("Lathe: config error", &e);
                }
            }
        }
    });
}

pub fn notify_user(title: &str, body: &str) {
    let _ = tauri_winrt_notification::Toast::new(
        tauri_winrt_notification::Toast::POWERSHELL_APP_ID,
    )
    .title(title)
    .text1(body)
    .show();
}

/// Brief 4.1 and 5.7: the adapter picker and the device dropdowns need names.
fn print_devices() -> Result<()> {
    let adapters = lathe_core::asr::list_adapters();
    let best = lathe_core::asr::best_adapter(&adapters).map(|a| a.id);
    println!("compute adapters:");
    for a in &adapters {
        let mark = if Some(a.id) == best { " <- automatic" } else { "" };
        println!(
            "  {}: {} [{}] -- {} MiB{mark}",
            a.id,
            a.name,
            a.kind.label(),
            a.vram_total / (1024 * 1024)
        );
    }
    println!();
    lathe_core::audio::list_devices()?;
    Ok(())
}

