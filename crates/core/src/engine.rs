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
    /// The language it was recognised in, for the history.
    pub language: String,
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

/// The local models a dictation needs resident before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Needs {
    speech: bool,
    s1mini: bool,
    /// The instruction model: another language, a rewrite, a general model picked for
    /// English, or the English vocabulary judge.
    instruct: bool,
}

pub struct Engine {
    /// Initialised on first use, not at startup. `llama_backend_init` loads the
    /// Vulkan backend and touches the driver, which costs about 12MB of working set
    /// that brief section 3 does not want to pay while idle.
    backend: Option<LlamaBackend>,
    asr: Option<Asr>,
    cleanup: Option<Cleanup>,
    /// Loaded only when a non-English preset needs it. See amendment A21. Also serves
    /// English when a general model was picked for English cleanup.
    cleanup_multilingual: Option<Cleanup>,
    /// Which file `cleanup_multilingual` was loaded from. English and Polish may name
    /// different instruction models, and switching must not keep the wrong one.
    cleanup_multilingual_path: Option<std::path::PathBuf>,
    /// The instruction model file last looked for and not found.
    /// Without it every non-English dictation would retry the load, and pay the warmup
    /// behind it, for a file that is still absent.
    cleanup_multilingual_missing: Option<std::path::PathBuf>,
    /// Resolved once per process. Enumerating adapters initialises the Vulkan backend,
    /// and the answer cannot change while the app is running.
    gpu: Option<crate::asr::Gpu>,
    /// The warning from the last load whose weights did not fit the card, until
    /// someone takes it to show the user. The log line alone was found to be invisible.
    spill_warning: Option<String>,
    /// Why the last dictation's cloud model fell back to the local one, until someone
    /// takes it to show the user. Key already blanked out.
    cloud_warning: Option<String>,
    /// The last cloud cleanup failed and a local model was loaded in its place. Kept
    /// resident until the cloud answers again, so a provider that is down costs one
    /// load rather than one per dictation.
    cloud_failing: bool,
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
            cleanup_multilingual_path: None,
            cleanup_multilingual_missing: None,
            gpu: None,
            spill_warning: None,
            cloud_warning: None,
            cloud_failing: false,
            last_used: Instant::now(),
        })
    }

    /// The last load's "will not fit" warning, once. `None` when it fitted.
    pub fn take_spill_warning(&mut self) -> Option<String> {
        self.spill_warning.take()
    }

    pub fn take_cloud_warning(&mut self) -> Option<String> {
        self.cloud_warning.take()
    }

    fn warn(&mut self, message: String) {
        self.cloud_warning = Some(match self.cloud_warning.take() {
            Some(earlier) => format!("{earlier}\n{message}"),
            None => message,
        });
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

    /// Whether this dictation goes through the instruction model rather than S1-mini:
    /// any language S1-mini does not cover (amendment A21), and any preset that asks
    /// for a rewrite (amendment A33), which only an instruction model can be asked for.
    pub fn wants_instruct(config: &Config, preset: &Preset) -> bool {
        !config.languages.current().eq_ignore_ascii_case("en")
            || (preset.cleanup && preset.rewrite != crate::cleanup::Rewrite::Off)
            || config.english_uses_instruction_model()
    }

    /// Whether the resident instruction model is the file this dictation wants.
    fn instruct_is(&self, config: &Config) -> bool {
        self.cleanup_multilingual_path.as_deref() == Some(config.instruction_model_path().as_path())
    }

    /// Whether an English dictation also keeps the instruction model resident, to weigh
    /// the vocabulary's questions with it rather than with S1-mini.
    fn judges_with_instruct(config: &Config, preset: &Preset) -> bool {
        !Self::wants_instruct(config, preset)
            && config.vocabulary.context
            && config.vocabulary.context_with_instruction_model
    }

    /// The local models this dictation needs resident before it starts. A cloud model
    /// in a slot means nothing local for it: the local one is loaded only if the cloud
    /// fails, and speech detection needs no model of the recogniser's.
    fn needs(config: &Config, preset: &Preset) -> Needs {
        let speech = config.needs_local_speech();
        if config.cloud_replaces_local_cleanup(preset) {
            return Needs { speech, s1mini: false, instruct: false };
        }
        let english = !Self::wants_instruct(config, preset);
        Needs {
            speech,
            s1mini: english,
            instruct: !english || Self::judges_with_instruct(config, preset),
        }
    }

    /// Local models resident that a cloud model has taken over from, and which only
    /// hold graphics memory. A cleanup model loaded because the cloud failed stays
    /// until the cloud answers again; a speech model stays while it is the fallback.
    fn surplus(&self, config: &Config, preset: &Preset) -> bool {
        let cleanup = config.cloud_replaces_local_cleanup(preset)
            && !self.cloud_failing
            && (self.cleanup.is_some() || self.cleanup_multilingual.is_some());
        let speech = !config.needs_local_speech()
            && !config.models.speech_cloud_fallback
            && self.asr.is_some();
        cleanup || speech
    }

    /// Whether everything *this dictation* needs is resident, and nothing a cloud model
    /// replaced is still holding memory.
    ///
    /// Amendment A21 loads one cleanup model per language, so "loaded" is meaningless
    /// without one: a session that has dictated in English has the speech model and
    /// S1-mini, and still needs Gemma before it can clean a word of Polish.
    pub fn loaded(&self, config: &Config, preset: &Preset) -> bool {
        let needs = Self::needs(config, preset);
        if needs.speech && self.asr.is_none() {
            return false;
        }
        if self.surplus(config, preset) {
            return false;
        }
        let instruct = (self.cleanup_multilingual.is_some() && self.instruct_is(config))
            || self.cleanup_multilingual_missing.as_deref() == Some(config.instruction_model_path().as_path());
        if needs.instruct && !instruct {
            return false;
        }
        !needs.s1mini || self.cleanup.is_some()
    }

    /// Loads the models this dictation needs if they are not resident, then forces
    /// Vulkan shader compilation before reporting ready.
    ///
    /// Phase 1 measured roughly 3 seconds of shader compilation on the first inference
    /// after process start, separate from model load. Without this warmup that cost
    /// lands inside the user's first dictation, while brief 4.4 has already told them
    /// loading is finished.
    /// Only the cleanup model this dictation needs gets loaded. Loading both would
    /// cost 2.3GB of VRAM and several seconds for a user who never needs the other one.
    pub fn ensure_loaded(
        &mut self,
        config: &Config,
        preset: &Preset,
        progress: &dyn Fn(&str),
    ) -> Result<()> {
        let language = config.languages.current();
        let english = !Self::wants_instruct(config, preset);
        let judge = Self::judges_with_instruct(config, preset);
        if self.loaded(config, preset) {
            self.last_used = Instant::now();
            return Ok(());
        }
        let needs = Self::needs(config, preset);

        if config.cloud_replaces_local_cleanup(preset) {
            // What frees the graphics memory when a slot is switched to the cloud.
            if !self.cloud_failing {
                let s1 = self.cleanup.take().is_some();
                let instruct = self.cleanup_multilingual.take().is_some();
                self.cleanup_multilingual_path = None;
                if s1 || instruct {
                    eprintln!("local cleanup model unloaded: a cloud model cleans this dictation");
                }
            }
        } else {
            // One cleanup model resident at a time. Both together are 3.2 GB beside the
            // speech model, and on an 8 GB card that other applications also use the
            // second one lands in system memory and runs 20-40x slower -- Polish cleanup
            // was taking 35-90 s. Switching language costs a 1-2 s reload instead. The
            // exception is a user who asked for the instruction model to judge
            // vocabulary in English too.
            if english && !judge && self.cleanup_multilingual.take().is_some() {
                eprintln!("instruction model unloaded: switching to S1-mini");
            }
            if !english && self.cleanup.take().is_some() {
                eprintln!("s1-mini unloaded: switching to the instruction model ({language})");
            }
        }
        if !config.needs_local_speech()
            && !config.models.speech_cloud_fallback
            && self.asr.take().is_some()
        {
            eprintln!("speech model unloaded: a cloud model recognises speech");
        }
        // English and another language may each name their own instruction model.
        if self.cleanup_multilingual.is_some() && !self.instruct_is(config) {
            self.cleanup_multilingual = None;
            self.cleanup_multilingual_path = None;
            eprintln!("instruction model unloaded: this language uses a different one");
        }

        let mut pending = Vec::new();
        if needs.speech && self.asr.is_none() {
            pending.push(config.whisper_path());
        }
        if needs.s1mini && self.cleanup.is_none() {
            pending.push(config.cleanup_path());
        }
        if needs.instruct && self.cleanup_multilingual.is_none() {
            pending.push(config.instruction_model_path());
        }
        if pending.is_empty() {
            // Said, so a log that shows no weights loading is not mistaken for a hang,
            // and without asking the card: enumerating it costs working set for nothing.
            eprintln!("graphics memory: no weights to load for this dictation");
            self.last_used = Instant::now();
            return Ok(());
        }
        self.report_vram(config, &pending);

        if needs.speech && self.asr.is_none() {
            progress("Loading speech model");
            self.load_speech(config)?;
        }
        self.load_cleanup(config, needs.s1mini, needs.instruct, judge, progress)?;

        progress("Compiling shaders");
        self.warmup(config, english, judge)?;
        self.last_used = Instant::now();
        Ok(())
    }

    fn load_speech(&mut self, config: &Config) -> Result<()> {
        let (asr, ms) = Asr::load(&config.whisper_path(), config.models.threads)?;
        eprintln!("speech model loaded in {ms}ms on backend '{}'", asr.backend());
        self.asr = Some(asr);
        Ok(())
    }

    /// S1-mini and the instruction model, whichever are asked for and not resident.
    fn load_cleanup(
        &mut self,
        config: &Config,
        s1mini: bool,
        instruct: bool,
        judge: bool,
        progress: &dyn Fn(&str),
    ) -> Result<()> {
        let language = config.languages.current();
        if s1mini && self.cleanup.is_none() {
            progress("Loading cleanup model");
            self.load_s1mini(config)?;
        }

        if instruct && self.cleanup_multilingual.is_none() {
            let path = config.instruction_model_path();
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
                self.cleanup_multilingual_path = Some(path.clone());
                self.cleanup_multilingual_missing = None;
            } else {
                // Not fatal. Recognition still works in this language; only the tidying
                // is missing, and the model is an optional download.
                eprintln!(
                    "multilingual cleanup model not present at {}; \
                     non-English dictation will be pasted uncleaned",
                    path.display()
                );
                self.cleanup_multilingual_missing = Some(path.clone());
                // A rewrite preset in English can still be cleaned the ordinary way,
                // which beats pasting raw speech because one optional file is absent.
                // (A judge that is missing just leaves S1-mini to judge.)
                // Nor when S1-mini is not what English was given: the English file is
                // the one that is missing.
                if !judge
                    && language.eq_ignore_ascii_case("en")
                    && !config.english_uses_instruction_model()
                    && self.cleanup.is_none()
                {
                    eprintln!("the rewrite needs that model; cleaning with S1-mini instead");
                    progress("Loading cleanup model");
                    self.load_s1mini(config)?;
                }
            }
        }
        Ok(())
    }

    fn load_s1mini(&mut self, config: &Config) -> Result<()> {
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
        Ok(())
    }

    /// The local cleanup model for this dictation, now, for a dictation the cloud was
    /// to clean and did not: S1-mini for English, the instruction model otherwise. No
    /// warmup: this is the fallback path, already late.
    fn load_fallback_cleanup(&mut self, config: &Config, preset: &Preset) -> Result<()> {
        let english = !Self::wants_instruct(config, preset);
        let mut pending = Vec::new();
        if english && self.cleanup.is_none() {
            pending.push(config.cleanup_path());
        }
        if !english && !(self.cleanup_multilingual.is_some() && self.instruct_is(config)) {
            pending.push(config.instruction_model_path());
        }
        if pending.is_empty() {
            return Ok(());
        }
        if self.cleanup_multilingual.is_some() && !self.instruct_is(config) {
            self.cleanup_multilingual = None;
            self.cleanup_multilingual_path = None;
        }
        self.report_vram(config, &pending);
        let started = Instant::now();
        self.load_cleanup(config, english, !english, false, &|_| {})?;
        if self.cleanup_multilingual.is_none() && self.cleanup.is_none() {
            anyhow::bail!("{} is not downloaded", config.instruction_model_path().display());
        }
        eprintln!(
            "local cleanup model ready in {}ms, after the cloud failed",
            started.elapsed().as_millis()
        );
        Ok(())
    }

    /// Log what is about to be loaded against what the card can still take, and warn
    /// when it will not fit. Weights that spill into system memory make every
    /// dictation 10-40x slower and nothing else says so. Measured before loading,
    /// because afterwards the budget cannot tell spilled weights from resident ones.
    fn report_vram(&mut self, config: &Config, pending: &[std::path::PathBuf]) {
        let (device, layers) = Self::gpu(&mut self.gpu, config.models.gpu_device);
        if layers == 0 {
            return;
        }
        let Some(free) = crate::asr::vram_free(device) else {
            return;
        };
        let needed: u64 = pending
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let (line, warning) = crate::asr::vram_report(free, needed as usize);
        eprintln!("{line}");
        if let Some(warning) = &warning {
            eprintln!("warning: {warning}");
        }
        self.spill_warning = warning;
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

    fn warmup(&mut self, config: &Config, english: bool, judge: bool) -> Result<()> {
        let started = Instant::now();
        if let Some(asr) = &self.asr {
            let silence = vec![0.0f32; crate::audio::TARGET_RATE as usize];
            let _ = asr.transcribe(&silence, "en");
        }
        // Only the model this dictation will actually use.
        let cleanup = if english {
            self.cleanup.as_ref()
        } else {
            self.cleanup_multilingual.as_ref().or(self.cleanup.as_ref())
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
                &[],
            );
        }
        // A judge beside S1-mini only ever scores, so warming it means scoring once.
        if let (true, Some(model), Some(backend)) =
            (judge, &self.cleanup_multilingual, &self.backend)
        {
            let _ = model.log_likelihoods(backend, &["warm up", "warmed up"], &[], config.models.threads);
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
        self.cleanup_multilingual_path = None;
        // Looked for again next time: the download may have finished in the meantime.
        self.cleanup_multilingual_missing = None;
        eprintln!("models unloaded after idle timeout");
        true
    }

    pub fn process(&mut self, config: &Config, preset: &Preset, pcm: &[f32]) -> Result<Processed> {
        self.last_used = Instant::now();

        // Brief 6.1: never call the model on silence. Runs without the speech model,
        // which a cloud recogniser leaves unloaded.
        let Some(gated) = Asr::gate_with(
            pcm,
            &config.vad_path(),
            config.audio.min_speech_ms,
            config.models.threads,
        )?
        else {
            return Ok(Processed::Rejected(Rejected::NoSpeech));
        };

        // Brief 5.5 pass 1: bias the recogniser toward the active vocabulary before it
        // decodes. Capped at 128 terms; biasing toward a list longer than the utterance
        // stops helping and starts dragging unrelated words toward it.
        let vocabulary = config.vocabulary.for_preset(&preset.vocabulary_sets);
        // Amendment A29: the language is a global switch, not a property of the preset.
        let lang = config.languages.current();

        // Brief 4.3: the external endpoint, when configured, replaces local recognition.
        // The VAD gate above still runs locally, so silence is never uploaded.
        if config.remote_asr.enabled {
            anyhow::bail!(
                "the external endpoint's API key could not be moved to the system credential \
                 store, so the endpoint is not used. The log says why."
            );
        }
        let (text, asr_ms) = if let Some((provider, model)) = config.cloud_speech()? {
            let started = Instant::now();
            match cloud_transcribe(provider, &model, &gated.pcm, lang, &vocabulary.terms(64)) {
                Ok(text) => (text, started.elapsed().as_millis()),
                Err(e) if config.models.speech_cloud_fallback => {
                    eprintln!("remote transcription failed, falling back to local: {e:#}");
                    // Not loaded up front, since the cloud was to do it. Slow once.
                    if self.asr.is_none() {
                        self.report_vram(config, &[config.whisper_path()]);
                        self.load_speech(config)?;
                    }
                    self.warn(format!(
                        "Speech: {model} at {} failed, so the local speech model \
                         recognised this dictation instead. {e:#}",
                        provider.display_name()
                    ));
                    let asr = self.asr.as_ref().context("speech model is not loaded")?;
                    asr.set_hotwords(&vocabulary.hotwords(128), config.vocabulary.hotword_boost);
                    let t = asr.transcribe(&gated.pcm, lang)?;
                    (t.text, t.infer_ms)
                }
                // Brief section 10: fail loudly rather than silently downgrading.
                Err(e) => return Err(e),
            }
        } else {
            let asr = self.asr.as_ref().context("speech model is not loaded")?;
            asr.set_hotwords(&vocabulary.hotwords(128), config.vocabulary.hotword_boost);
            let t = asr.transcribe(&gated.pcm, lang)?;
            (t.text, t.infer_ms)
        };

        // Brief 5.5 pass 2: fix what biasing did not, before the cleanup model sees it.
        // Order matters -- correcting a proper noun after cleanup would mean the
        // normaliser had already reasoned about a word that was wrong.
        //
        // The words only the sentence can settle -- "cloud" as the word or as "Claude"
        // -- are put to whichever cleanup model this dictation loaded, as the sentence
        // both ways. It works on text, so it does the same job behind every speech
        // model, including those that ignore pass 1 entirely. A cloud model cleaning
        // this dictation settles them itself instead, told which words they are: no
        // local model is loaded to ask, and writing every one as the term would turn
        // "the cloud" into "the Claude".
        let cloud_cleans = config.cloud_replaces_local_cleanup(preset);
        let cloud_settles = cloud_cleans && vocabulary.context;
        let heard = text;
        let (text, corrections) = if cloud_settles {
            vocabulary.correct_leaving_open(&heard)
        } else {
            self.weigh(config, preset, &vocabulary, &heard)
        };
        if corrections > 0 {
            eprintln!("vocabulary: {corrections} correction(s)");
        }

        let mut transcript = crate::asr::Transcript {
            text,
            infer_ms: asr_ms,
            audio_secs: gated.pcm.len() as f32 / crate::audio::TARGET_RATE as f32,
        };

        if transcript.text.is_empty() {
            return Ok(Processed::Rejected(Rejected::EmptyTranscript));
        }

        // Brief 4.2 restricted cleanup to English because S1-mini is English only. It
        // still is; amendment A21 adds a second, multilingual model for everything else,
        // so a non-English preset is cleaned rather than passed through raw. Amendment
        // A33 sends a rewrite preset through that same model in any language.
        // A cloud model first, when one is picked for this dictation's slot. Whatever
        // goes wrong there, the local model cleans it instead and the user is told.
        let mut cloud_failure = None;
        let cloud = if preset.cleanup {
            let terms = vocabulary.terms(64);
            let named = if cloud_settles { vocabulary.named_forms(32) } else { Vec::new() };
            match config.cloud_cleanup(preset) {
                Ok(Some((provider, model))) => {
                    let started = Instant::now();
                    match cloud_clean(provider, &model, preset, &transcript.text, lang, &terms, &named) {
                        Ok(text) => {
                            self.cloud_failing = false;
                            Some((text, started.elapsed().as_millis()))
                        }
                        Err(e) => {
                            eprintln!("cloud cleanup with '{model}' failed, cleaning locally: {e:#}");
                            cloud_failure = Some(format!(
                                "{model} at {} failed. {e:#}",
                                provider.display_name()
                            ));
                            None
                        }
                    }
                }
                Ok(None) => None,
                Err(e) => {
                    eprintln!("{e:#}; cleaning locally");
                    cloud_failure = Some(format!("{e:#}."));
                    None
                }
            }
        } else {
            None
        };
        if let Some(why) = cloud_failure {
            self.cloud_failing = true;
            // Not loaded up front because the cloud was to do it. Slow once; a missing
            // file leaves the transcript uncleaned, as it would without the cloud.
            let fallback = self.load_fallback_cleanup(config, preset);
            if let Err(e) = &fallback {
                eprintln!("could not load the local cleanup model: {e:#}");
            }
            // The words the cloud was to settle, weighed by the local model instead.
            if cloud_settles {
                let (text, corrections) = self.weigh(config, preset, &vocabulary, &heard);
                if corrections > 0 {
                    eprintln!("vocabulary: {corrections} correction(s), weighed locally");
                }
                transcript.text = text;
            }
            self.warn(match fallback {
                Ok(()) => format!("Cleanup: {why} The local model cleaned this dictation instead."),
                Err(e) => format!(
                    "Cleanup: {why} The local model could not be loaded either ({e:#}), so this \
                     dictation was pasted uncleaned."
                ),
            });
        }

        let (cleaned, cleanup_ms) = if let Some(done) = cloud {
            done
        } else if preset.cleanup {
            let instruct = Self::wants_instruct(config, preset);
            let model = if instruct {
                // Absent, and in English, S1-mini was loaded in its place.
                self.cleanup_multilingual.as_ref().or(self.cleanup.as_ref())
            } else {
                self.cleanup.as_ref()
            };
            // The instruction model can be told the vocabulary; S1-mini ignores it.
            let terms = vocabulary.terms(64);

            match model {
                Some(model) => {
                    let backend = self
                        .backend
                        .as_ref()
                        .context("llama backend is not initialised")?;
                    let rewrite = preset.rewrite;
                    let result = if rewrite != crate::cleanup::Rewrite::Off && model.can_rewrite() {
                        model.rewrite(
                            backend,
                            &transcript.text,
                            rewrite,
                            preset.styling,
                            config.models.threads,
                            language_name(lang),
                            &terms,
                        )?
                    } else {
                        model.normalize(
                            backend,
                            &transcript.text,
                            preset.styling,
                            preset.structure,
                            preset.context,
                            config.models.threads,
                            language_name(lang),
                            &terms,
                        )?
                    };
                    (result.text, result.infer_ms)
                }
                // The multilingual model is optional: it is a 2.3GB download and a
                // user who only dictates English never needs it. Say so once per
                // dictation rather than failing, since the raw transcript is still good.
                None => {
                    eprintln!(
                        "no cleanup model for '{lang}'; pasting the transcript uncleaned"
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
            language: lang.to_string(),
            audio_secs: transcript.audio_secs,
            asr_ms: transcript.infer_ms,
            cleanup_ms,
            vad_ms: gated.vad_ms,
        })))
    }

    /// Pass 2 with the local judge: whichever cleanup model this dictation loaded
    /// weighs each open word as the sentence both ways. Without one, or with context
    /// off, the words go the way they did before the judge existed.
    fn weigh(
        &self,
        config: &Config,
        preset: &Preset,
        vocabulary: &crate::vocabulary::Vocabulary,
        text: &str,
    ) -> (String, usize) {
        let judge_model = if Self::wants_instruct(config, preset)
            || Self::judges_with_instruct(config, preset)
        {
            self.cleanup_multilingual.as_ref().or(self.cleanup.as_ref())
        } else {
            self.cleanup.as_ref()
        };
        match (judge_model, &self.backend) {
            (Some(model), Some(backend)) if vocabulary.context => {
                let margin = vocabulary.context_margin;
                let threads = config.models.threads;
                let known = vocabulary.terms(64);
                vocabulary.correct_in_context(text, &mut |choice| {
                    // The term in question is always among those the judge is told of,
                    // even past the cap.
                    let mut terms = known.clone();
                    if !terms.iter().any(|t| t.eq_ignore_ascii_case(&choice.term)) {
                        terms.push(choice.term.clone());
                    }
                    match model.log_likelihoods(
                        backend,
                        &[&choice.as_heard, &choice.as_term],
                        &terms,
                        threads,
                    ) {
                        Ok(scores) => {
                            let take = choice.decide(scores[0], scores[1], margin);
                            eprintln!(
                                "vocabulary: '{}' or '{}'? {:+.2} -> {}",
                                choice.heard,
                                choice.term,
                                scores[1] - scores[0],
                                if take { &choice.term } else { &choice.heard }
                            );
                            Some(take)
                        }
                        Err(e) => {
                            eprintln!("vocabulary: could not weigh '{}': {e:#}", choice.heard);
                            None
                        }
                    }
                })
            }
            _ => vocabulary.correct(text),
        }
    }
}


/// Brief 4.3, through a provider. Builds the backend per call rather than holding it:
/// it owns no model and no connection, so there is nothing to keep warm, and reading
/// the config each time means a change takes effect on the next dictation like every
/// other setting. The key is read from the credential store at the same moment.
pub fn cloud_transcribe(
    provider: &crate::config::Provider,
    model: &str,
    pcm: &[f32],
    lang: &str,
    hints: &[String],
) -> Result<String> {
    use crate::asr_backend::{AsrBackend, OpenAiCompatBackend};
    use crate::secrets::KeyStore;

    if provider.base_url.trim().is_empty() {
        anyhow::bail!(
            "{} has no address. Add one on the Providers page.",
            provider.display_name()
        );
    }
    if provider.api == crate::config::Api::Anthropic {
        anyhow::bail!("{} has no speech models; pick one from another provider", provider.display_name());
    }
    let key = crate::secrets::OsKeyStore.get(&provider.id)?;
    let backend = OpenAiCompatBackend::new(&provider.base_url, model, key, provider.timeout_secs);
    // The vocabulary goes out as the request's `prompt`, which is the only way to bias
    // a remote recogniser. Capped like the local list: a Whisper-style prompt keeps
    // only its last 224 tokens.
    backend.transcribe(pcm, lang, hints)
}

/// Cleanup through a cloud model, with the prompt the local instruction model gets.
/// `named` is the vocabulary's named spoken forms, for the model to settle.
pub fn cloud_clean(
    provider: &crate::config::Provider,
    model: &str,
    preset: &Preset,
    text: &str,
    lang: &str,
    terms: &[String],
    named: &[(String, String)],
) -> Result<String> {
    use crate::cleanup::{build_instruct_prompt_for, build_rewrite_prompt_for, Rewrite, Turns};
    use crate::secrets::KeyStore;

    let key = crate::secrets::OsKeyStore.get(&provider.id)?;
    let client = crate::cloud::ChatClient::new(provider, model, key)?;
    let language = language_name(lang);
    let prompt = if preset.rewrite != Rewrite::Off {
        build_rewrite_prompt_for(Turns::Plain, text, language, preset.rewrite, preset.styling, terms, named)
    } else {
        build_instruct_prompt_for(
            Turns::Plain, text, language, preset.styling, preset.structure, preset.context, terms, named,
        )
    };
    // The local instruction model's budget: lists and email layout run longer.
    client.complete(&prompt, crate::cloud::max_tokens_for(text, 2.0))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instruction_model_judges_english_only_when_asked() {
        let mut config = Config::default();
        let preset = config.active().clone();
        assert!(!Engine::judges_with_instruct(&config, &preset));
        config.vocabulary.context_with_instruction_model = true;
        assert!(Engine::judges_with_instruct(&config, &preset));
        // Context off means there is nothing to judge.
        config.vocabulary.context = false;
        assert!(!Engine::judges_with_instruct(&config, &preset));
        // Polish goes through the instruction model anyway; it is not a second model.
        config.vocabulary.context = true;
        config.languages.active = "pl".into();
        assert!(!Engine::judges_with_instruct(&config, &preset));
    }

    fn cloud(model: &str) -> Option<crate::config::CloudChoice> {
        Some(crate::config::CloudChoice { provider: "or".into(), model: model.into() })
    }

    #[test]
    fn a_cloud_model_in_the_slot_means_no_local_cleanup_model_in_english_too() {
        let mut config = Config::default();
        let preset = config.active().clone();
        assert_eq!(
            Engine::needs(&config, &preset),
            Needs { speech: true, s1mini: true, instruct: false }
        );
        config.models.cleanup_cloud = cloud("gpt-4.1-mini");
        // Not even as the vocabulary judge: the cloud model is told the named forms.
        config.vocabulary.context_with_instruction_model = true;
        assert_eq!(
            Engine::needs(&config, &preset),
            Needs { speech: true, s1mini: false, instruct: false }
        );
        // Polish uses the other slot, which is still local.
        config.languages.active = "pl".into();
        assert_eq!(
            Engine::needs(&config, &preset),
            Needs { speech: true, s1mini: false, instruct: true }
        );
        config.models.cleanup_multilingual_cloud = cloud("gpt-4.1-mini");
        config.models.whisper_cloud = cloud("whisper-1");
        assert_eq!(
            Engine::needs(&config, &preset),
            Needs { speech: false, s1mini: false, instruct: false }
        );
        // Raw is not cleaned, so its judge is local as before.
        let mut raw = preset.clone();
        raw.cleanup = false;
        assert!(Engine::needs(&config, &raw).instruct);
    }

    #[test]
    fn with_every_slot_in_the_cloud_nothing_is_loaded() {
        let mut config = Config::default();
        // No file exists here: anything that tried to load one would fail.
        config.models.dir = std::env::temp_dir().join("lathe-no-models-here");
        config.models.whisper_cloud = cloud("whisper-1");
        config.models.cleanup_cloud = cloud("gpt-4.1-mini");
        let preset = config.active().clone();
        let mut engine = Engine::new().unwrap();
        assert!(engine.loaded(&config, &preset));
        engine.ensure_loaded(&config, &preset, &|step| panic!("{step}")).unwrap();
        assert!(engine.asr.is_none() && engine.cleanup.is_none() && engine.cleanup_multilingual.is_none());
        assert!(engine.backend.is_none(), "the GPU backend was started for nothing");
        // With the fallback on, the speech model still waits for a failure.
        config.models.speech_cloud_fallback = true;
        assert!(engine.loaded(&config, &preset));
        // Back to local speech: now it is needed.
        config.models.whisper_cloud = None;
        assert!(!engine.loaded(&config, &preset));
    }

    #[test]
    fn a_general_model_picked_for_english_routes_english_through_it() {
        let mut config = Config::default();
        let preset = config.active().clone();
        assert!(!Engine::wants_instruct(&config, &preset));
        assert_eq!(config.instruction_model_path(), config.cleanup_multilingual_path());

        config.models.cleanup = "gemma-4-E2B_q4_0-it.gguf".into();
        assert!(Engine::wants_instruct(&config, &preset));
        assert_eq!(config.instruction_model_path(), config.cleanup_path());
        // Already the instruction model, so there is no second one to judge with.
        config.vocabulary.context_with_instruction_model = true;
        assert!(!Engine::judges_with_instruct(&config, &preset));

        // Polish keeps its own model whatever English uses.
        config.languages.active = "pl".into();
        assert_eq!(config.instruction_model_path(), config.cleanup_multilingual_path());
    }
}
