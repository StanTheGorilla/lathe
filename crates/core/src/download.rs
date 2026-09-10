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

/// The known model files sitting in `dir`, with their sizes.
pub fn present_in(dir: &Path) -> Vec<(&'static Known, u64)> {
    KNOWN
        .iter()
        .filter_map(|k| {
            let size = std::fs::metadata(dir.join(k.file)).ok()?.len();
            Some((k, size))
        })
        .collect()
}

/// Moves every known model file from `from` into `to`, reporting bytes moved overall.
///
/// `std::fs::rename` is instant within a volume and fails across one, which is the case
/// that matters: choosing a models directory is usually about getting several gigabytes
/// off the system drive. The fallback copies and only unlinks the source once the copy
/// has landed, so an interrupted move can never destroy the only copy of a 2GB file.
///
/// Files already present at the destination are left alone rather than overwritten.
pub fn relocate(
    from: &Path,
    to: &Path,
    mut progress: impl FnMut(&'static str, u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<u32> {
    if from == to {
        return Ok(0);
    }

    let items = present_in(from);
    let total: u64 = items.iter().map(|(_, size)| *size).sum();
    let mut done = 0u64;
    let mut moved = 0u32;

    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;

    for (entry, size) in items {
        if cancel() {
            return Err(anyhow!("move cancelled"));
        }

        let src = from.join(entry.file);
        let dst = to.join(entry.file);

        if dst.exists() {
            done += size;
            progress(entry.file, done, total);
            continue;
        }

        progress(entry.file, done, total);

        if std::fs::rename(&src, &dst).is_err() {
            copy_across(&src, &dst, size, done, total, &mut progress, cancel, entry.file)?;
            std::fs::remove_file(&src)
                .with_context(|| format!("removing {} after the copy", src.display()))?;
        }

        done += size;
        moved += 1;
        progress(entry.file, done, total);
    }

    Ok(moved)
}

#[allow(clippy::too_many_arguments)]
fn copy_across(
    src: &Path,
    dst: &Path,
    size: u64,
    base: u64,
    total: u64,
    progress: &mut impl FnMut(&'static str, u64, u64),
    cancel: &dyn Fn() -> bool,
    label: &'static str,
) -> Result<()> {
    let part = dst.with_extension("part");
    let mut reader =
        std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mut writer =
        std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;

    let mut buffer = vec![0u8; 4 << 20];
    let mut copied = 0u64;
    loop {
        if cancel() {
            drop(writer);
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!("move cancelled"));
        }
        let read = reader.read(&mut buffer).context("reading the model file")?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .with_context(|| format!("writing {}", part.display()))?;
        copied += read as u64;
        progress(label, base + copied, total);
    }

    writer.flush()?;
    drop(writer);

    if copied < size {
        let _ = std::fs::remove_file(&part);
        return Err(anyhow!(
            "{label} copied short: {copied} bytes of {size}; the original is untouched"
        ));
    }

    std::fs::rename(&part, dst).with_context(|| format!("moving into place: {}", dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch(tag: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lathe-relocate-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, bytes: usize) {
        std::fs::write(dir.join(name), vec![7u8; bytes]).unwrap();
    }

    const A_MODEL: &str = "cohere-transcribe-q8_0.gguf";
    const ANOTHER: &str = "ggml-silero-v5.1.2.bin";

    fn never() -> impl Fn() -> bool {
        || false
    }

    #[test]
    fn moves_known_files_and_leaves_nothing_behind() {
        let from = scratch("from");
        let to = scratch("to");
        write(&from, A_MODEL, 2048);
        write(&from, ANOTHER, 512);
        write(&from, "notes.txt", 10);

        let moved = relocate(&from, &to, |_, _, _| {}, &never()).unwrap();

        assert_eq!(moved, 2);
        assert!(to.join(A_MODEL).exists());
        assert!(to.join(ANOTHER).exists());
        assert!(!from.join(A_MODEL).exists());
        // Files the app does not know about are not its business to move.
        assert!(from.join("notes.txt").exists());
    }

    #[test]
    fn an_existing_file_at_the_destination_is_not_overwritten() {
        let from = scratch("from");
        let to = scratch("to");
        write(&from, A_MODEL, 2048);
        write(&to, A_MODEL, 4096);

        relocate(&from, &to, |_, _, _| {}, &never()).unwrap();

        assert_eq!(std::fs::metadata(to.join(A_MODEL)).unwrap().len(), 4096);
    }

    #[test]
    fn moving_a_directory_onto_itself_does_nothing() {
        let dir = scratch("same");
        write(&dir, A_MODEL, 100);
        assert_eq!(relocate(&dir, &dir, |_, _, _| {}, &never()).unwrap(), 0);
        assert!(dir.join(A_MODEL).exists());
    }

    #[test]
    fn progress_reaches_the_total_it_promised() {
        let from = scratch("from");
        let to = scratch("to");
        write(&from, A_MODEL, 2048);
        write(&from, ANOTHER, 512);

        let mut last = (0u64, 0u64);
        relocate(&from, &to, |_, done, total| last = (done, total), &never()).unwrap();
        assert_eq!(last, (2560, 2560));
    }

    /// The cross-volume path, which `rename` cannot take. Exercised directly because a
    /// test cannot rely on a second drive existing.
    #[test]
    fn the_copy_fallback_moves_bytes_faithfully() {
        let from = scratch("from");
        let to = scratch("to");
        write(&from, A_MODEL, 5000);

        let mut seen = 0u64;
        copy_across(
            &from.join(A_MODEL),
            &to.join(A_MODEL),
            5000,
            0,
            5000,
            &mut |_, done, _| seen = done,
            &never(),
            A_MODEL,
        )
        .unwrap();

        assert_eq!(seen, 5000);
        assert_eq!(std::fs::read(to.join(A_MODEL)).unwrap(), vec![7u8; 5000]);
        // The source is only unlinked by the caller, after the copy has landed.
        assert!(from.join(A_MODEL).exists());
    }

    #[test]
    fn a_cancelled_copy_leaves_no_half_file_and_keeps_the_original() {
        let from = scratch("from");
        let to = scratch("to");
        write(&from, A_MODEL, 5000);

        let err = copy_across(
            &from.join(A_MODEL),
            &to.join(A_MODEL),
            5000,
            0,
            5000,
            &mut |_, _, _| {},
            &|| true,
            A_MODEL,
        )
        .unwrap_err();

        assert!(err.to_string().contains("cancelled"));
        assert!(!to.join(A_MODEL).exists());
        assert!(!to.join(A_MODEL).with_extension("part").exists());
        assert!(from.join(A_MODEL).exists());
    }
}
