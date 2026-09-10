// Config lives in a single config.toml under %APPDATA%\Lathe\, per brief section 3.
// It is hand-edited in phase 3; the settings UI in phase 4 writes the same file.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cleanup::{Context, Structure, Styling};

pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("no roaming AppData directory")?;
    Ok(base.join("Lathe"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Amendment A6, reversed by A20: back to Ctrl+Space by request, because
    /// Ctrl+Alt+Space was already taken by another application on this machine.
    pub hotkey: String,
    /// Brief 5.6: re-paste the last transcript pre-cleanup.
    pub paste_raw_hotkey: String,
    /// Brief 5.1: a press shorter than this toggles, longer is push-to-talk.
    pub tap_threshold_ms: u64,
    /// Brief 5.1: hard cap so a stuck key cannot fill RAM.
    pub max_record_secs: u64,
    pub active_preset: String,
    pub audio: Audio,
    pub models: Models,
    pub remote_asr: RemoteAsr,
    pub cues: Cues,
    pub output: Output,
    pub vocabulary: crate::vocabulary::Vocabulary,
    pub history: HistoryConfig,
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Substring match against the device name. Empty means the system default.
    pub input_device: String,
    /// Output device for cues, separate from the input per brief 5.2.
    pub output_device: String,
    pub input_gain: f32,
    /// Brief 5.7: reject clips with less than this much detected speech.
    pub min_speech_ms: u32,
    /// Quieten other applications while recording. Not in the brief; see amendment A16.
    pub duck_others: bool,
    /// What to scale other applications to while recording. 0.0 silences them, 1.0
    /// leaves them alone. Defaults to full silence.
    pub duck_level: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    pub dir: PathBuf,
    /// The speech model file. Cohere Transcribe by default: measured faster and more
    /// accurate than Whisper on this hardware. Must be GGUF.
    pub whisper: String,
    pub vad: String,
    pub cleanup: String,
    /// Cleanup for languages S1-mini does not cover. Optional; see amendment A21.
    pub cleanup_multilingual: String,
    /// Vulkan adapter index, brief 4.1. Enumerated names are printed by `lathe devices`.
    pub gpu_device: i32,
    pub threads: i32,
    /// Brief 4.4: free VRAM after this long idle. Zero disables unloading.
    pub idle_unload_secs: u64,
    pub keep_loaded: bool,
}

/// Brief 4.3: "External ASR endpoint (advanced)", off by default.
///
/// Any server speaking the OpenAI `/v1/audio/transcriptions` shape. This is the only
/// way to reach Cohere Transcribe, which cannot run locally on this hardware.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteAsr {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub timeout_secs: u64,
    /// Fall back to the local model when the endpoint fails, rather than surfacing the
    /// error. Off by default: brief section 10 requires failing loudly, and a silent
    /// downgrade to a weaker model would hide a broken configuration indefinitely.
    pub fallback_to_local: bool,
}

impl Default for RemoteAsr {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            model: "cohere-transcribe-03-2026".into(),
            api_key: String::new(),
            timeout_secs: 120,
            fallback_to_local: false,
        }
    }
}

/// Brief 6.4.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    /// Brief 6.4 names 200 dictations.
    pub limit: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            limit: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cues {
    pub volume: f32,
    /// Brief 5.2: off by default.
    pub tick_on_paste: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Output {
    /// Brief 5.6: clipboard path above this length, SendInput below.
    pub clipboard_threshold: usize,
    /// Milliseconds to wait before restoring the previous clipboard contents.
    pub clipboard_restore_delay_ms: u64,
    /// Leave the dictated text on the clipboard after pasting, so it can be pasted
    /// again by hand. Overrides the restore behaviour in brief 5.6; see amendment A12.
    pub keep_on_clipboard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub styling: Styling,
    pub structure: Structure,
    pub context: Context,
    pub lang: String,
    /// False for the Raw preset, which bypasses S1-mini entirely per brief 5.3.
    #[serde(default = "yes")]
    pub cleanup: bool,
    #[serde(default = "yes")]
    pub auto_paste: bool,
    /// Brief 5.5: which vocabulary sets this preset uses. Empty means every set that
    /// is enabled globally.
    #[serde(default)]
    pub vocabulary_sets: Vec<String>,
    /// Brief 6.8: find/replace applied after cleanup.
    #[serde(default)]
    pub replacements: Vec<crate::vocabulary::Replacement>,
    #[serde(default)]
    pub hotkey: Option<String>,
}

fn yes() -> bool {
    true
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            output_device: String::new(),
            input_gain: 1.0,
            min_speech_ms: 300,
            duck_others: true,
            // Silence, not "quieter". Lowering to 15% still leaves speech audible enough
            // to be picked up by the microphone and transcribed, which is the problem
            // this exists to solve.
            duck_level: 0.0,
        }
    }
}

impl Default for Models {
    fn default() -> Self {
        Self {
            dir: config_dir()
                .map(|d| d.join("models"))
                .unwrap_or_else(|_| PathBuf::from("models")),
            // Q8_0, not Q5_0 and not F16. Measured on this hardware: Q8_0 matches Q5_0's
            // speed exactly (296ms vs 305ms warm) at higher precision, while F16 is 18%
            // slower and 1.6GB larger for no observable gain. See amendment A23.
            whisper: "cohere-transcribe-q8_0.gguf".into(),
            vad: "ggml-silero-v5.1.2.bin".into(),
            // F16, not Q4_K_M. Q4 was observed dropping a clause outright -- "send me the
            // file when you get a chance" came back without "when you get a chance".
            // Losing meaning is not worth the 111ms. See amendment A23.
            cleanup: "s1-mini-f16.gguf".into(),
            cleanup_multilingual: "gemma-3-4b-it-qat-Q4_0.gguf".into(),
            gpu_device: 0,
            threads: std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(8),
            idle_unload_secs: 15 * 60,
            keep_loaded: false,
        }
    }
}

impl Default for Cues {
    fn default() -> Self {
        Self {
            // Reported a little loud at 0.2 against the richer cue in A25: more
            // partials means more energy at the same peak, so the same number reads
            // louder than it used to.
            volume: 0.14,
            tick_on_paste: false,
        }
    }
}

impl Default for Output {
    fn default() -> Self {
        Self {
            clipboard_threshold: 200,
            clipboard_restore_delay_ms: 400,
            keep_on_clipboard: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Space".into(),
            paste_raw_hotkey: "Ctrl+Shift+Space".into(),
            tap_threshold_ms: 400,
            max_record_secs: 300,
            active_preset: "Prompt".into(),
            audio: Audio::default(),
            models: Models::default(),
            remote_asr: RemoteAsr::default(),
            cues: Cues::default(),
            output: Output::default(),
            vocabulary: crate::vocabulary::Vocabulary::default(),
            history: HistoryConfig::default(),
            presets: default_presets(),
        }
    }
}

/// Brief 5.3, in exactly the order the brief lists them.
fn default_presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "Prompt".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::General,
            lang: "en".into(),
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
        Preset {
            name: "Message".into(),
            styling: Styling::SemiCasual,
            structure: Structure::Prose,
            context: Context::General,
            lang: "en".into(),
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
        Preset {
            name: "Email".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::Email,
            lang: "en".into(),
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
        Preset {
            name: "Notes".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Lists,
            context: Context::General,
            lang: "en".into(),
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
        // Requested during dogfooding: this user dictates in English and Polish. The
        // preset exists so switching language is one tray click, not a settings edit.
        Preset {
            name: "Polski".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::General,
            lang: "pl".into(),
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
        Preset {
            name: "Raw".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::General,
            lang: "en".into(),
            cleanup: false,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
        },
    ]
}

impl Config {
    /// Loads the config, writing a default one first if none exists.
    /// Returns the config, its path, and whether this run created it.
    pub fn load_or_create() -> Result<(Self, PathBuf, bool)> {
        let path = config_path()?;
        if !path.exists() {
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
            let default = Config::default();
            std::fs::write(&path, default.to_toml()?)
                .with_context(|| format!("writing {}", path.display()))?;
            return Ok((default, path, true));
        }
        let config = Self::load(&path)?;
        Ok((config, path, false))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.migrate();
        Ok(config)
    }

    /// Carries older config files forward.
    ///
    /// The speech runtime changed from whisper.cpp to CrispASR (amendment A14), which
    /// reads GGUF and not the legacy whisper.cpp .bin format. A config naming a .bin
    /// would otherwise fail on the next dictation with an error about the file, which is
    /// true but unhelpful.
    fn migrate(&mut self) {
        if self.models.whisper.ends_with(".bin") {
            let previous = std::mem::replace(
                &mut self.models.whisper,
                Models::default().whisper,
            );
            eprintln!(
                "config: speech model '{previous}' is the legacy whisper.cpp format, \n                 which is no longer supported; using '{}' instead",
                self.models.whisper
            );
        }
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// The preset selected in the tray.
    ///
    /// Brief 6.3 specified automatic switching based on the foreground application.
    /// Removed by request: see amendment A13. The preset is whatever the user picked.
    pub fn active(&self) -> &Preset {
        self.preset(&self.active_preset)
            .or_else(|| self.presets.first())
            .expect("config must contain at least one preset")
    }

    pub fn whisper_path(&self) -> PathBuf {
        self.models.dir.join(&self.models.whisper)
    }

    pub fn vad_path(&self) -> PathBuf {
        self.models.dir.join(&self.models.vad)
    }

    pub fn cleanup_path(&self) -> PathBuf {
        self.models.dir.join(&self.models.cleanup)
    }

    pub fn cleanup_multilingual_path(&self) -> PathBuf {
        self.models.dir.join(&self.models.cleanup_multilingual)
    }
}

/// Brief section 3: the config file is watched so the core reloads without a restart.
///
/// Sends every successful reload, and the error text when a hand edit does not parse --
/// the daemon surfaces that as a toast, because a config typo that silently reverted to
/// the previous settings would be worse than saying so.
pub fn watch(path: PathBuf) -> std::sync::mpsc::Receiver<Result<Config, String>> {
    use notify::{RecursiveMode, Watcher};
    use std::sync::mpsc;

    let (out_tx, out_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("config watch unavailable: {e}");
                return;
            }
        };
        let Some(dir) = path.parent().map(|d| d.to_path_buf()) else {
            return;
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            eprintln!("config watch failed: {e}");
            return;
        }

        for event in rx {
            let Ok(event) = event else { continue };
            if !event.paths.iter().any(|p| p == &path) {
                continue;
            }
            // Editors write in several steps; let the file settle before reading.
            std::thread::sleep(std::time::Duration::from_millis(150));
            let result = Config::load(&path).map_err(|e| format!("{e:#}"));
            if out_tx.send(result).is_err() {
                return;
            }
        }
    });

    out_rx
}

pub fn history_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("history.db"))
}
