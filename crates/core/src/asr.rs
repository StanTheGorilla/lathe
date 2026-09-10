// Speech recognition through CrispASR. Brief sections 4.1 and 4.3, and amendment A14.
//
// One runtime serves every model: Cohere Transcribe, Whisper (GGUF), and the Silero VAD
// gate from 6.1. That is why whisper-rs is gone -- see A14 for the DLL collision that
// forced the choice, and for why it turned out to be the better architecture anyway.

use anyhow::{anyhow, Result};
use crispasr::Session;
use std::path::Path;
use std::time::Instant;

/// `vad_segments` returns spans in centiseconds; at 16kHz that is 160 samples each.
const SAMPLES_PER_CENTISECOND: f32 = super::audio::TARGET_RATE as f32 / 100.0;

pub struct Asr {
    session: Session,
    backend: String,
    threads: i32,
}

pub struct Transcript {
    pub text: String,
    pub infer_ms: u128,
    pub audio_secs: f32,
}

pub struct Gated {
    pub pcm: Vec<f32>,
    pub segments: usize,
    pub speech_secs: f32,
    pub vad_ms: u128,
}

impl Asr {
    pub fn load(model: &Path, threads: i32) -> Result<(Self, u128)> {
        if !model.exists() {
            return Err(anyhow!("speech model not found at {}", model.display()));
        }
        let path = model
            .to_str()
            .ok_or_else(|| anyhow!("model path is not valid UTF-8"))?;

        let start = Instant::now();
        // CrispASR picks the backend from the model's architecture and the backends it
        // was compiled with. Vulkan is used when the model and the hardware allow it.
        let session = Session::open_with_backend(path, &detect(path)?, threads)
            .map_err(|e| anyhow!("could not open {}: {e}", model.display()))?;
        let load_ms = start.elapsed().as_millis();
        let backend = session.backend();

        // Said once per load rather than once per dictation. Silence here is what made
        // the vocabulary look broken: the biasing call succeeds on every backend, so
        // nothing ever reported that most of them drop it on the floor.
        if !backend_biases(&backend) {
            eprintln!(
                "note: '{backend}' does not support recogniser biasing; \
                 vocabulary pass 1 has no effect and terms are applied by the \
                 repair pass only"
            );
        }

        Ok((
            Self {
                session,
                backend,
                threads,
            },
            load_ms,
        ))
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Brief 6.1: run Silero before recognition and return only the speech span.
    ///
    /// Amendment A10 still applies: trim to the span between the first and last speech
    /// segment rather than splicing the pauses out, because splicing measurably
    /// degrades transcription.
    ///
    /// Returns `None` when there is not enough speech to be worth transcribing.
    pub fn gate(&self, pcm: &[f32], vad_model: &Path, min_speech_ms: u32) -> Result<Option<Gated>> {
        if !vad_model.exists() {
            // Brief section 10: fail loudly rather than skipping the gate, which would
            // reintroduce the hallucination problem 6.1 exists to solve.
            return Err(anyhow!(
                "speech detection model not found at {}",
                vad_model.display()
            ));
        }
        let vad_path = vad_model
            .to_str()
            .ok_or_else(|| anyhow!("VAD model path is not valid UTF-8"))?;

        let start = Instant::now();
        // Silero is 0.8MB and takes tens of milliseconds on CPU. It is run on the CPU
        // deliberately: whisper.cpp cannot execute the Silero graph on Vulkan, which
        // amendment A3 established the hard way.
        let segments = crispasr::vad_segments(
            vad_path,
            pcm,
            super::audio::TARGET_RATE as i32,
            0.5,
            min_speech_ms as i32,
            100,
            self.threads,
            false,
        )
        .map_err(|e| anyhow!("speech detection failed: {e}"))?;
        let vad_ms = start.elapsed().as_millis();

        if segments.is_empty() {
            return Ok(None);
        }

        let mut speech_cs = 0.0f32;
        let mut first = f32::MAX;
        let mut last = 0.0f32;
        for segment in &segments {
            speech_cs += (segment.1 - segment.0).max(0.0);
            first = first.min(segment.0);
            last = last.max(segment.1);
        }

        // Brief 5.7: reject clips with under the configured speech duration.
        let speech_secs = speech_cs / 100.0;
        if speech_secs * 1000.0 < min_speech_ms as f32 {
            return Ok(None);
        }

        // 100ms of padding, in centiseconds, so the first and last words are not
        // clipped by a tight segment boundary.
        let pad = 10.0f32;
        let start_sample = (((first - pad).max(0.0)) * SAMPLES_PER_CENTISECOND) as usize;
        let end_sample = (((last + pad) * SAMPLES_PER_CENTISECOND) as usize).min(pcm.len());
        let start_sample = start_sample.min(end_sample);

        Ok(Some(Gated {
            pcm: pcm[start_sample..end_sample].to_vec(),
            segments: segments.len(),
            speech_secs,
            vad_ms,
        }))
    }

    /// Brief 5.5 pass 1: bias decoding toward these terms.
    ///
    /// Contextual biasing rather than a decoder prompt, so the list does not consume
    /// context or pull the output style around. An empty string clears it.
    ///
    /// Whether this does anything at all depends on the backend -- see
    /// [`backend_biases`]. It is called unconditionally regardless, because the call is
    /// free and the answer changes with the model.
    pub fn set_hotwords(&self, terms: &str, boost: f32) {
        if let Err(e) = self.session.set_hotwords(terms, boost) {
            // Pass 2 still runs, so this degrades rather than fails.
            eprintln!("vocabulary biasing unavailable on this model: {e}");
        }
    }

    pub fn transcribe(&self, pcm: &[f32], lang: &str) -> Result<Transcript> {
        let audio_secs = pcm.len() as f32 / super::audio::TARGET_RATE as f32;
        let start = Instant::now();
        let segments = self
            .session
            .transcribe_with_language(pcm, Some(lang))
            .map_err(|e| anyhow!("transcription failed: {e}"))?;
        let infer_ms = start.elapsed().as_millis();

        let text = segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        Ok(Transcript {
            text: text.trim().to_string(),
            infer_ms,
            audio_secs,
        })
    }
}

/// The CrispASR backends that do something with the terms handed to `set_hotwords`.
///
/// `crispasr_session_set_hotwords` returns success unconditionally. Underneath, it feeds
/// parakeet's trie directly, and for the prompt-driven backends it prepends the terms to
/// the ask prompt their decoder reads. Every other backend -- Cohere Transcribe, the
/// default speech model, among them -- stores the string and never looks at it again.
/// Checked against crispasr v0.8.31 `src/crispasr_c_api.cpp`; the Cohere dispatch at the
/// `s->backend == "cohere"` branch passes the decoder audio and a language code, nothing
/// more.
///
/// Conservative on purpose: a backend absent from this list is reported as not biasing,
/// which is the safe direction to be wrong in. Being told the vocabulary is doing less
/// than it is costs a sentence of surprise; the reverse cost an evening.
const BIASING_BACKENDS: [&str; 9] = [
    "parakeet",
    "lfm2",
    "mini_omni2",
    "higgs_stt",
    "qwen3",
    "granite",
    "voxtral",
    "voxtral4b",
    "glmasr",
];

pub fn backend_biases(backend: &str) -> bool {
    BIASING_BACKENDS
        .iter()
        .any(|b| backend.eq_ignore_ascii_case(b))
}

/// The backend a model file will run on, without loading the model. Reads the GGUF
/// header only, so it is cheap enough for a settings screen.
pub fn backend_of(model: &Path) -> Option<String> {
    if !model.exists() {
        return None;
    }
    Session::detect_backend(model.to_str()?).ok()
}

/// The model file says which architecture it is; CrispASR reads that and names the
/// backend to run it on.
fn detect(path: &str) -> Result<String> {
    Session::detect_backend(path).map_err(|e| {
        anyhow!(
            "could not identify the model architecture ({e}). \
             Models must be GGUF; the legacy whisper.cpp .bin format is not supported."
        )
    })
}

/// What ggml reports a compute device as. Ordering matters: see `best_adapter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    Cpu,
    Other,
}

impl AdapterKind {
    fn rank(self) -> u8 {
        match self {
            AdapterKind::Discrete => 0,
            AdapterKind::Integrated => 1,
            AdapterKind::Other => 2,
            AdapterKind::Cpu => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AdapterKind::Discrete => "discrete",
            AdapterKind::Integrated => "integrated",
            AdapterKind::Cpu => "cpu",
            AdapterKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Adapter {
    pub id: i32,
    pub name: String,
    pub vram_total: usize,
    pub kind: AdapterKind,
}

/// Vulkan adapters, for the picker in brief 4.1.
pub fn list_adapters() -> Vec<Adapter> {
    use llama_cpp_2::LlamaBackendDeviceType as T;
    llama_cpp_2::list_llama_ggml_backend_devices()
        .into_iter()
        .enumerate()
        .map(|(i, d)| Adapter {
            id: i as i32,
            name: d.description,
            vram_total: d.memory_total,
            kind: match d.device_type {
                T::Gpu => AdapterKind::Discrete,
                T::IntegratedGpu => AdapterKind::Integrated,
                T::Cpu => AdapterKind::Cpu,
                _ => AdapterKind::Other,
            },
        })
        .collect()
}

/// The device to use when the config asks for automatic selection.
///
/// Discrete cards first, then integrated, then anything that is not the CPU, with more
/// memory winning within a class. ggml's own order is whatever the driver enumerated,
/// which on a machine with both a discrete and an integrated GPU is often the
/// integrated one -- the common NVIDIA laptop, where index 0 is the Intel chip.
/// Sorting by memory alone would be worse still: the CPU device reports system RAM,
/// which on this machine is three times the card's VRAM.
pub fn best_adapter(adapters: &[Adapter]) -> Option<&Adapter> {
    adapters
        .iter()
        .min_by_key(|a| (a.kind.rank(), std::cmp::Reverse(a.vram_total)))
}

/// A device that exists, resolved from whatever the config asked for.
#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: i32,
    pub name: String,
    /// False when the chosen device is the CPU, where offloading layers is meaningless.
    pub offload: bool,
}

/// Turns `models.gpu_device` into a device that is actually present.
///
/// A negative index means automatic. A positive one that no longer matches anything --
/// a config copied from a machine with more GPUs, or a card that has been removed --
/// falls back to automatic rather than failing the dictation.
pub fn resolve_gpu(requested: i32) -> Gpu {
    resolve_among(requested, &list_adapters())
}

fn resolve_among(requested: i32, adapters: &[Adapter]) -> Gpu {
    let chosen = adapters.iter().find(|a| a.id == requested).or_else(|| {
        if requested >= 0 {
            eprintln!(
                "gpu device {requested} is not present ({} found); choosing automatically",
                adapters.len()
            );
        }
        best_adapter(adapters)
    });

    match chosen {
        Some(a) => Gpu {
            device: a.id,
            name: a.name.clone(),
            offload: a.kind != AdapterKind::Cpu,
        },
        // ggml always registers a CPU device, so this is unreachable in practice.
        None => Gpu {
            device: 0,
            name: "CPU".into(),
            offload: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(id: i32, name: &str, mib: usize, kind: AdapterKind) -> Adapter {
        Adapter {
            id,
            name: name.into(),
            vram_total: mib * 1024 * 1024,
            kind,
        }
    }

    /// The common NVIDIA laptop: the integrated chip enumerates first, and picking
    /// index 0 hands the work to the wrong device.
    #[test]
    fn prefers_discrete_over_integrated() {
        let found = [
            adapter(0, "Intel UHD Graphics", 2048, AdapterKind::Integrated),
            adapter(1, "NVIDIA GeForce RTX 4060", 8192, AdapterKind::Discrete),
        ];
        assert_eq!(best_adapter(&found).unwrap().id, 1);
    }

    /// Even when the integrated one claims more memory, which it can: shared memory is
    /// reported as system RAM.
    #[test]
    fn discrete_wins_on_kind_not_size() {
        let found = [
            adapter(0, "Radeon Graphics", 24370, AdapterKind::Integrated),
            adapter(1, "Radeon RX 6600 XT", 8176, AdapterKind::Discrete),
        ];
        assert_eq!(best_adapter(&found).unwrap().id, 1);
    }

    #[test]
    fn cpu_is_never_chosen_over_a_gpu() {
        let found = [
            adapter(0, "Radeon RX 6600 XT", 8176, AdapterKind::Discrete),
            adapter(1, "Ryzen 7 5700G", 24370, AdapterKind::Cpu),
        ];
        assert_eq!(best_adapter(&found).unwrap().id, 0);
    }

    #[test]
    fn largest_card_wins_between_two_of_a_kind() {
        let found = [
            adapter(0, "RTX 3060", 12288, AdapterKind::Discrete),
            adapter(1, "RTX 4090", 24576, AdapterKind::Discrete),
        ];
        assert_eq!(best_adapter(&found).unwrap().id, 1);
    }

    #[test]
    fn cpu_only_machine_runs_without_offloading() {
        let found = [adapter(0, "Ryzen 7 5700G", 24370, AdapterKind::Cpu)];
        let gpu = resolve_among(-1, &found);
        assert_eq!(gpu.device, 0);
        assert!(!gpu.offload, "layers must not be offloaded to the CPU device");
    }

    /// A config carried from a machine with more GPUs than this one has.
    #[test]
    fn stale_index_falls_back_instead_of_failing() {
        let found = [
            adapter(0, "Intel UHD Graphics", 2048, AdapterKind::Integrated),
            adapter(1, "NVIDIA GeForce RTX 4060", 8192, AdapterKind::Discrete),
        ];
        let gpu = resolve_among(7, &found);
        assert_eq!(gpu.device, 1);
        assert!(gpu.offload);
    }

    #[test]
    fn an_explicit_choice_is_honoured() {
        let found = [
            adapter(0, "Intel UHD Graphics", 2048, AdapterKind::Integrated),
            adapter(1, "NVIDIA GeForce RTX 4060", 8192, AdapterKind::Discrete),
        ];
        assert_eq!(resolve_among(0, &found).device, 0);
    }

    #[test]
    fn no_devices_at_all_still_yields_something_runnable() {
        let gpu = resolve_among(-1, &[]);
        assert!(!gpu.offload);
    }
}
