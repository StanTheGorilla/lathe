// Model lifecycle and the dictation pipeline, brief section 4.4.

use anyhow::{Context as _, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use std::time::{Duration, Instant};

use crate::asr::Asr;
use crate::cleanup::Cleanup;
use crate::config::{Config, Preset};

pub struct Outcome {
    pub raw: String,
    pub cleaned: String,
    pub preset: String,
    pub audio_secs: f32,
    pub asr_ms: u128,
    pub cleanup_ms: u128,
    pub vad_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// VAD found no speech worth transcribing. Brief 6.1.
    NoSpeech,
    /// Whisper returned nothing.
    EmptyTranscript,
}

pub enum Processed {
    Done(Box<Outcome>),
    Rejected(Rejected),
}

pub struct Engine {
    /// Initialised on first use, not at startup. `llama_backend_init` loads the
    /// Vulkan backend and touches the driver, which costs about 12MB of working set
    /// that brief section 3 does not want to pay while idle.
    backend: Option<LlamaBackend>,
    asr: Option<Asr>,
    cleanup: Option<Cleanup>,
    /// Loaded only when a non-English preset needs it. See amendment A21.
    cleanup_multilingual: Option<Cleanup>,
    /// Set once the optional multilingual model has been looked for and was not there.
    /// Without it every non-English dictation would retry the load, and pay the warmup
    /// behind it, for a file that is still absent.
    cleanup_multilingual_missing: bool,
    /// Resolved once per process. Enumerating adapters initialises the Vulkan backend,
    /// and the answer cannot change while the app is running.
    gpu: Option<crate::asr::Gpu>,
    last_used: Instant,
}

impl Engine {
    pub fn new() -> Result<Self> {
        // Nothing is loaded and no GPU work happens here; brief 4.4 defers all of it to
        // the first hotkey press.
        Ok(Self {
            backend: None,
            asr: None,
            cleanup: None,
            cleanup_multilingual: None,
            cleanup_multilingual_missing: false,
            gpu: None,
            last_used: Instant::now(),
        })
    }

    /// The device index and layer count to load a cleanup model with.
    ///
    /// Zero layers when the chosen device is the CPU: offloading 999 layers to a device
    /// that cannot take them is how a machine with no usable GPU fails today.
    fn gpu(slot: &mut Option<crate::asr::Gpu>, requested: i32) -> (i32, u32) {
        let gpu = slot.get_or_insert_with(|| {
            let gpu = crate::asr::resolve_gpu(requested);
            eprintln!(
                "cleanup model on device {} '{}' ({})",
                gpu.device,
                gpu.name,
                if gpu.offload { "offloading" } else { "cpu only" }
            );
            gpu
        });
        (gpu.device, if gpu.offload { 999 } else { 0 })
    }

    /// Whether everything *this language* needs is resident.
    ///
    /// Amendment A21 loads one cleanup model per language, so "loaded" is meaningless
    /// without one: a session that has dictated in English has the speech model and
    /// S1-mini, and still needs Gemma before it can clean a word of Polish.
    pub fn loaded(&self, language: &str) -> bool {
        if self.asr.is_none() {
            return false;
        }
        if language.eq_ignore_ascii_case("en") {
            self.cleanup.is_some()
        } else {
            self.cleanup_multilingual.is_some() || self.cleanup_multilingual_missing
        }
    }

    /// Loads both models if they are not resident, then forces Vulkan shader
    /// compilation before reporting ready.
    ///
    /// Phase 1 measured roughly 3 seconds of shader compilation on the first inference
    /// after process start, separate from model load. Without this warmup that cost
    /// lands inside the user's first dictation, while brief 4.4 has already told them
    /// loading is finished.
    /// `language` is the preset's code, so only the cleanup model that language needs
    /// gets loaded. Loading both would cost 2.3GB of VRAM and several seconds for a user
    /// who never dictates in the other one.
    pub fn ensure_loaded(
        &mut self,
        config: &Config,
        language: &str,
        progress: &dyn Fn(&str),
    ) -> Result<()> {
        let english = language.eq_ignore_ascii_case("en");
        if self.loaded(language) {
            self.last_used = Instant::now();
            return Ok(());
        }

        if self.asr.is_none() {
            progress("Loading speech model");
            let (asr, ms) = Asr::load(&config.whisper_path(), config.models.threads)?;
            eprintln!("speech model loaded in {ms}ms on backend '{}'", asr.backend());
            self.asr = Some(asr);
        }

        if english && self.cleanup.is_none() {
            progress("Loading cleanup model");
            let (device, layers) = Self::gpu(&mut self.gpu, config.models.gpu_device);
            let backend = Self::backend_mut(&mut self.backend)?;
            let (cleanup, ms) = Cleanup::load(
                backend,
                &config.cleanup_path(),
                device,
                layers,
                crate::cleanup::Flavour::S1Mini,
            )?;
            eprintln!("s1-mini loaded in {ms}ms");
            self.cleanup = Some(cleanup);
        }

        if !english && self.cleanup_multilingual.is_none() {
            let path = config.cleanup_multilingual_path();
            if path.exists() {
                progress("Loading multilingual cleanup model");
                let (device, layers) = Self::gpu(&mut self.gpu, config.models.gpu_device);
                let backend = Self::backend_mut(&mut self.backend)?;
                let (cleanup, ms) = Cleanup::load(
                    backend,
                    &path,
                    device,
                    layers,
                    crate::cleanup::Flavour::Instruct,
                )?;
                eprintln!("multilingual cleanup model loaded in {ms}ms");
                self.cleanup_multilingual = Some(cleanup);
                self.cleanup_multilingual_missing = false;
            } else {
                // Not fatal. Recognition still works in this language; only the tidying
                // is missing, and the model is an optional download.
                eprintln!(
                    "multilingual cleanup model not present at {}; \
                     non-English dictation will be pasted uncleaned",
                    path.display()
                );
                self.cleanup_multilingual_missing = true;
            }
        }

        progress("Compiling shaders");
        self.warmup(config, english)?;
        self.last_used = Instant::now();
        Ok(())
    }

    fn backend_mut(slot: &mut Option<LlamaBackend>) -> Result<&LlamaBackend> {
        match slot {
            Some(backend) => Ok(backend),
            None => {
                let backend = LlamaBackend::init().context("initialising the llama backend")?;
                Ok(slot.insert(backend))
            }
        }
    }

    fn warmup(&mut self, config: &Config, english: bool) -> Result<()> {
        let started = Instant::now();
        if let Some(asr) = &self.asr {
            let silence = vec![0.0f32; crate::audio::TARGET_RATE as usize];
            let _ = asr.transcribe(&silence, "en");
        }
        // Only the model this dictation will actually use.
        let cleanup = if english {
            self.cleanup.as_ref()
        } else {
            self.cleanup_multilingual.as_ref()
        };
        if let (Some(cleanup), Some(backend)) = (cleanup, &self.backend) {
            let _ = cleanup.normalize(
                backend,
                "warm up",
                crate::cleanup::Styling::SemiFormal,
                crate::cleanup::Structure::Prose,
                crate::cleanup::Context::General,
                config.models.threads,
                "English",
            );
        }
        eprintln!("warmup took {}ms", started.elapsed().as_millis());
        Ok(())
    }

    /// Brief 4.4: free VRAM after the idle timeout. Returns true if anything was freed.
    pub fn unload_if_idle(&mut self, config: &Config) -> bool {
        if config.models.keep_loaded || config.models.idle_unload_secs == 0 {
            return false;
        }
        if self.asr.is_none() && self.cleanup.is_none() && self.cleanup_multilingual.is_none() {
            return false;
        }
        if self.last_used.elapsed() < Duration::from_secs(config.models.idle_unload_secs) {
            return false;
        }
        self.asr = None;
        self.cleanup = None;
        self.cleanup_multilingual = None;
        // Looked for again next time: the download may have finished in the meantime.
        self.cleanup_multilingual_missing = false;
        eprintln!("models unloaded after idle timeout");
        true
    }

    pub fn process(&mut self, config: &Config, preset: &Preset, pcm: &[f32]) -> Result<Processed> {
        self.last_used = Instant::now();

        let asr = self.asr.as_ref().context("speech model is not loaded")?;

        // Brief 6.1: never call the model on silence.
        let Some(gated) = asr.gate(pcm, &config.vad_path(), config.audio.min_speech_ms)? else {
            return Ok(Processed::Rejected(Rejected::NoSpeech));
        };

        // Brief 5.5 pass 1: bias the recogniser toward the active vocabulary before it
        // decodes. Capped at 128 terms; biasing toward a list longer than the utterance
        // stops helping and starts dragging unrelated words toward it.
        let vocabulary = config.vocabulary.for_preset(&preset.vocabulary_sets);
        asr.set_hotwords(&vocabulary.hotwords(128), config.vocabulary.hotword_boost);

        // Brief 4.3: the external endpoint, when configured, replaces local recognition.
        // The VAD gate above still runs locally, so silence is never uploaded.
        let (text, asr_ms) = if config.remote_asr.enabled {
            let started = Instant::now();
            match remote_transcribe(config, &gated.pcm, &preset.lang) {
                Ok(text) => (text, started.elapsed().as_millis()),
                Err(e) if config.remote_asr.fallback_to_local => {
                    eprintln!("remote transcription failed, falling back to local: {e:#}");
                    let t = asr.transcribe(&gated.pcm, &preset.lang)?;
                    (t.text, t.infer_ms)
                }
                // Brief section 10: fail loudly rather than silently downgrading.
                Err(e) => return Err(e),
            }
        } else {
            let t = asr.transcribe(&gated.pcm, &preset.lang)?;
            (t.text, t.infer_ms)
        };

        // Brief 5.5 pass 2: fix what biasing did not, before the cleanup model sees it.
        // Order matters -- correcting a proper noun after cleanup would mean the
        // normaliser had already reasoned about a word that was wrong.
        let (text, corrections) = vocabulary.correct(&text);
        if corrections > 0 {
            eprintln!("vocabulary: {corrections} correction(s)");
        }

        let transcript = crate::asr::Transcript {
            text,
            infer_ms: asr_ms,
            audio_secs: gated.pcm.len() as f32 / crate::audio::TARGET_RATE as f32,
        };

        if transcript.text.is_empty() {
            return Ok(Processed::Rejected(Rejected::EmptyTranscript));
        }

        // Brief 4.2 restricted cleanup to English because S1-mini is English only. It
        // still is; amendment A21 adds a second, multilingual model for everything else,
        // so a non-English preset is cleaned rather than passed through raw.
        let (cleaned, cleanup_ms) = if preset.cleanup {
            let english = preset.lang.eq_ignore_ascii_case("en");
            let model = if english {
                self.cleanup.as_ref()
            } else {
                self.cleanup_multilingual.as_ref()
            };

            match model {
                Some(model) => {
                    let backend = self
                        .backend
                        .as_ref()
                        .context("llama backend is not initialised")?;
                    let result = model.normalize(
                        backend,
                        &transcript.text,
                        preset.styling,
                        preset.structure,
                        preset.context,
                        config.models.threads,
                        language_name(&preset.lang),
                    )?;
                    (result.text, result.infer_ms)
                }
                // The multilingual model is optional: it is a 2.3GB download and a
                // user who only dictates English never needs it. Say so once per
                // dictation rather than failing, since the raw transcript is still good.
                None => {
                    eprintln!(
                        "no cleanup model for '{}'; pasting the transcript uncleaned",
                        preset.lang
                    );
                    (transcript.text.clone(), 0)
                }
            }
        } else {
            (transcript.text.clone(), 0)
        };

        // Brief 6.8: deterministic find/replace, applied last so it has the final say
        // over anything the model produced.
        let cleaned = crate::vocabulary::apply_replacements(&cleaned, &preset.replacements);

        self.last_used = Instant::now();
        Ok(Processed::Done(Box::new(Outcome {
            raw: transcript.text,
            cleaned,
            preset: preset.name.clone(),
            audio_secs: transcript.audio_secs,
            asr_ms: transcript.infer_ms,
            cleanup_ms,
            vad_ms: gated.vad_ms,
        })))
    }
}


/// Brief 4.3. Builds the backend per call rather than holding it: it owns no model and
/// no connection, so there is nothing to keep warm, and reading the config each time
/// means an endpoint change takes effect on the next dictation like every other setting.
fn remote_transcribe(config: &Config, pcm: &[f32], lang: &str) -> Result<String> {
    use crate::asr_backend::{AsrBackend, OpenAiCompatBackend};

    let remote = &config.remote_asr;
    if remote.base_url.trim().is_empty() {
        anyhow::bail!("the external transcription endpoint is enabled but has no URL");
    }

    let backend = OpenAiCompatBackend::new(
        &remote.base_url,
        &remote.model,
        Some(remote.api_key.clone()),
        remote.timeout_secs,
    );
    backend.transcribe(pcm, lang, &[])
}

/// The English name of a language code, for the instruction prompt.
///
/// Naming the language in the prompt matters: a general model handed Polish text under
/// an English instruction will sometimes answer in English, and that failure looks like
/// an unwanted translation feature rather than a bug.
pub fn language_name(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "en" => "English",
        "pl" => "Polish",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "cs" => "Czech",
        "uk" => "Ukrainian",
        "ru" => "Russian",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" | "nb" => "Norwegian",
        "fi" => "Finnish",
        "tr" => "Turkish",
        "ja" => "Japanese",
        "ko" => "Korean",
        "zh" => "Chinese",
        "ar" => "Arabic",
        "hi" => "Hindi",
        _ => "the same language as the transcript",
    }
}
