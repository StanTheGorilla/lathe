// Phase 1 spike. Brief section 9.1, widened by amendment A3 to link both ggml stacks
// in one process and run the full mic -> Whisper -> S1-mini path.
//
// This is a measurement tool, not the app. There is no tray, no hotkey and no daemon;
// those arrive in phase 3.





use anyhow::Result;
use lathe_core::{asr, audio, cleanup};
use clap::{Parser, Subcommand, ValueEnum};
use lathe_core::cleanup::{Context, Structure, Styling};
use llama_cpp_2::llama_backend::LlamaBackend;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "lathe-spike", about = "Lathe phase 1 spike")]
struct Cli {
    /// Vulkan device index. Defaults to 0, which is the discrete GPU: ggml orders
    /// discrete adapters ahead of integrated ones.
    #[arg(long, default_value_t = 0, global = true)]
    gpu: i32,

    /// Directory holding the model files.
    #[arg(long, default_value = "models", global = true)]
    models: PathBuf,

    /// Override the speech model filename, for comparing quantizations.
    #[arg(long, global = true, default_value = "cohere-transcribe-q5_0.gguf")]
    speech_model: String,

    /// Override the English cleanup model filename.
    #[arg(long, global = true, default_value = "s1-mini-q4_k_m.gguf")]
    cleanup_model: String,

    /// Override the non-English cleanup model filename, for comparing candidates.
    #[arg(long, global = true, default_value = "gemma-3-4b-it-qat-Q4_0.gguf")]
    multilingual_model: String,

    /// Let whisper.cpp and llama.cpp log to stderr. Off by default: llama.cpp emits
    /// several hundred lines per model load.
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List Vulkan adapters and audio devices.
    Devices,
    /// Record from the default input device and write a 16kHz mono wav.
    Record {
        #[arg(long, default_value = "capture.wav")]
        out: PathBuf,
        #[arg(long, default_value_t = 300)]
        max_secs: u64,
    },
    /// Transcribe a wav file.
    Transcribe {
        wav: PathBuf,
        #[arg(long, default_value = "en")]
        lang: String,
        /// Vocabulary hints for whisper's initial_prompt, brief 5.5 pass 1.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        no_vad: bool,
    },
    /// Normalize a transcript with S1-mini.
    Clean {
        text: String,
        /// Language code. Anything but "en" uses the multilingual model.
        #[arg(long, default_value = "en")]
        lang: String,
        #[arg(long, value_enum, default_value_t = StylingArg::SemiFormal)]
        styling: StylingArg,
        #[arg(long, value_enum, default_value_t = StructureArg::Prose)]
        structure: StructureArg,
        #[arg(long, value_enum, default_value_t = ContextArg::General)]
        context: ContextArg,
        /// Print the exact prompt sent to the model and exit without loading it.
        #[arg(long)]
        show_prompt: bool,
    },
    /// Run a wav through the whole pipeline in one process and report timings.
    /// This is the phase 1 form of the benchmark in brief 6.9, and the real test of
    /// amendment A3: both ggml stacks doing actual GPU work back to back.
    Bench {
        wav: PathBuf,
        #[arg(long, default_value = "en")]
        lang: String,
        #[arg(long)]
        no_vad: bool,
        /// Number of passes, to separate cold shader compilation from warm timings.
        #[arg(long, default_value_t = 2)]
        passes: usize,
    },
    /// Measure recognition accuracy against a list of sentences you read aloud.
    ///
    /// Recordings are kept, so a second run against a different --speech-model scores
    /// the same audio instead of asking for it again. That is the whole point: the
    /// question "is the recogniser the problem" is only answerable by comparing two
    /// models on identical input.
    Accuracy {
        #[arg(long, default_value = "assets/accuracy-en.txt")]
        sentences: PathBuf,
        /// Where the recordings live. Reused when already present.
        #[arg(long, default_value = "recordings")]
        dir: PathBuf,
        /// Language passed to the recogniser. Polish lines need their own run.
        #[arg(long, default_value = "en")]
        lang: String,
        /// Re-record everything, discarding what is already there.
        #[arg(long)]
        rerecord: bool,
        /// Score what has already been recorded and skip straight to the report.
        #[arg(long)]
        score_only: bool,
        /// Comma-separated terms to bias the recogniser toward, as brief 5.5 pass 1
        /// does. The point of the flag is the A/B: the same audio and the same model,
        /// scored with and without, which is the only way to tell whether a backend
        /// honours biasing or merely accepts the call and drops it.
        #[arg(long)]
        hotwords: Option<String>,
        #[arg(long, default_value_t = 2.0)]
        hotword_boost: f32,
        /// Apply brief 5.5 pass 2 -- the phonetic repair pass, using the vocabulary from
        /// the real config -- before scoring. Measures what the app actually pastes
        /// rather than what the recogniser alone produced.
        #[arg(long)]
        vocabulary: bool,
    },
    /// Run the cleanup model over a set of transcripts and, given an earlier run's
    /// output, score how closely this model reproduces it.
    ///
    /// The reference is the full-precision model's output: decoding is greedy, so a
    /// quantization either reproduces F16 word for word or it does not, and "does not"
    /// is reported as exact-match rate, word error rate against F16, and the number of
    /// outputs that lost content -- the clause-dropping failure amendment A23 caught by
    /// eye, detected mechanically here.
    CleanupEval {
        /// JSON lines of {raw, styling, structure, context}.
        #[arg(long, default_value = "assets/cleanup-eval.jsonl")]
        set: PathBuf,
        /// Where this model's outputs go, one JSON line per input, in order.
        #[arg(long)]
        out: PathBuf,
        /// A previous --out file to score against, normally the F16 run.
        #[arg(long)]
        reference: Option<PathBuf>,
        /// Score against the set's own `clean` field instead of another run. For
        /// comparing different models, where no one of them is the reference.
        #[arg(long)]
        against_clean: bool,
        /// Language code. Anything but "en" uses the instruction-model prompt.
        #[arg(long, default_value = "en")]
        lang: String,
        /// Use the instruction-model prompt in English too, to compare a general model
        /// (LFM2.5, Qwen3.5, Gemma) against S1-mini on the same set with
        /// --against-clean. Pass the model with --cleanup-model.
        #[arg(long)]
        instruct: bool,
        /// Clean through a cloud provider instead: its OpenAI-compatible address.
        /// The key, if it needs one, comes from the LATHE_API_KEY environment
        /// variable, so it never lands in shell history or a file.
        #[arg(long, requires = "cloud_model")]
        cloud_url: Option<String>,
        /// The model to ask at --cloud-url, as the provider names it.
        #[arg(long)]
        cloud_model: Option<String>,
    },
    /// Put the vocabulary's context questions to a cleanup model and score its answers.
    ///
    /// Each line is a sentence as heard, the word in it that a term might claim, and
    /// whether that word was meant as heard or as the term. The model reads the
    /// sentence both ways, exactly as a dictation does, and the report gives the
    /// accuracy at a range of margins, so `context_margin` can be set from evidence.
    ContextEval {
        /// JSON lines of {text, heard, term, named, want: "term" | "heard"}.
        #[arg(long, default_value = "assets/context-eval.jsonl")]
        set: PathBuf,
        /// Judge with the instruction model given by --multilingual-model instead of
        /// the English cleanup model.
        #[arg(long)]
        instruct: bool,
        /// Score without telling the model the vocabulary, as 0.1.15 did.
        #[arg(long)]
        no_terms: bool,
        /// Tell the model only the term in question, not the whole vocabulary.
        #[arg(long)]
        only_asked: bool,
    },
    /// Duck other applications for a few seconds, then restore. Verifies the ducking
    /// path without needing a dictation.
    Duck {
        #[arg(long, default_value_t = 4)]
        secs: u64,
        #[arg(long, default_value_t = 0.15)]
        level: f32,
    },
    /// The whole path: record, transcribe, normalize.
    Run {
        #[arg(long, default_value_t = 300)]
        max_secs: u64,
        #[arg(long, default_value = "en")]
        lang: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        no_vad: bool,
        #[arg(long, value_enum, default_value_t = StylingArg::SemiFormal)]
        styling: StylingArg,
        #[arg(long, value_enum, default_value_t = StructureArg::Prose)]
        structure: StructureArg,
        #[arg(long, value_enum, default_value_t = ContextArg::General)]
        context: ContextArg,
        /// Keep the captured audio. Brief 5.1: off unless asked for explicitly.
        #[arg(long)]
        keep_audio: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum StylingArg {
    Casual,
    SemiCasual,
    SemiFormal,
    Formal,
}

#[derive(Copy, Clone, ValueEnum)]
enum StructureArg {
    Prose,
    Lists,
}

#[derive(Copy, Clone, ValueEnum)]
enum ContextArg {
    General,
    Email,
}

impl From<StylingArg> for Styling {
    fn from(a: StylingArg) -> Self {
        match a {
            StylingArg::Casual => Styling::Casual,
            StylingArg::SemiCasual => Styling::SemiCasual,
            StylingArg::SemiFormal => Styling::SemiFormal,
            StylingArg::Formal => Styling::Formal,
        }
    }
}

impl From<StructureArg> for Structure {
    fn from(a: StructureArg) -> Self {
        match a {
            StructureArg::Prose => Structure::Prose,
            StructureArg::Lists => Structure::Lists,
        }
    }
}

impl From<ContextArg> for Context {
    fn from(a: ContextArg) -> Self {
        match a {
            ContextArg::General => Context::General,
            ContextArg::Email => Context::Email,
        }
    }
}

const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";

fn threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(8)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.verbose {
        // Both libraries log to stderr by default. Route them through the log and
        // tracing facades, which drop everything when no subscriber is installed.
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
    }

    // Copied out before the match takes `cli.command` by value.
    let models_dir = cli.models.clone();
    let speech_model = cli.speech_model.clone();

    match cli.command {
        Command::Devices => {
            print_vulkan()?;
            audio::list_devices()?;
        }

        Command::Record { out, max_secs } => {
            let pcm = audio::record(max_secs)?;
            audio::write_wav(&out, &pcm, audio::TARGET_RATE)?;
            println!("wrote {} ({} samples)", out.display(), pcm.len());
        }

        Command::Transcribe {
            wav,
            lang,
            prompt,
            no_vad,
        } => {
            let pcm = audio::read_wav(&wav)?;
            match run_asr(&cli.models, &cli.speech_model, cli.gpu, &pcm, &lang, prompt.as_deref(), no_vad)? {
                Some(transcript) => println!("{}", transcript.text),
                None => eprintln!("no speech detected, whisper never called (brief 6.1)"),
            }
        }

        Command::Clean {
            text,
            lang,
            styling,
            structure,
            context,
            show_prompt,
        } => {
            if show_prompt {
                print!(
                    "{}",
                    if lang.eq_ignore_ascii_case("en") {
                        cleanup::build_prompt(&text, styling.into(), structure.into(), context.into())
                    } else {
                        cleanup::build_instruct_prompt(
                            &text,
                            lathe_core::engine::language_name(&lang),
                            styling.into(),
                            structure.into(),
                            context.into(),
                        )
                    }
                );
                return Ok(());
            }
            let backend = LlamaBackend::init()?;
            let cleaned = run_cleanup(
                &backend,
                &cli.models,
                &cli.cleanup_model,
                &cli.multilingual_model,
                cli.gpu,
                &lang,
                &text,
                styling.into(),
                structure.into(),
                context.into(),
            )?;
            println!("{}", cleaned.text);
        }

        Command::Bench {
            wav,
            lang,
            no_vad,
            passes,
        } => {
            let pcm = audio::read_wav(&wav)?;
            let audio_secs = pcm.len() as f32 / audio::TARGET_RATE as f32;

            let (whisper, whisper_load) =
                asr::Asr::load(&cli.models.join(&cli.speech_model), threads())?;

            let backend = LlamaBackend::init()?;
            let (s1, s1_load) = cleanup::Cleanup::load(
                &backend,
                &cli.models.join(&cli.cleanup_model),
                cli.gpu,
                999,
                cleanup::Flavour::S1Mini,
            )?;

            println!(
                "\nloaded: asr {whisper_load}ms on '{}', s1-mini {s1_load}ms",
                whisper.backend()
            );
            println!("{:.2}s of audio, {passes} passes\n", audio_secs);

            let gated = if no_vad {
                asr::Gated {
                    pcm: pcm.clone(),
                    segments: 0,
                    speech_secs: audio_secs,
                    vad_ms: 0,
                }
            } else {
                let Some(g) = whisper.gate(&pcm, &cli.models.join(VAD_MODEL), MIN_SPEECH_MS)?
                else {
                    println!("no speech detected, the model was never called (brief 6.1)");
                    return Ok(());
                };
                println!(
                    "vad: {} segments, {:.2}s of speech kept, {}ms\n",
                    g.segments, g.speech_secs, g.vad_ms
                );
                g
            };

            for pass in 1..=passes {
                let transcript = whisper.transcribe(&gated.pcm, &lang)?;
                let cleaned = s1.normalize(
                    &backend,
                    &transcript.text,
                    Styling::SemiFormal,
                    Structure::Prose,
                    Context::General,
                    threads(),
                    "English",
                    &[],
                )?;
                let total = transcript.infer_ms + cleaned.infer_ms;
                println!(
                    "pass {pass}: asr {}ms + cleanup {}ms = {}ms, {:.2}x realtime",
                    transcript.infer_ms,
                    cleaned.infer_ms,
                    total,
                    audio_secs / (total as f32 / 1000.0)
                );
                if pass == passes {
                    println!("\nraw:     {}", transcript.text);
                    println!("cleaned: {}", cleaned.text);
                }
            }
        }

        Command::Accuracy {
            sentences,
            dir,
            lang,
            rerecord,
            score_only,
            hotwords,
            hotword_boost,
            vocabulary,
        } => {
            accuracy(
                &models_dir,
                &speech_model,
                &sentences,
                &dir,
                &lang,
                rerecord,
                score_only,
                hotwords.as_deref(),
                hotword_boost,
                vocabulary,
            )?;
        }

        Command::CleanupEval {
            set,
            out,
            reference,
            against_clean,
            lang,
            instruct,
            cloud_url,
            cloud_model,
        } => {
            let cloud = cloud_url.zip(cloud_model);
            cleanup_eval(
                cloud.as_ref().map(|(u, m)| (u.as_str(), m.as_str())),
                &cli.models,
                &cli.cleanup_model,
                cli.gpu,
                &set,
                &out,
                reference.as_deref(),
                against_clean,
                &lang,
                instruct,
            )?;
        }

        Command::ContextEval { set, instruct, no_terms, only_asked } => {
            let (model, flavour) = if instruct {
                (&cli.multilingual_model, cleanup::Flavour::Instruct)
            } else {
                (&cli.cleanup_model, cleanup::Flavour::S1Mini)
            };
            context_eval(&cli.models.join(model), flavour, cli.gpu, &set, !no_terms, only_asked)?;
        }

        Command::Duck { secs, level } => {
            let ducker = lathe_core::ducking::Ducker::start(level)?;
            println!(
                "ducked {} audio session(s) to {:.0}%; restoring in {secs}s",
                ducker.count(),
                level * 100.0
            );
            std::thread::sleep(std::time::Duration::from_secs(secs));
            drop(ducker);
            println!("restored");
        }

        Command::Run {
            max_secs,
            lang,
            prompt,
            no_vad,
            styling,
            structure,
            context,
            keep_audio,
        } => {
            let pcm = audio::record(max_secs)?;
            if let Some(path) = &keep_audio {
                audio::write_wav(path, &pcm, audio::TARGET_RATE)?;
                eprintln!("kept audio at {}", path.display());
            }

            let Some(transcript) =
                run_asr(&cli.models, &cli.speech_model, cli.gpu, &pcm, &lang, prompt.as_deref(), no_vad)?
            else {
                // Brief 6.1 and 5.2: this is where the daemon plays the error cue.
                eprintln!("\nno speech detected, whisper never called (brief 6.1)");
                return Ok(());
            };
            println!("\nraw:     {}", transcript.text);

            if transcript.text.is_empty() {
                eprintln!("whisper produced nothing. Not calling S1-mini.");
                return Ok(());
            }

            // Brief 4.2: English only. Non-English bypasses S1-mini entirely.
            if lang != "en" {
                eprintln!("\nlanguage is {lang}, not en -- S1-mini bypassed per brief 4.2");
                return Ok(());
            }

            let backend = LlamaBackend::init()?;
            let cleaned = run_cleanup(
                &backend,
                &cli.models,
                &cli.cleanup_model,
                &cli.multilingual_model,
                cli.gpu,
                &lang,
                &transcript.text,
                styling.into(),
                structure.into(),
                context.into(),
            )?;

            if cleaned.text.is_empty() {
                println!("cleaned: (empty -- valid result per brief 4.2, nothing would be pasted)");
            } else {
                println!("cleaned: {}", cleaned.text);
            }

            let total = transcript.infer_ms + cleaned.infer_ms;
            println!(
                "\n{:.2}s audio | asr {}ms | cleanup {}ms ({} prompt, {} generated) | total {}ms | rtf {:.2}x",
                transcript.audio_secs,
                transcript.infer_ms,
                cleaned.infer_ms,
                cleaned.prompt_tokens,
                cleaned.generated_tokens,
                total,
                transcript.audio_secs / (total as f32 / 1000.0)
            );
        }
    }

    Ok(())
}

fn print_vulkan() -> Result<()> {
    println!("compute adapters (ggml order, discrete first):");
    for a in asr::list_adapters() {
        println!(
            "  {}: {} [{}] -- {} MiB",
            a.id,
            a.name,
            a.kind.label(),
            a.vram_total / (1024 * 1024)
        );
    }
    Ok(())
}

/// Brief 5.7: the default gate rejects clips with under 300ms of detected speech.
const MIN_SPEECH_MS: u32 = 300;

fn run_asr(
    models: &Path,
    speech_model: &str,
    _gpu: i32,
    pcm: &[f32],
    lang: &str,
    prompt: Option<&str>,
    no_vad: bool,
) -> Result<Option<asr::Transcript>> {
    let (engine, load_ms) = asr::Asr::load(&models.join(speech_model), threads())?;
    eprintln!(
        "speech model loaded in {load_ms}ms on backend '{}'",
        engine.backend()
    );

    let gated = if no_vad {
        asr::Gated {
            pcm: pcm.to_vec(),
            segments: 0,
            speech_secs: pcm.len() as f32 / audio::TARGET_RATE as f32,
            vad_ms: 0,
        }
    } else {
        let Some(gated) = engine.gate(pcm, &models.join(VAD_MODEL), MIN_SPEECH_MS)? else {
            return Ok(None);
        };
        eprintln!(
            "vad: {} speech segments, {:.2}s of speech, {}ms",
            gated.segments, gated.speech_secs, gated.vad_ms
        );
        gated
    };

    // Brief 5.5 pass 1. The daemon always does this; the spike exists to measure what
    // the daemon does, so a --prompt that was accepted and ignored measured nothing.
    if let Some(terms) = prompt {
        eprintln!("hotwords: {terms}");
        engine.set_hotwords(terms, 2.0);
    }

    let transcript = engine.transcribe(&gated.pcm, lang)?;
    eprintln!(
        "asr: {}ms for {:.2}s audio",
        transcript.infer_ms, transcript.audio_secs
    );
    Ok(Some(transcript))
}

fn run_cleanup(
    backend: &LlamaBackend,
    models: &Path,
    cleanup_model: &str,
    multilingual_model: &str,
    gpu: i32,
    lang: &str,
    raw: &str,
    styling: Styling,
    structure: Structure,
    context: Context,
) -> Result<cleanup::Cleaned> {
    // All layers on GPU. S1-mini is 0.6B; there is no reason to split it.
    let (model, flavour) = if lang.eq_ignore_ascii_case("en") {
        (cleanup_model, cleanup::Flavour::S1Mini)
    } else {
        (multilingual_model, cleanup::Flavour::Instruct)
    };
    let (engine, load_ms) =
        cleanup::Cleanup::load(backend, &models.join(model), gpu, 999, flavour)?;
    eprintln!("s1-mini loaded in {load_ms}ms");

    let cleaned = engine.normalize(
        backend,
        raw,
        styling,
        structure,
        context,
        threads(),
        lathe_core::engine::language_name(lang),
        &[],
    )?;
    eprintln!(
        "s1-mini: {}ms, {} tokens generated",
        cleaned.infer_ms, cleaned.generated_tokens
    );
    Ok(cleaned)
}

#[derive(serde::Deserialize)]
struct ContextItem {
    text: String,
    heard: String,
    term: String,
    named: bool,
    want: String,
}

/// One question and what the model made of it.
struct ContextScore {
    text: String,
    heard: String,
    term: String,
    named: bool,
    want_term: bool,
    /// log p(as term) - log p(as heard).
    delta: f32,
}

fn context_eval(
    model_path: &Path,
    flavour: cleanup::Flavour,
    gpu: i32,
    set: &Path,
    with_terms: bool,
    only_asked: bool,
) -> Result<()> {
    use lathe_core::vocabulary::{Set, Term, Vocabulary};

    // What a dictation tells the judge: the shipped vocabulary, as the engine passes it.
    let shipped = Vocabulary::default().terms(64);

    let items: Vec<ContextItem> = read_jsonl(set)?;
    let backend = LlamaBackend::init()?;
    let (model, load_ms) = cleanup::Cleanup::load(&backend, model_path, gpu, 999, flavour)?;
    eprintln!("{} loaded in {load_ms}ms, {} sentences", model_path.display(), items.len());

    let mut scores = Vec::new();
    let mut unasked = Vec::new();
    let started = std::time::Instant::now();
    for item in &items {
        let term = if item.named {
            Term::heard(item.term.clone(), &[item.heard.as_str()])
        } else {
            Term::new(item.term.clone())
        };
        let vocabulary = Vocabulary {
            sets: vec![Set {
                name: "eval".into(),
                enabled: true,
                terms: vec![term],
            }],
            ..Vocabulary::default()
        };
        let terms = if with_terms && only_asked {
            vec![item.term.clone()]
        } else if with_terms {
            let mut t = shipped.clone();
            if !t.contains(&item.term) {
                t.push(item.term.clone());
            }
            t
        } else {
            Vec::new()
        };
        let mut asked = false;
        let mut failure = None;
        vocabulary.correct_in_context(&item.text, &mut |choice| {
            // Only the first such word is the one the line is about.
            if asked || !choice.heard.eq_ignore_ascii_case(&item.heard) {
                return None;
            }
            asked = true;
            match model.log_likelihoods(
                &backend,
                &[&choice.as_heard, &choice.as_term],
                &terms,
                threads(),
            ) {
                Ok(s) => scores.push(ContextScore {
                    text: item.text.clone(),
                    heard: item.heard.clone(),
                    term: item.term.clone(),
                    named: item.named,
                    want_term: item.want == "term",
                    delta: s[1] - s[0],
                }),
                Err(e) => failure = Some(e),
            }
            None
        });
        if let Some(e) = failure {
            return Err(e);
        }
        if !asked {
            unasked.push(item.text.clone());
        }
    }
    let per_question = started.elapsed().as_millis() as f64 / scores.len().max(1) as f64;

    println!("{:>7}  {:<5} {:<6} sentence", "delta", "want", "kind");
    for s in &scores {
        println!(
            "{:>+7.2}  {:<5} {:<6} {}  [{} / {}]",
            s.delta,
            if s.want_term { "term" } else { "heard" },
            if s.named { "named" } else { "twin" },
            s.text,
            s.heard,
            s.term,
        );
    }
    if !unasked.is_empty() {
        println!("\nnever asked (the vocabulary did not treat the word as a question):");
        for t in &unasked {
            println!("  {t}");
        }
    }

    // Named forms switch on any lead; inferred twins need the margin. Accuracy for
    // each margin tells which value to put in `context_margin`.
    println!("\n{:>7}  {:>11}  {:>11}  {:>11}", "margin", "named", "twins", "all");
    let default_margin = Vocabulary::default().context_margin;
    for margin in [-1.0f32, 0.0, 0.5, 1.0, 2.0, 3.0, 4.0, 6.0] {
        let right = |named: Option<bool>| {
            let pool: Vec<_> = scores
                .iter()
                .filter(|s| named.is_none_or(|n| s.named == n))
                .collect();
            let ok = pool
                .iter()
                .filter(|s| {
                    let needed = if s.named { 0.0 } else { margin };
                    (s.delta > needed) == s.want_term
                })
                .count();
            format!("{ok}/{}", pool.len())
        };
        println!(
            "{:>7.1}  {:>11}  {:>11}  {:>11}{}",
            margin,
            right(Some(true)),
            right(Some(false)),
            right(None),
            if margin == default_margin { "  <- current default" } else { "" }
        );
    }
    // Named forms on their own, against a threshold of their own: below zero leans
    // toward the term the user named.
    println!("\n{:>9}  {:>11}  {:>13}  {:>13}", "named at", "named", "wanted term", "wanted heard");
    for threshold in [-6.0f32, -4.0, -3.0, -2.0, -1.0, 0.0, 1.0] {
        let named: Vec<_> = scores.iter().filter(|s| s.named).collect();
        let count = |want: Option<bool>| {
            let pool: Vec<_> = named.iter().filter(|s| want.is_none_or(|w| s.want_term == w)).collect();
            let ok = pool.iter().filter(|s| (s.delta > threshold) == s.want_term).count();
            format!("{ok}/{}", pool.len())
        };
        println!(
            "{:>9.1}  {:>11}  {:>13}  {:>13}{}",
            threshold,
            count(None),
            count(Some(true)),
            count(Some(false)),
            if threshold == 0.0 { "  <- current" } else { "" }
        );
    }
    println!("\n{per_question:.0} ms per question, both readings together");
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EvalItem {
    raw: String,
    styling: Styling,
    structure: Structure,
    context: Context,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    /// What the output should be, when the set knows. Synthetic sets do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clean: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EvalOutput {
    #[serde(flatten)]
    item: EvalItem,
    cleaned: String,
    prompt_tokens: usize,
    generated_tokens: usize,
    setup_ms: u128,
    prompt_ms: u128,
    infer_ms: u128,
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l)
                .map_err(|e| anyhow::anyhow!("{}:{}: {e}", path.display(), i + 1))
        })
        .collect()
}

fn cleanup_eval(
    cloud: Option<(&str, &str)>,
    models: &Path,
    cleanup_model: &str,
    gpu: i32,
    set: &Path,
    out: &Path,
    reference: Option<&Path>,
    against_clean: bool,
    lang: &str,
    instruct: bool,
) -> Result<()> {
    let items: Vec<EvalItem> = read_jsonl(set)?;
    let outputs = match cloud {
        Some((url, model)) => cloud_cleanup_outputs(url, model, items, lang)?,
        None => local_cleanup_outputs(models, cleanup_model, gpu, items, lang, instruct)?,
    };

    let mut lines = String::new();
    for o in &outputs {
        lines.push_str(&serde_json::to_string(o)?);
        lines.push('\n');
    }
    std::fs::write(out, lines)?;
    let label = cloud.map_or(cleanup_model, |(_, m)| m);
    score_cleanup(label, &outputs, reference, against_clean)
}

/// The eval set cleaned by a cloud model, with the prompt the app sends it.
fn cloud_cleanup_outputs(
    url: &str,
    model: &str,
    items: Vec<EvalItem>,
    lang: &str,
) -> Result<Vec<EvalOutput>> {
    use lathe_core::cleanup::{build_instruct_prompt_for, Turns};
    use lathe_core::config::Provider;

    let provider = Provider {
        id: "eval".into(),
        name: "eval".into(),
        base_url: url.into(),
        models: vec![],
        timeout_secs: 120,
    };
    let key = std::env::var("LATHE_API_KEY").ok();
    let client = lathe_core::cloud::ChatClient::new(&provider, model, key)?;
    let language = lathe_core::engine::language_name(lang);
    let total = items.len();
    let mut outputs = Vec::with_capacity(total);
    for (i, item) in items.into_iter().enumerate() {
        let prompt = build_instruct_prompt_for(
            Turns::Plain, &item.raw, language, item.styling, item.structure, item.context, &[],
        );
        let started = std::time::Instant::now();
        let cleaned = client.complete(&prompt, lathe_core::cloud::max_tokens_for(&item.raw, 2.0))?;
        eprint!("\r{}/{total}", i + 1);
        outputs.push(EvalOutput {
            item,
            cleaned,
            prompt_tokens: 0,
            generated_tokens: 0,
            setup_ms: 0,
            prompt_ms: 0,
            infer_ms: started.elapsed().as_millis(),
        });
    }
    eprintln!();
    Ok(outputs)
}

fn local_cleanup_outputs(
    models: &Path,
    cleanup_model: &str,
    gpu: i32,
    items: Vec<EvalItem>,
    lang: &str,
    instruct: bool,
) -> Result<Vec<EvalOutput>> {
    let flavour = if lang.eq_ignore_ascii_case("en") && !instruct {
        cleanup::Flavour::S1Mini
    } else {
        cleanup::Flavour::Instruct
    };
    let language = lathe_core::engine::language_name(lang);
    let backend = LlamaBackend::init()?;
    let (engine, load_ms) = cleanup::Cleanup::load(
        &backend,
        &models.join(cleanup_model),
        gpu,
        999,
        flavour,
    )?;
    eprintln!("{cleanup_model} loaded in {load_ms}ms, {} inputs", items.len());

    // The first call pays for shader compilation; run it and discard so the timings
    // below are warm. Same discipline as `bench`.
    if let Some(first) = items.first() {
        engine.normalize(
            &backend,
            &first.raw,
            first.styling,
            first.structure,
            first.context,
            threads(),
            language,
            &[],
        )?;
    }

    let total = items.len();
    let mut outputs = Vec::with_capacity(total);
    for (i, item) in items.into_iter().enumerate() {
        let cleaned = engine.normalize(
            &backend,
            &item.raw,
            item.styling,
            item.structure,
            item.context,
            threads(),
            language,
            &[],
        )?;
        eprint!("\r{}/{total}", i + 1);
        outputs.push(EvalOutput {
            item,
            cleaned: cleaned.text,
            prompt_tokens: cleaned.prompt_tokens,
            generated_tokens: cleaned.generated_tokens,
            setup_ms: cleaned.setup_ms,
            prompt_ms: cleaned.prompt_ms,
            infer_ms: cleaned.infer_ms,
        });
    }
    eprintln!();
    Ok(outputs)
}

fn score_cleanup(
    label: &str,
    outputs: &[EvalOutput],
    reference: Option<&Path>,
    against_clean: bool,
) -> Result<()> {
    let total = outputs.len();
    let n = total as f64;
    let sum = |f: &dyn Fn(&EvalOutput) -> u128| outputs.iter().map(f).sum::<u128>() as f64;
    let generated = sum(&|o| o.generated_tokens as u128);
    let decode_ms = sum(&|o| o.infer_ms - o.prompt_ms);
    println!("{label}: {total} inputs");
    println!(
        "  per dictation: setup {:.1}ms, prompt {:.1}ms ({:.0} tokens), decode {:.1}ms ({:.2}ms/token, {:.0} tokens)",
        sum(&|o| o.setup_ms) / n,
        sum(&|o| o.prompt_ms) / n,
        sum(&|o| o.prompt_tokens as u128) / n,
        decode_ms / n,
        decode_ms / generated,
        generated / n,
    );

    // What to score against: another run's outputs, or the set's own clean text.
    let (label, refs): (String, Vec<String>) = if against_clean {
        let clean = outputs
            .iter()
            .map(|o| o.item.clean.clone())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("--against-clean needs a `clean` field on every input"))?;
        ("clean text".to_string(), clean)
    } else if let Some(reference) = reference {
        let runs: Vec<EvalOutput> = read_jsonl(reference)?;
        if runs.len() != total {
            anyhow::bail!("reference has {} outputs, this run {total}", runs.len());
        }
        for (i, (r, h)) in runs.iter().zip(outputs).enumerate() {
            if r.item.raw != h.item.raw {
                anyhow::bail!("input {} differs between the reference and this set", i + 1);
            }
        }
        (reference.display().to_string(), runs.into_iter().map(|r| r.cleaned).collect())
    } else {
        return Ok(());
    };

    let mut exact = 0usize;
    let mut errors = 0usize;
    let mut ref_words = 0usize;
    let mut lost = Vec::new();
    let mut differing: Vec<(usize, usize)> = Vec::new();
    for (i, (r, h)) in refs.iter().zip(outputs).enumerate() {
        let rw = words(r);
        let hw = words(&h.cleaned);
        let (dist, _) = compare(&rw, &hw);
        ref_words += rw.len();
        errors += dist;
        if *r == h.cleaned {
            exact += 1;
        } else {
            differing.push((dist, i));
        }
        if lost_content(&rw, &hw) {
            lost.push(i);
        }
    }
    println!(
        "  vs {label}: exact {exact}/{total} ({:.1}%), WER {:.2}%, content lost in {}",
        100.0 * exact as f64 / n,
        100.0 * errors as f64 / ref_words.max(1) as f64,
        lost.len(),
    );
    for &i in &lost {
        println!("\n  content lost, input {}:", i + 1);
        println!("    ref:  {}", refs[i]);
        println!("    this: {}", outputs[i].cleaned);
    }
    differing.sort_unstable_by(|a, b| b.cmp(a));
    for &(dist, i) in differing.iter().take(5) {
        if lost.contains(&i) {
            continue;
        }
        println!("\n  {dist} word edits, input {}:", i + 1);
        println!("    ref:  {}", refs[i]);
        println!("    this: {}", outputs[i].cleaned);
    }
    Ok(())
}

/// Words that carry no content on their own. Dropping one of these is a stylistic
/// difference; dropping anything else is the failure the eval exists to catch.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "so", "to", "of", "in", "on", "at", "for",
    "with", "by", "from", "as", "is", "are", "was", "were", "be", "been", "am", "it",
    "its", "it's", "this", "that", "these", "those", "i", "i'm", "i'd", "i'll", "i've",
    "you", "your", "we", "our", "they", "their", "he", "she", "me", "my", "us", "them",
    "do", "does", "did", "have", "has", "had", "will", "would", "can", "could", "should",
    "just", "really", "very", "also", "then", "there", "here", "if", "than", "about",
    "up", "out", "please", "thanks", "thank", "okay", "ok", "yeah", "yes", "no", "not",
    "um", "uh", "like", "mean", "well", "actually", "basically", "kind", "sort",
    // Polish function words and fillers, for the multilingual sets.
    "i", "w", "na", "z", "ze", "że", "się", "nie", "to", "jest", "są", "był", "była",
    "no", "znaczy", "jakby", "okej", "yyy", "więc", "tak", "już", "ale", "o", "do", "po",
    "od", "za", "co", "ten", "ta", "te", "ja", "ty", "my", "by", "czy", "jak", "tam", "tu",
    "mi", "mnie", "ci", "cię", "go", "mu", "ją", "jej", "ich", "im", "nas", "wam", "was",
    "dla", "przez", "przy", "pod", "nad", "bez", "też", "tylko", "bardzo", "może",
];

/// True when the hypothesis is the reference with content words removed and nothing
/// added: every content word it has, the reference also has, and at least one of the
/// reference's is gone. A reworded output is not a loss, and neither is one that
/// collapses a word the reference repeated -- only a word that disappears entirely.
fn lost_content(reference: &[String], hypothesis: &[String]) -> bool {
    use std::collections::HashSet;
    fn content(ws: &[String]) -> HashSet<&str> {
        ws.iter()
            .map(String::as_str)
            .filter(|w| !STOPWORDS.contains(w))
            .collect()
    }
    let r = content(reference);
    let h = content(hypothesis);
    h.is_subset(&r) && !r.is_subset(&h)
}

/// One line per sentence, `#` for comments.
fn read_sentences(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Lowercased words with punctuation stripped. Diacritics are deliberately *kept*: on the
/// Polish sentences, restoring them is part of what is being measured, and folding them
/// away here would score a wrong answer as right.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-level edit distance, and the substitutions it had to make.
///
/// The count alone says how bad it is; the substitution pairs say *what* to fix, which is
/// the part that turns a number into a decision.
fn compare(reference: &[String], hypothesis: &[String]) -> (usize, Vec<(String, String)>) {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut cost = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        cost[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let hit = reference[i - 1] == hypothesis[j - 1];
            cost[i][j] = if hit {
                cost[i - 1][j - 1]
            } else {
                1 + cost[i - 1][j - 1].min(cost[i - 1][j]).min(cost[i][j - 1])
            };
        }
    }

    // Walk back for the substitutions. Insertions and deletions are counted but not
    // named: a missing word has nothing to pair it with.
    let mut swaps = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if reference[i - 1] == hypothesis[j - 1] {
            i -= 1;
            j -= 1;
        } else if cost[i][j] == cost[i - 1][j - 1] + 1 {
            swaps.push((reference[i - 1].clone(), hypothesis[j - 1].clone()));
            i -= 1;
            j -= 1;
        } else if cost[i][j] == cost[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    swaps.reverse();
    (cost[n][m], swaps)
}

fn prompt_enter(message: &str) {
    use std::io::Write;
    print!("{message}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[allow(clippy::too_many_arguments)]
fn accuracy(
    models: &Path,
    speech_model: &str,
    sentences: &Path,
    dir: &Path,
    lang: &str,
    rerecord: bool,
    score_only: bool,
    hotwords: Option<&str>,
    hotword_boost: f32,
    use_vocabulary: bool,
) -> Result<()> {
    let lines = read_sentences(sentences)?;
    if lines.is_empty() {
        anyhow::bail!("{} has no sentences in it", sentences.display());
    }
    std::fs::create_dir_all(dir)?;

    if !score_only {
        println!(
            "\n{} sentences. For each one: press Enter, read it aloud, press Enter again.\n\
             Read it as written -- a word you skip counts against the recogniser.\n",
            lines.len()
        );
        for (i, line) in lines.iter().enumerate() {
            let wav = dir.join(format!("{:02}.wav", i + 1));
            if wav.exists() && !rerecord {
                continue;
            }
            println!("[{}/{}]  {line}", i + 1, lines.len());
            prompt_enter("         press Enter to start recording... ");
            let recorder = audio::Recorder::start("")?;
            prompt_enter("         recording, press Enter when done... ");
            let recording = recorder.finish(1.0)?;
            audio::write_wav(&wav, &recording.pcm, audio::TARGET_RATE)?;
            println!(
                "         {:.1}s, peak {:.1} dBFS\n",
                recording.pcm.len() as f32 / audio::TARGET_RATE as f32,
                recording.peak_db
            );
        }
    }

    let (engine, load_ms) = asr::Asr::load(&models.join(speech_model), threads())?;
    println!(
        "\nscoring against {} ({}ms, backend '{}')",
        speech_model,
        load_ms,
        engine.backend()
    );
    match hotwords {
        Some(terms) if !terms.trim().is_empty() => {
            engine.set_hotwords(terms, hotword_boost);
            println!("biasing toward (boost {hotword_boost}): {terms}");
        }
        _ => println!("no biasing"),
    }

    // The real vocabulary, not a synthetic one: the question is what this user's own
    // configured terms recover, not what some ideal list would.
    let vocabulary = if use_vocabulary {
        let (config, path, _) = lathe_core::config::Config::load_or_create()?;
        println!("repair pass on, vocabulary from {}", path.display());
        Some(config.vocabulary)
    } else {
        println!("repair pass off");
        None
    };
    println!();

    let (mut total_words, mut total_errors, mut scored) = (0usize, 0usize, 0usize);
    let mut all_swaps: Vec<(String, String)> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let wav = dir.join(format!("{:02}.wav", i + 1));
        if !wav.exists() {
            continue;
        }
        let pcm = audio::read_wav(&wav)?;
        let gated = match engine.gate(&pcm, &models.join(VAD_MODEL), MIN_SPEECH_MS)? {
            Some(g) => g.pcm,
            None => pcm,
        };
        let mut heard = engine.transcribe(&gated, lang)?.text;
        if let Some(vocabulary) = &vocabulary {
            heard = vocabulary.correct(&heard).0;
        }

        let reference = words(line);
        let hypothesis = words(&heard);
        let (errors, swaps) = compare(&reference, &hypothesis);

        total_words += reference.len();
        total_errors += errors;
        scored += 1;
        all_swaps.extend(swaps.iter().cloned());

        let rate = if reference.is_empty() {
            0.0
        } else {
            100.0 * errors as f32 / reference.len() as f32
        };
        let mark = if errors == 0 { "ok  " } else { "MISS" };
        println!("{mark} [{:.0}%] {line}", rate);
        if errors > 0 {
            println!("          heard: {heard}");
            for (want, got) in &swaps {
                println!("          {want} -> {got}");
            }
        }
    }

    if scored == 0 {
        anyhow::bail!("nothing recorded in {} yet", dir.display());
    }

    println!("\n----- {scored} sentences, {total_words} words -----");
    println!(
        "word error rate: {:.1}%  ({total_errors} errors)",
        100.0 * total_errors as f32 / total_words as f32
    );

    // The same word missed repeatedly is a vocabulary entry waiting to be written; a
    // scatter of one-offs is the model simply being what it is.
    let mut counts: std::collections::HashMap<(String, String), usize> = Default::default();
    for swap in all_swaps {
        *counts.entry(swap).or_default() += 1;
    }
    let mut repeated: Vec<_> = counts.into_iter().filter(|(_, n)| *n > 1).collect();
    if !repeated.is_empty() {
        repeated.sort_by(|a, b| b.1.cmp(&a.1));
        println!("\nmissed more than once:");
        for ((want, got), n) in repeated {
            println!("  {n}x  {want} -> {got}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_clause_is_content_loss() {
        // Amendment A23's Q4_K_M failure, verbatim.
        let f16 = words("Can you send me the file when you get a chance? I mean the one from yesterday, not the older one. Thanks.");
        let q4 = words("Can you send me the file from yesterday, not the older one? Thanks.");
        assert!(lost_content(&f16, &q4));
    }

    #[test]
    fn rewording_is_not_content_loss() {
        let f16 = words("Please send the report by Friday.");
        assert!(!lost_content(&f16, &words("Send the report by Friday, please.")));
        assert!(!lost_content(&f16, &words("Please send the summary by Friday.")));
        assert!(!lost_content(&f16, &f16));
        let repeated = words("Send the report. Send the report by Friday.");
        assert!(!lost_content(&repeated, &words("Send the report by Friday.")));
    }
}
