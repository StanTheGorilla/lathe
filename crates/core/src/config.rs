// Config lives in a single config.toml under %APPDATA%\Lathe\, per brief section 3.
// It is hand-edited in phase 3; the settings UI in phase 4 writes the same file.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cleanup::{Context, Rewrite, Structure, Styling};

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
    /// The other platforms default differently; see `DEFAULT_HOTKEY`.
    pub hotkey: String,
    /// Brief 5.6: re-paste the last transcript pre-cleanup.
    pub paste_raw_hotkey: String,
    /// Whether the dictate binding is push-to-talk, a toggle, or decided by how long
    /// the press lasted. `tap_threshold_ms` only applies to the last of those.
    pub hotkey_mode: crate::hotkey::Mode,
    /// Brief 5.1: a press shorter than this toggles, longer is push-to-talk.
    pub tap_threshold_ms: u64,
    /// Brief 5.1: hard cap so a stuck key cannot fill RAM.
    pub max_record_secs: u64,
    pub active_preset: String,
    pub languages: Languages,
    pub audio: Audio,
    pub models: Models,
    /// Cloud providers: an OpenAI-compatible address and the models picked from it.
    /// Their keys are in the system credential store, never here (see `secrets`).
    pub providers: Vec<Provider>,
    /// The settings the Providers page replaced. Read so an old config can be moved
    /// across (`migrate_remote_asr`); written back only if that move failed, so the
    /// old key is never dropped before the credential store holds it.
    #[serde(skip_serializing_if = "RemoteAsr::is_unset")]
    pub remote_asr: RemoteAsr,
    pub cues: Cues,
    pub output: Output,
    pub vocabulary: crate::vocabulary::Vocabulary,
    pub history: HistoryConfig,
    pub updates: Updates,
    pub presets: Vec<Preset>,
}

/// Amendment A31: the daily check against GitHub's releases. Only the automatic check
/// is switchable; the About screen can always ask by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    pub check: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self { check: true }
    }
}

/// The language being dictated. A preset says how the text should come out; this says
/// what is being said. Two are configured -- the one usually spoken and one more -- and
/// the tray switches between them, so changing language never means changing preset.
/// Amendment A29.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Languages {
    /// ISO 639-1 code, as the recogniser takes it.
    pub main: String,
    /// The other language the tray can switch to. Empty means no switch is offered.
    pub secondary: String,
    /// Which of the two is in use right now. Persisted, so it survives a restart.
    pub active: String,
}

impl Default for Languages {
    fn default() -> Self {
        Self {
            main: "en".into(),
            secondary: "pl".into(),
            active: "en".into(),
        }
    }
}

impl Languages {
    /// The language to recognise and clean in. Falls back to the main one if the active
    /// code is not one of the two, which a hand-edit can produce.
    pub fn current(&self) -> &str {
        if self.active == self.secondary && !self.secondary.is_empty() {
            &self.secondary
        } else {
            &self.main
        }
    }

    /// The language the tray offers to switch to, if there is one.
    pub fn other(&self) -> Option<&str> {
        if self.secondary.is_empty() || self.secondary == self.main {
            return None;
        }
        Some(if self.current() == self.main { &self.secondary } else { &self.main })
    }
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
    /// Negative means automatic: prefer a discrete card over an integrated one rather
    /// than trusting ggml's enumeration order. An index that is not present falls back
    /// to automatic.
    pub gpu_device: i32,
    pub threads: i32,
    /// Brief 4.4: free VRAM after this long idle. Zero disables unloading.
    pub idle_unload_secs: u64,
    /// On by default since the idle unload was found to be the cause of permanently
    /// slow sessions: every reload is a fresh allocation, and one made while other
    /// applications hold the card puts the weights in system memory for good.
    pub keep_loaded: bool,
    /// A cloud model in place of each local one. The local file stays chosen beside
    /// it: cleanup falls back to it when the cloud fails, and speech may.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whisper_cloud: Option<CloudChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_cloud: Option<CloudChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_multilingual_cloud: Option<CloudChoice>,
    /// Use the local speech model when the cloud one fails. Off by default, as the
    /// external endpoint's was: brief section 10 wants a broken setup to fail loudly,
    /// and speech is the one stage whose fallback changes what was heard.
    pub speech_cloud_fallback: bool,
}

/// A cloud provider: anything that speaks the OpenAI API shape -- OpenRouter, OpenAI,
/// DeepSeek, Groq, a local LM Studio or Ollama -- or Anthropic's own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    /// Fixed when the provider is added and never shown. The key is filed under it,
    /// so renaming a provider keeps its key.
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Which request shape the address speaks. Absent in configs written before
    /// Anthropic was offered, all of which were OpenAI-compatible.
    #[serde(default)]
    pub api: Api,
    #[serde(default)]
    pub models: Vec<CloudModel>,
    #[serde(default = "provider_timeout")]
    pub timeout_secs: u64,
}

impl Provider {
    /// The name to show and log. A provider saved before it was given one still has
    /// to be told apart from the others, so its address stands in.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let host = self
            .base_url
            .trim()
            .split("://")
            .last()
            .unwrap_or("")
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if host.is_empty() {
            "the unnamed provider".to_string()
        } else {
            host.to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    /// `/chat/completions` with a Bearer key: nearly every provider.
    #[default]
    OpenAi,
    /// `/v1/messages` with an `x-api-key` header.
    Anthropic,
}

fn provider_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloudModel {
    /// Exactly as the provider names it, e.g. "google/gemma-3-27b-it".
    pub name: String,
    pub kind: CloudKind,
    /// The provider's own name for it, when its model list gave one ("Google: Gemma 3
    /// 27B"). Only for showing; requests go by `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Which endpoint a model answers on: speech goes to `/audio/transcriptions`,
/// cleanup to `/chat/completions`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloudKind {
    Speech,
    Cleanup,
}

/// A model slot pointing at the cloud.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudChoice {
    pub provider: String,
    pub model: String,
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

impl RemoteAsr {
    /// Nothing the user set: no endpoint, no key, not switched on.
    pub fn is_unset(&self) -> bool {
        !self.enabled && self.base_url.trim().is_empty() && self.api_key.trim().is_empty()
    }
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
    /// Amendment A33: an opt-in rewrite through the instruction model. Off on every
    /// default preset; a preset that turns it on skips S1-mini and needs the
    /// multilingual model, which is the only one that can be asked.
    #[serde(default)]
    pub rewrite: Rewrite,
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
            // Q8_0. Q4_K_M was observed dropping a clause outright (amendment A23); Q8_0
            // reproduces F16 on 94-95% of dictations with no content loss found, at half
            // the size and twice the decode speed. Half the size also matters for fitting
            // beside the speech model on an 8 GB card. Amendment A27.
            cleanup: "s1-mini-q8_0.gguf".into(),
            // Gemma 4 E2B over Gemma 3 4B: fewer errors and content losses on Polish,
            // same speed, and only ~1 GB of it lives in graphics memory. Amendment A28.
            cleanup_multilingual: "gemma-4-E2B_q4_0-it.gguf".into(),
            gpu_device: -1,
            threads: std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(8),
            idle_unload_secs: 15 * 60,
            keep_loaded: true,
            whisper_cloud: None,
            cleanup_cloud: None,
            cleanup_multilingual_cloud: None,
            speech_cloud_fallback: false,
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

/// The dictate and paste-raw bindings a fresh install gets. Amendment A30: each
/// platform gets the chord that is free there, since Ctrl+Space is the input-source
/// switch on macOS, and on Linux the hotkey is not swallowed, so a chord an editor
/// also uses would fire in the editor on every dictation.
#[cfg(windows)]
pub const DEFAULT_HOTKEY: (&str, &str) = ("Ctrl+Space", "Ctrl+Shift+Space");
#[cfg(target_os = "macos")]
pub const DEFAULT_HOTKEY: (&str, &str) = ("Alt+Space", "Alt+Shift+Space");
#[cfg(target_os = "linux")]
pub const DEFAULT_HOTKEY: (&str, &str) = ("Ctrl+Alt+Space", "Ctrl+Alt+Shift+Space");

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.0.into(),
            paste_raw_hotkey: DEFAULT_HOTKEY.1.into(),
            hotkey_mode: crate::hotkey::Mode::Auto,
            tap_threshold_ms: 400,
            max_record_secs: 300,
            active_preset: "Prompt".into(),
            languages: Languages::default(),
            audio: Audio::default(),
            models: Models::default(),
            providers: Vec::new(),
            remote_asr: RemoteAsr::default(),
            cues: Cues::default(),
            output: Output::default(),
            vocabulary: crate::vocabulary::Vocabulary::default(),
            history: HistoryConfig::default(),
            updates: Updates::default(),
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
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
            rewrite: Rewrite::Off,
        },
        Preset {
            name: "Message".into(),
            styling: Styling::SemiCasual,
            structure: Structure::Prose,
            context: Context::General,
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
            rewrite: Rewrite::Off,
        },
        Preset {
            name: "Email".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::Email,
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
            rewrite: Rewrite::Off,
        },
        Preset {
            name: "Notes".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Lists,
            context: Context::General,
            cleanup: true,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
            rewrite: Rewrite::Off,
        },
        Preset {
            name: "Raw".into(),
            styling: Styling::SemiFormal,
            structure: Structure::Prose,
            context: Context::General,
            cleanup: false,
            auto_paste: true,
            vocabulary_sets: vec![],
            replacements: crate::vocabulary::default_replacements(),
            hotkey: None,
            rewrite: Rewrite::Off,
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
        let config = Self::load_migrating(&path)?;
        Ok((config, path, false))
    }

    /// `load`, then the one-time move of the old external endpoint to Providers. The
    /// config is written back only when something moved; a move that fails leaves the
    /// file as it was, key included, and says why in the log.
    pub fn load_migrating(path: &Path) -> Result<Self> {
        let mut config = Self::load(path)?;
        if !config.remote_asr.is_unset() {
            match config.migrate_remote_asr(&crate::secrets::OsKeyStore) {
                Ok(()) => {
                    std::fs::write(path, config.to_toml()?)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!(
                        "providers: the external endpoint is now a provider, and its key \
                         is in the system credential store"
                    );
                }
                Err(e) => eprintln!(
                    "providers: could not move the external endpoint's key to the system \
                     credential store, so it is left where it was and not used: {e:#}"
                ),
            }
        }
        Ok(config)
    }

    /// The old `[remote_asr]` settings as a provider, its key in `store`. The key is
    /// only removed from the config once the store has given it back intact.
    pub fn migrate_remote_asr(&mut self, store: &dyn crate::secrets::KeyStore) -> Result<()> {
        let old = self.remote_asr.clone();
        let mut id = "endpoint".to_string();
        let mut n = 2;
        while self.provider(&id).is_some() {
            id = format!("endpoint-{n}");
            n += 1;
        }
        let key = old.api_key.trim();
        if !key.is_empty() {
            crate::secrets::set_verified(store, &id, key)?;
        }
        self.providers.push(Provider {
            id: id.clone(),
            name: "External endpoint".into(),
            base_url: old.base_url.trim().to_string(),
            api: Api::OpenAi,
            models: vec![CloudModel { name: old.model.clone(), kind: CloudKind::Speech, label: None }],
            timeout_secs: old.timeout_secs,
        });
        if old.enabled {
            self.models.whisper_cloud = Some(CloudChoice { provider: id, model: old.model });
        }
        self.models.speech_cloud_fallback = old.fallback_to_local;
        self.remote_asr = RemoteAsr { enabled: false, base_url: String::new(), api_key: String::new(), ..RemoteAsr::default() };
        Ok(())
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    fn resolve(&self, choice: &CloudChoice, what: &str) -> Result<(&Provider, String)> {
        let provider = self.provider(&choice.provider).with_context(|| {
            format!("{what} is set to a model at a provider that is no longer in Providers")
        })?;
        Ok((provider, choice.model.clone()))
    }

    /// The cloud model recognising speech, if one is picked.
    pub fn cloud_speech(&self) -> Result<Option<(&Provider, String)>> {
        self.models
            .whisper_cloud
            .as_ref()
            .map(|c| self.resolve(c, "Speech"))
            .transpose()
    }

    /// The cloud model cleaning this dictation, if one is picked for its slot. English
    /// without a rewrite is the English slot; every other language, and every rewrite,
    /// is the other one, the same split the local models follow.
    pub fn cloud_cleanup(&self, preset: &Preset) -> Result<Option<(&Provider, String)>> {
        let choice = if self.cloud_cleanup_is_english(preset) {
            &self.models.cleanup_cloud
        } else {
            &self.models.cleanup_multilingual_cloud
        };
        choice.as_ref().map(|c| self.resolve(c, "Cleanup")).transpose()
    }

    fn cloud_cleanup_is_english(&self, preset: &Preset) -> bool {
        self.languages.current().eq_ignore_ascii_case("en") && preset.rewrite == Rewrite::Off
    }

    /// Whether a cloud model cleans this dictation. Then no local cleanup model is
    /// loaded for it, in either slot: one is loaded only if the cloud fails.
    pub fn cloud_replaces_local_cleanup(&self, preset: &Preset) -> bool {
        preset.cleanup
            && if self.cloud_cleanup_is_english(preset) {
                self.models.cleanup_cloud.is_some()
            } else {
                self.models.cleanup_multilingual_cloud.is_some()
            }
    }

    /// Whether the local speech model has to be loaded before a dictation: not when a
    /// cloud model recognises speech. With the fallback on, it is loaded if the cloud
    /// fails, not before.
    pub fn needs_local_speech(&self) -> bool {
        self.models.whisper_cloud.is_none()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
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

    /// Whether English is cleaned by a general instruction model rather than S1-mini,
    /// because one was picked for the English slot. Read off the file name: every
    /// S1-mini build this app knows of carries it, and S1-mini's prompt contract is
    /// fixed, so anything else has to be told what cleaning means.
    pub fn english_uses_instruction_model(&self) -> bool {
        !self.models.cleanup.to_lowercase().contains("s1-mini")
    }

    /// The instruction model a dictation in the current language would use: the one
    /// picked for English when English has one, the multilingual one otherwise.
    pub fn instruction_model_path(&self) -> PathBuf {
        if self.languages.current().eq_ignore_ascii_case("en") && self.english_uses_instruction_model() {
            self.cleanup_path()
        } else {
            self.cleanup_multilingual_path()
        }
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
            let result = Config::load_migrating(&path).map_err(|e| format!("{e:#}"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{tests::MemoryStore, KeyStore};

    fn old_endpoint(key: &str) -> Config {
        let mut c = Config::default();
        c.remote_asr = RemoteAsr {
            enabled: true,
            base_url: "https://api.example.com/v1".into(),
            model: "whisper-large".into(),
            api_key: key.into(),
            timeout_secs: 90,
            fallback_to_local: true,
        };
        c
    }

    #[test]
    fn the_old_endpoint_becomes_a_provider_and_its_key_leaves_the_file() {
        let store = MemoryStore::default();
        let mut c = old_endpoint("sk-0123456789abcdef");
        c.migrate_remote_asr(&store).unwrap();

        assert_eq!(store.get("endpoint").unwrap().as_deref(), Some("sk-0123456789abcdef"));
        let toml = c.to_toml().unwrap();
        assert!(!toml.contains("sk-0123456789abcdef"), "{toml}");
        assert!(!toml.contains("remote_asr"));

        let p = c.provider("endpoint").unwrap();
        assert_eq!(p.base_url, "https://api.example.com/v1");
        assert_eq!(p.timeout_secs, 90);
        assert_eq!(p.models, vec![CloudModel { name: "whisper-large".into(), kind: CloudKind::Speech, label: None }]);
        assert_eq!(
            c.models.whisper_cloud,
            Some(CloudChoice { provider: "endpoint".into(), model: "whisper-large".into() })
        );
        assert!(c.models.speech_cloud_fallback);

        // And it survives the round trip through the file.
        let back: Config = toml::from_str(&toml).unwrap();
        assert!(back.remote_asr.is_unset());
        assert_eq!(back.providers, c.providers);
        assert_eq!(back.models.whisper_cloud, c.models.whisper_cloud);
    }

    #[test]
    fn a_failed_move_keeps_the_key_in_the_file() {
        for store in [
            MemoryStore { broken: true, ..Default::default() },
            MemoryStore { forgetful: true, ..Default::default() },
        ] {
            let mut c = old_endpoint("sk-0123456789abcdef");
            assert!(c.migrate_remote_asr(&store).is_err());
            assert!(c.providers.is_empty());
            // Written back as it was: nothing lost.
            assert!(c.to_toml().unwrap().contains("sk-0123456789abcdef"));
        }
    }

    #[test]
    fn an_endpoint_without_a_key_moves_without_the_store() {
        let store = MemoryStore { broken: true, ..Default::default() };
        let mut c = old_endpoint("");
        c.remote_asr.enabled = false;
        c.migrate_remote_asr(&store).unwrap();
        assert_eq!(c.providers.len(), 1);
        assert_eq!(c.models.whisper_cloud, None);
    }

    #[test]
    fn an_untouched_config_has_nothing_to_move_and_nothing_cloud() {
        let c = Config::default();
        assert!(c.remote_asr.is_unset());
        let toml = c.to_toml().unwrap();
        assert!(!toml.contains("remote_asr"));
        assert!(!toml.contains("_cloud ="));
        assert!(c.cloud_speech().unwrap().is_none());
        assert!(c.cloud_cleanup(c.active()).unwrap().is_none());
    }

    #[test]
    fn rewrites_and_other_languages_use_the_other_cleanup_slot() {
        let mut c = Config::default();
        c.providers.push(Provider {
            id: "or".into(),
            name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api: Api::OpenAi,
            models: vec![],
            timeout_secs: 120,
        });
        let mut preset = c.active().clone();
        assert!(!c.cloud_replaces_local_cleanup(&preset));
        c.models.cleanup_cloud = Some(CloudChoice { provider: "or".into(), model: "en-model".into() });
        assert_eq!(c.cloud_cleanup(&preset).unwrap().unwrap().1, "en-model");
        // English too: no S1-mini beside a cloud model.
        assert!(c.cloud_replaces_local_cleanup(&preset));
        preset.rewrite = Rewrite::Prompt;
        assert!(!c.cloud_replaces_local_cleanup(&preset));
        c.models.cleanup_multilingual_cloud =
            Some(CloudChoice { provider: "or".into(), model: "other-model".into() });
        assert_eq!(c.cloud_cleanup(&preset).unwrap().unwrap().1, "other-model");
        assert!(c.cloud_replaces_local_cleanup(&preset));
        // Raw is never cleaned, so nothing replaces anything.
        preset.cleanup = false;
        assert!(!c.cloud_replaces_local_cleanup(&preset));
        preset.cleanup = true;
        preset.rewrite = Rewrite::Off;
        c.languages.secondary = "pl".into();
        c.languages.active = "pl".into();
        assert_eq!(c.cloud_cleanup(&preset).unwrap().unwrap().1, "other-model");
        // A provider that was removed is an error, not a silent switch to local.
        c.providers.clear();
        assert!(c.cloud_cleanup(&preset).is_err());
    }

    #[test]
    fn a_provider_from_before_anthropic_loads_as_openai_and_always_has_a_name() {
        let old = r#"
            [[providers]]
            id = "p-1"
            name = ""
            base_url = ""
            models = [{ name = "gpt-4o-mini", kind = "cleanup" }]
        "#;
        let c: Config = toml::from_str(old).unwrap();
        let p = &c.providers[0];
        assert_eq!(p.api, Api::OpenAi);
        assert_eq!(p.models[0].label, None);
        assert_eq!(p.display_name(), "the unnamed provider");
        let mut p = p.clone();
        p.base_url = "https://api.deepseek.com/v1".into();
        assert_eq!(p.display_name(), "api.deepseek.com");
        p.name = " DeepSeek ".into();
        assert_eq!(p.display_name(), "DeepSeek");
        // No label, nothing written for it.
        let toml = Config { providers: vec![p], ..Config::default() }.to_toml().unwrap();
        assert!(!toml.contains("label"), "{toml}");
        assert!(toml.contains("api = \"openai\""), "{toml}");
    }

    #[test]
    fn a_cloud_speech_model_means_no_local_one_up_front() {
        let mut c = Config::default();
        assert!(c.needs_local_speech());
        c.models.whisper_cloud = Some(CloudChoice { provider: "or".into(), model: "whisper-1".into() });
        assert!(!c.needs_local_speech());
        // The fallback loads it when the cloud fails, not before.
        c.models.speech_cloud_fallback = true;
        assert!(!c.needs_local_speech());
    }

    #[test]
    fn the_tray_offers_the_language_not_in_use() {
        let mut l = Languages { main: "en".into(), secondary: "pl".into(), active: "en".into() };
        assert_eq!(l.current(), "en");
        assert_eq!(l.other(), Some("pl"));
        l.active = "pl".into();
        assert_eq!(l.current(), "pl");
        assert_eq!(l.other(), Some("en"));
        // A code that is neither falls back to the main language.
        l.active = "xx".into();
        assert_eq!(l.current(), "en");
        // No secondary, no switch.
        l.secondary.clear();
        assert_eq!(l.other(), None);
    }
}
