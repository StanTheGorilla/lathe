// Model downloading. Brief 5.7 and phase 7.
//
// Small on purpose: this fetches three or four known files from one host, once. It is
// not a download manager. Progress is reported through a callback so the caller decides
// whether that becomes a tray tooltip, a notification, or a progress bar in settings.

use anyhow::{anyhow, Context as _, Result};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;

/// A model the app knows how to fetch.
#[derive(Debug, Clone, Serialize)]
pub struct Known {
    pub file: &'static str,
    pub label: &'static str,
    pub role: &'static str,
    pub url: &'static str,
    /// Rough size for the UI, in bytes. Exact size comes from the server.
    pub approx_bytes: u64,
    pub required: bool,
}

pub const KNOWN: &[Known] = &[
    Known {
        file: "cohere-transcribe-q8_0.gguf",
        label: "Cohere Transcribe Q8_0",
        role: "Speech",
        url: "https://huggingface.co/cstr/cohere-transcribe-03-2026-GGUF/resolve/main/cohere-transcribe-q8_0.gguf",
        approx_bytes: 2_430_000_000,
        required: true,
    },
    Known {
        file: "ggml-silero-v5.1.2.bin",
        label: "Silero VAD v5.1.2",
        role: "Speech detection",
        url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
        approx_bytes: 928_000,
        required: true,
    },
    Known {
        file: "s1-mini-f16.gguf",
        label: "S1-mini F16",
        role: "Cleanup, English",
        url: "https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/main/s1-mini-f16.gguf",
        approx_bytes: 1_515_000_000,
        required: true,
    },
    // Brief 4.2 requires these to ship alongside the model.
    Known {
        file: "LICENSE",
        label: "S1-mini licence",
        role: "Licence",
        url: "https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/main/LICENSE",
        approx_bytes: 12_000,
        required: true,
    },
    Known {
        file: "NOTICE",
        label: "S1-mini notice",
        role: "Licence",
        url: "https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/main/NOTICE",
        approx_bytes: 600,
        required: true,
    },
    Known {
        file: "cohere-transcribe-q5_0.gguf",
        label: "Cohere Transcribe Q5_0 (smaller)",
        role: "Speech, alternative",
        url: "https://huggingface.co/cstr/cohere-transcribe-03-2026-GGUF/resolve/main/cohere-transcribe-q5_0.gguf",
        approx_bytes: 1_738_000_000,
        required: false,
    },
    Known {
        file: "cohere-transcribe-f16.gguf",
        label: "Cohere Transcribe F16 (full precision)",
        role: "Speech, alternative",
        url: "https://huggingface.co/cstr/cohere-transcribe-03-2026-GGUF/resolve/main/cohere-transcribe.gguf",
        approx_bytes: 4_135_000_000,
        required: false,
    },
    Known {
        file: "s1-mini-q4_k_m.gguf",
        label: "S1-mini Q4_K_M (smaller)",
        role: "Cleanup, alternative",
        url: "https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/main/s1-mini-q4_k_m.gguf",
        approx_bytes: 484_000_000,
        required: false,
    },
    Known {
        file: "gemma-3-4b-it-Q8_0.gguf",
        label: "Gemma 3 4B Q8_0 (higher precision)",
        role: "Cleanup non-English, alternative",
        url: "https://huggingface.co/unsloth/gemma-3-4b-it-GGUF/resolve/main/gemma-3-4b-it-Q8_0.gguf",
        approx_bytes: 4_135_000_000,
        required: false,
    },
    Known {
        file: "gemma-3-4b-it-qat-Q4_0.gguf",
        label: "Gemma 3 4B (multilingual cleanup)",
        role: "Cleanup, non-English",
        url: "https://huggingface.co/ggml-org/gemma-3-4b-it-qat-GGUF/resolve/main/gemma-3-4b-it-qat-Q4_0.gguf",
        approx_bytes: 2_530_000_000,
        required: false,
    },
    Known {
        file: "whisper-large-v3-turbo-q5_0.gguf",
        label: "Whisper large-v3-turbo Q5_0",
        role: "Speech (alternative)",
        url: "https://huggingface.co/oxide-lab/whisper-large-v3-turbo-GGUF/resolve/main/whisper-large-v3-turbo-q5_0.gguf",
        approx_bytes: 574_000_000,
        required: false,
    },
];

pub fn known(file: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.file == file)
}

/// Downloads `file` into `dir`, reporting (bytes so far, total) as it goes.
///
/// Writes to a `.part` file and renames on success, so an interrupted download can never
/// be mistaken for a complete model. A half-written GGUF would fail at load with a
/// confusing parse error rather than an obvious "not downloaded".
pub fn fetch(
    entry: &Known,
    dir: &Path,
    mut progress: impl FnMut(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let final_path = dir.join(entry.file);
    let part_path = dir.join(format!("{}.part", entry.file));

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60 * 60)))
        .build()
        .new_agent();

    let mut response = agent
        .get(entry.url)
        .call()
        .map_err(|e| anyhow!("could not start the download: {e}"))?;

    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(entry.approx_bytes);

    let mut reader = response.body_mut().as_reader();
    let mut file = std::fs::File::create(&part_path)
        .with_context(|| format!("creating {}", part_path.display()))?;

    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        if cancel() {
            drop(file);
            let _ = std::fs::remove_file(&part_path);
            return Err(anyhow!("download cancelled"));
        }
        let read = reader.read(&mut buffer).context("reading from the server")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("writing {}", part_path.display()))?;
        done += read as u64;
        progress(done, total);
    }

    file.flush()?;
    drop(file);

    // A truncated transfer that ended cleanly still leaves a short file.
    if total > 0 && done < total {
        let _ = std::fs::remove_file(&part_path);
        return Err(anyhow!(
            "the download ended early: got {done} bytes of {total}"
        ));
    }

    std::fs::rename(&part_path, &final_path)
        .with_context(|| format!("moving into place: {}", final_path.display()))?;
    Ok(())
}
