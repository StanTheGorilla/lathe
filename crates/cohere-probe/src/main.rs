// Does Cohere Transcribe run on this machine, and how fast?
//
// Brief 4.3 says it cannot: custom conformer architecture, no ggml support, and a Rust
// port that builds only for Linux/CPU and macOS/Metal. That was true when the brief was
// written. CrispASR now provides a ggml-based runtime with a Vulkan backend and a
// C-ABI, and GGUF conversions of the model exist. This measures the claim.

use anyhow::{anyhow, Result};
use clap::Parser;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
struct Cli {
    wav: PathBuf,
    #[arg(long, default_value = "models/cohere-transcribe-q5_0.gguf")]
    model: PathBuf,
    #[arg(long, default_value = "en")]
    lang: String,
    /// vulkan, cpu, and whatever else CrispASR reports.
    #[arg(long)]
    backend: Option<String>,
    #[arg(long, default_value_t = 3)]
    passes: usize,
}

fn read_wav_16k_mono(path: &PathBuf) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };

    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels <= 1 {
        raw
    } else {
        raw.chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    if spec.sample_rate == 16_000 {
        return Ok(mono);
    }
    // Linear resample is fine for a measurement: the reference clip is already 16k, and
    // this path only exists so an arbitrary wav can be thrown at the probe.
    let ratio = 16_000.0 / spec.sample_rate as f32;
    let out_len = (mono.len() as f32 * ratio) as usize;
    Ok((0..out_len)
        .map(|i| {
            let src = i as f32 / ratio;
            let a = src.floor() as usize;
            let b = (a + 1).min(mono.len().saturating_sub(1));
            let t = src - a as f32;
            mono.get(a).copied().unwrap_or(0.0) * (1.0 - t) + mono.get(b).copied().unwrap_or(0.0) * t
        })
        .collect())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!(
        "backends compiled in: {}",
        crispasr::Session::available_backends().join(", ")
    );

    let model = cli
        .model
        .to_str()
        .ok_or_else(|| anyhow!("model path is not valid UTF-8"))?;
    if !cli.model.exists() {
        return Err(anyhow!("model not found at {}", cli.model.display()));
    }

    match crispasr::Session::detect_backend(model) {
        Ok(b) => println!("architecture detected as: {b}"),
        Err(e) => println!("architecture detection failed: {e}"),
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(8);

    let start = Instant::now();
    let session = match &cli.backend {
        Some(b) => crispasr::Session::open_with_backend(model, b, threads),
        None => crispasr::Session::open(model),
    }
    .map_err(|e| anyhow!("could not open the model: {e}"))?;
    println!(
        "loaded in {}ms, running on '{}'",
        start.elapsed().as_millis(),
        session.backend()
    );

    let pcm = read_wav_16k_mono(&cli.wav)?;
    let secs = pcm.len() as f32 / 16_000.0;
    println!("\n{secs:.2}s of audio, {} passes\n", cli.passes);

    let mut text = String::new();
    for pass in 1..=cli.passes {
        let start = Instant::now();
        let segments = session
            .transcribe_with_language(&pcm, Some(&cli.lang))
            .map_err(|e| anyhow!("transcription failed: {e}"))?;
        let ms = start.elapsed().as_millis();
        println!(
            "pass {pass}: {ms}ms, {:.2}x realtime",
            secs / (ms as f32 / 1000.0)
        );
        text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }

    println!("\ntext: {}", text.trim());

    println!();
    probe_llama_coexistence();
    println!();
    probe_replaces_whisper_rs(&session, &pcm, &cli.lang);
    Ok(())
}

/// Proves the two runtimes coexist: CrispASR's ggml in DLLs, llama.cpp's linked
/// statically into this executable.
fn probe_llama_coexistence() {
    match llama_cpp_2::llama_backend::LlamaBackend::init() {
        Ok(_) => println!("llama.cpp backend initialised alongside CrispASR"),
        Err(e) => println!("llama.cpp backend FAILED alongside CrispASR: {e}"),
    }
}

/// Can CrispASR do the two jobs whisper-rs currently does: load a whisper ggml model,
/// and gate on Silero? If so, whisper-rs can go, taking one ggml copy with it.
fn probe_replaces_whisper_rs(cohere: &crispasr::Session, pcm: &[f32], lang: &str) {
    let whisper = "models/ggml-large-v3-turbo-q5_0.bin";
    let silero = "models/ggml-silero-v5.1.2.bin";

    if !std::path::Path::new(whisper).exists() {
        println!("whisper model missing, skipping");
        return;
    }

    // VAD first, using the session we already have: it does not depend on the whisper
    // model at all, and is the part that decides whether whisper-rs can be dropped.
    if std::path::Path::new(silero).exists() {
        let start = std::time::Instant::now();
        match cohere.transcribe_vad_with_language(pcm, silero, None, Some(lang)) {
            Ok(segs) => println!(
                "silero VAD via crispasr: {}ms, {} segment(s)",
                start.elapsed().as_millis(),
                segs.len()
            ),
            Err(e) => println!("silero VAD via crispasr failed: {e}"),
        }
    }

    match crispasr::Session::detect_backend(whisper) {
        Ok(b) => println!("whisper ggml detected as backend '{b}'"),
        Err(e) => {
            println!("legacy whisper ggml not readable by crispasr: {e}");
            return;
        }
    }

    let session = match crispasr::Session::open(whisper) {
        Ok(s) => s,
        Err(e) => {
            println!("could not open the whisper model: {e}");
            return;
        }
    };
    let start = std::time::Instant::now();
    match session.transcribe_with_language(pcm, Some(lang)) {
        Ok(segs) => {
            let text: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
            println!(
                "whisper via crispasr: {}ms -- {}",
                start.elapsed().as_millis(),
                text.join(" ").trim()
            );
        }
        Err(e) => println!("whisper via crispasr failed: {e}"),
    }

    if !std::path::Path::new(silero).exists() {
        println!("silero model missing, skipping the VAD check");
        return;
    }
    let start = std::time::Instant::now();
    match session.transcribe_vad_with_language(pcm, silero, None, Some(lang)) {
        Ok(segs) => println!(
            "silero VAD via crispasr: {}ms, {} segment(s)",
            start.elapsed().as_millis(),
            segs.len()
        ),
        Err(e) => println!("silero VAD via crispasr failed: {e}"),
    }
}
