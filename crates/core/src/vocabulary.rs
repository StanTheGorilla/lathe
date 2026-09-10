// Vocabulary. Brief section 5.5, reworked per amendment A24.
//
// A vocabulary is a list of words. That is the whole model: you add a term you want
// spelled correctly, and it gets spelled correctly. There is no "heard as" column.
//
// It used to have one. That design let a user map "coherent transcript" onto "Cohere
// Transcribe", which worked right up until they genuinely meant "coherent transcript" --
// and then there was no way to say so. A vocabulary entry must never be able to destroy
// words the speaker actually said.
//
// So the work is split:
//
//   Pass 1 biases the recogniser before it decides anything, through CrispASR's
//   contextual hotwords. Nearly all the value is here: a term boosted during decoding is
//   simply transcribed correctly, and nothing has to be repaired afterwards.
//
//   Pass 2 is a narrow safety net for what pass 1 missed. It only touches a word when a
//   vocabulary term is its *exact phonetic twin* and within a small edit distance. That
//   pairing is deliberately strict. "cohere" and "coherent" encode differently (KHR
//   against KHRNT), so the pass leaves "coherent" alone -- which is exactly what the old
//   explicit mapping got wrong.

use rphonetic::{DoubleMetaphone, Encoder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A named group of terms that presets enable independently, so Polish surnames do not
/// corrupt English technical dictation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Set {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// One term per entry. Multi-word terms are used for recogniser biasing but are not
    /// candidates for the correction pass, which works one word at a time.
    #[serde(default, alias = "entries")]
    pub terms: Vec<Term>,
}

fn yes() -> bool {
    true
}

/// A word to spell correctly, and optionally the words the recogniser puts in its place.
///
/// `heard` is the narrow, opt-in exception to everything the module header says about
/// never destroying a word the speaker said. The phonetic pass below is deliberately
/// strict, and there are pairs it can never reach: "cloud" and "Claude" are phonetic
/// twins, but two edits apart, and the budget for a five-letter word is one. Loosening
/// the budget to reach them turns "roast" into "Rust".
///
/// So the pairing is named rather than inferred. A word only becomes a term when the
/// user has written that exact pairing down, which is what makes it safe: the cost is
/// paid knowingly, on one word, in one set, that presets can switch off. That is the
/// difference from the mapping amendment A24 removed -- that one applied to phrases and
/// could eat words nobody had thought about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub write: String,
    /// Words that mean this term when heard. Matched whole and case-insensitively;
    /// multi-word entries never match, since the pass works one word at a time.
    pub heard: Vec<String>,
}

impl Term {
    pub fn new(write: impl Into<String>) -> Self {
        Self {
            write: write.into(),
            heard: Vec::new(),
        }
    }

    pub fn heard(write: impl Into<String>, heard: &[&str]) -> Self {
        Self {
            write: write.into(),
            heard: heard.iter().map(|h| h.to_string()).collect(),
        }
    }
}

/// A term with no spoken forms is written back as a bare string, so the common case
/// keeps the config file readable and a hand-edited list stays a hand-edited list.
impl Serialize for Term {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if self.heard.is_empty() {
            return serializer.serialize_str(&self.write);
        }
        let mut st = serializer.serialize_struct("Term", 2)?;
        st.serialize_field("write", &self.write)?;
        st.serialize_field("heard", &self.heard)?;
        st.end()
    }
}

/// Reads a bare string, and the object shape that both this and the pre-A24 config used.
/// An old config keeps its vocabulary instead of silently emptying itself.
impl<'de> Deserialize<'de> for Term {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Bare(String),
            Full {
                write: String,
                #[serde(default)]
                heard: Vec<String>,
            },
        }

        Ok(match Shape::deserialize(deserializer)? {
            Shape::Bare(write) => Term::new(write),
            Shape::Full { write, heard } => Term { write, heard },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Vocabulary {
    /// The correction pass. Biasing always runs; this is the repair afterwards.
    pub correction_enabled: bool,
    /// Maximum edit distance for a correction, as a fraction of the word's length.
    /// Conservative by default: roughly one wrong letter in three.
    pub max_distance_ratio: f32,
    /// How hard to bias the recogniser toward these terms.
    pub hotword_boost: f32,
    pub sets: Vec<Set>,
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self {
            correction_enabled: true,
            max_distance_ratio: 0.34,
            hotword_boost: 2.0,
            sets: default_sets(),
        }
    }
}

impl Vocabulary {
    pub fn enabled_terms(&self) -> Vec<&Term> {
        self.sets
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| s.terms.iter())
            .filter(|t| !t.write.trim().is_empty())
            .collect()
    }

    /// Pass 1. Comma-separated terms for contextual biasing.
    ///
    /// Capped: biasing toward a list longer than the utterance stops helping and starts
    /// pulling unrelated words toward vocabulary entries.
    pub fn hotwords(&self, limit: usize) -> String {
        let mut terms = self.enabled_terms();
        terms.truncate(limit);
        terms
            .iter()
            .map(|t| t.write.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Pass 2. Returns the corrected text and how many words changed.
    pub fn correct(&self, text: &str) -> (String, usize) {
        if !self.correction_enabled || text.is_empty() {
            return (text.to_string(), 0);
        }
        let terms = self.enabled_terms();
        if terms.is_empty() {
            return (text.to_string(), 0);
        }
        Corrector::new(&terms, self.max_distance_ratio).apply(text)
    }

    /// The view of this vocabulary a preset sees. An empty selection means every set
    /// that is enabled globally; a non-empty one means exactly those sets, named.
    pub fn for_preset(&self, selected: &[String]) -> Vocabulary {
        if selected.is_empty() {
            return self.clone();
        }
        Vocabulary {
            correction_enabled: self.correction_enabled,
            max_distance_ratio: self.max_distance_ratio,
            hotword_boost: self.hotword_boost,
            sets: self
                .sets
                .iter()
                .filter(|s| selected.iter().any(|n| n.eq_ignore_ascii_case(&s.name)))
                .cloned()
                .collect(),
        }
    }
}

struct Corrector<'a> {
    /// (lowercased term, phonetic key, the term as written).
    candidates: Vec<(String, String, &'a str)>,
    /// Lowercased spoken form -> the term to write. Checked before the phonetic pass:
    /// the user named this pairing, so it outranks anything inferred.
    spoken: HashMap<String, &'a str>,
    max_distance_ratio: f32,
    phonetic: DoubleMetaphone,
}

impl<'a> Corrector<'a> {
    fn new(terms: &[&'a Term], max_distance_ratio: f32) -> Self {
        let phonetic = DoubleMetaphone::default();
        let candidates = terms
            .iter()
            // Single words only. The pass compares one spoken word at a time, and
            // matching a word against a phrase produces nonsense.
            .filter(|t| t.write.split_whitespace().count() == 1)
            .filter_map(|t| {
                let lower = t.write.to_lowercase();
                let code = phonetic.encode(&fold_to_ascii(&lower)?);
                Some((lower, code, t.write.as_str()))
            })
            .collect();

        let mut spoken = HashMap::new();
        for term in terms {
            for form in &term.heard {
                let form = form.trim().to_lowercase();
                // Same one-word rule as the candidates, for the same reason.
                if form.is_empty() || form.split_whitespace().count() != 1 {
                    continue;
                }
                // A term never rewrites itself.
                if form == term.write.to_lowercase() {
                    continue;
                }
                spoken.entry(form).or_insert(term.write.as_str());
            }
        }

        Self {
            candidates,
            spoken,
            max_distance_ratio,
            phonetic,
        }
    }

    fn apply(&self, text: &str) -> (String, usize) {
        let mut out = String::with_capacity(text.len());
        let mut replacements = 0;

        for token in tokenize(text) {
            match token {
                Token::Gap(gap) => out.push_str(gap),
                Token::Word(word) => match self.lookup(word) {
                    Some(term) => {
                        out.push_str(&match_case(word, term));
                        replacements += 1;
                    }
                    None => out.push_str(word),
                },
            }
        }

        (out, replacements)
    }

    /// A named spoken form first, then the phonetic net.
    fn lookup(&self, word: &str) -> Option<&'a str> {
        if let Some(term) = self.spoken.get(&word.to_lowercase()) {
            return Some(term);
        }
        self.best_match(word)
    }

    /// A candidate must be phonetically identical *and* within the edit-distance budget.
    /// Either test alone is far too eager: phonetic keys collide often, and edit distance
    /// happily turns "form" into "from".
    fn best_match(&self, word: &str) -> Option<&'a str> {
        let lower = word.to_lowercase();
        // Short words are mostly function words, and one edit reaches half the dictionary
        // from there.
        if lower.chars().count() < 4 {
            return None;
        }

        // rphonetic's Double Metaphone slices by byte offset and panics outright on a
        // multi-byte character, so nothing non-ASCII may reach it. Folding rather than
        // skipping is what lets a Polish name in the vocabulary still match the
        // recogniser's unaccented guess at it.
        let folded = fold_to_ascii(&lower)?;
        let code = self.phonetic.encode(&folded);
        if code.is_empty() {
            return None;
        }

        let budget = ((lower.chars().count() as f32) * self.max_distance_ratio).floor() as usize;
        if budget == 0 {
            return None;
        }

        let mut best: Option<(usize, &'a str)> = None;
        for (term, term_code, written) in &self.candidates {
            // Already correct, ignoring case. Nothing to do, and nothing else may claim
            // this word either.
            if *term == lower {
                return None;
            }
            if *term_code != code {
                continue;
            }
            let distance = strsim::levenshtein(&lower, term);
            if distance > budget {
                continue;
            }
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, written));
            }
        }
        best.map(|(_, written)| written)
    }
}

/// Strips Latin diacritics so a word can be handed to the phonetic encoder.
///
/// Returns `None` for anything that is not Latin script, which is the signal to leave
/// the word alone: the encoder is an English phonetic model and has nothing useful to
/// say about it.
fn fold_to_ascii(word: &str) -> Option<String> {
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        let base = match c {
            'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
            'ď' | 'đ' => "d",
            'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
            'ĥ' | 'ħ' => "h",
            'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
            'ĵ' => "j",
            'ķ' => "k",
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
            'ñ' | 'ń' | 'ņ' | 'ň' => "n",
            'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
            'ŕ' | 'ŗ' | 'ř' => "r",
            'ś' | 'ŝ' | 'ş' | 'š' => "s",
            'ţ' | 'ť' | 'ŧ' => "t",
            'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
            'ŵ' => "w",
            'ý' | 'ÿ' | 'ŷ' => "y",
            'ź' | 'ż' | 'ž' => "z",
            'æ' => "ae",
            'œ' => "oe",
            'ß' => "ss",
            'þ' => "th",
            'ð' => "d",
            _ => return None,
        };
        out.push_str(base);
    }
    Some(out)
}

enum Token<'a> {
    Word(&'a str),
    Gap(&'a str),
}

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut in_word = false;

    for (i, c) in text.char_indices() {
        // Apostrophes are part of a word: "don't" is one token, not two.
        let is_word = c.is_alphanumeric() || c == '\'' || c == '\u{2019}';
        if is_word != in_word {
            if i > start {
                tokens.push(if in_word {
                    Token::Word(&text[start..i])
                } else {
                    Token::Gap(&text[start..i])
                });
            }
            start = i;
            in_word = is_word;
        }
    }
    if start < text.len() {
        tokens.push(if in_word {
            Token::Word(&text[start..])
        } else {
            Token::Gap(&text[start..])
        });
    }
    tokens
}

/// Keeps a sentence-initial capital when the recogniser produced one, but never
/// overrides a term's own casing: `llama.cpp` stays lowercase mid-sentence, and
/// `SvelteKit` keeps its internal capital.
fn match_case(original: &str, term: &str) -> String {
    let first_upper = original.chars().next().is_some_and(|c| c.is_uppercase());
    let term_starts_lower = term.chars().next().is_some_and(|c| c.is_lowercase());

    if first_upper && term_starts_lower {
        let mut chars = term.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => term.to_string(),
        }
    } else {
        term.to_string()
    }
}

/// The sets shipped by default: terms that appear throughout this project and would be
/// spoken while working on it.
fn default_sets() -> Vec<Set> {
    let strings = |items: &[&str]| items.iter().map(|s| Term::new(*s)).collect::<Vec<_>>();

    vec![
        Set {
            name: "lathe".into(),
            enabled: true,
            terms: strings(&[
                "Cohere",
                "Cohere Transcribe",
                "Lathe",
                "S1-mini",
                "Superwhisper",
                "CrispASR",
                "Silero",
                "Whisper",
                "GGUF",
                "VAD",
            ]),
        },
        Set {
            name: "dev".into(),
            enabled: true,
            terms: strings(&[
                "llama.cpp",
                "whisper.cpp",
                "ggml",
                "Vulkan",
                "CUDA",
                "ROCm",
                "Tauri",
                "Svelte",
                "SvelteKit",
                "Vite",
                "Rust",
                "cargo",
                "clippy",
                "MSVC",
                "CMake",
                "Ninja",
                "komorebi",
                "WASAPI",
                "cpal",
                "npm",
                "TOML",
                "JSON",
                "SQLite",
                "PostgreSQL",
                "Kubernetes",
                "Docker",
                "regex",
                "async",
                "stdout",
                "stderr",
                "repo",
                "commit",
                "branch",
                "rebase",
                "refactor",
                "linter",
                "GitHub",
                "Levenshtein",
                "Metaphone",
                "quantization",
                "inference",
                "latency",
                "throughput",
                "Anthropic",
                "Claude",
                "OpenAI",
                "HuggingFace",
            ]),
        },
    ]
}

/// Applies a preset's replacement rules. Brief 6.8.
///
/// Deterministic and model-free: literal by default, regex when the rule says so. This
/// is the escape hatch for anything vocabulary deliberately will not do, including
/// unconditional phrase substitution.
pub fn apply_replacements(text: &str, rules: &[Replacement]) -> String {
    let mut out = text.to_string();
    for rule in rules {
        if rule.find.is_empty() {
            continue;
        }
        if rule.regex {
            match regex::Regex::new(&rule.find) {
                Ok(re) => out = re.replace_all(&out, rule.replace.as_str()).into_owned(),
                // A bad pattern is the user's typo, not a reason to lose the dictation.
                Err(e) => eprintln!("replacement rule '{}' is not valid regex: {e}", rule.find),
            }
        } else {
            out = out.replace(&rule.find, &rule.replace);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replacement {
    pub find: String,
    pub replace: String,
    #[serde(default)]
    pub regex: bool,
}

pub fn default_replacements() -> Vec<Replacement> {
    vec![
        Replacement {
            find: "new paragraph".into(),
            replace: "\n\n".into(),
            regex: false,
        },
        Replacement {
            find: "new line".into(),
            replace: "\n".into(),
            regex: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocabulary {
        Vocabulary::default()
    }

    #[test]
    fn corrects_phonetically_identical_slips() {
        let (out, _) = vocab().correct("I built it with vulcan and svelt");
        assert!(out.contains("Vulkan"), "got {out}");
        assert!(out.contains("Svelte"), "got {out}");
    }

    /// The reason the "heard as" column was removed. A vocabulary entry must never be
    /// able to eat a word the speaker actually said.
    #[test]
    fn leaves_a_real_word_alone_even_when_a_term_looks_similar() {
        // "Cohere" is in the default vocabulary; "coherent" is a different word and must
        // survive. Their phonetic keys differ, which is what protects it.
        let (out, n) = vocab().correct("that was a coherent argument");
        assert_eq!(out, "that was a coherent argument");
        assert_eq!(n, 0);
    }

    #[test]
    fn leaves_ordinary_prose_alone() {
        let text = "the quick brown fox jumped over the lazy dog and then went home";
        let (out, n) = vocab().correct(text);
        assert_eq!(out, text);
        assert_eq!(n, 0);
    }

    #[test]
    fn preserves_punctuation_and_spacing() {
        let (out, _) = vocab().correct("Use vulcan, not CUDA.");
        assert_eq!(out, "Use Vulkan, not CUDA.");
    }

    #[test]
    fn keeps_a_sentence_initial_capital() {
        let (out, _) = vocab().correct("Vulcan is the backend");
        assert_eq!(out, "Vulkan is the backend");
    }

    #[test]
    fn a_term_already_spelled_correctly_is_untouched() {
        let (out, n) = vocab().correct("Vulkan and Svelte are fine");
        assert_eq!(out, "Vulkan and Svelte are fine");
        assert_eq!(n, 0);
    }

    #[test]
    fn reads_the_old_config_shape() {
        // Old configs stored objects with a `write` field. They must still load.
        let toml = r#"
            name = "old"
            enabled = true
            entries = [{ write = "SvelteKit", heard = ["svelt kit"] }, { write = "Vulkan" }]
        "#;
        let set: Set = toml::from_str(toml).unwrap();
        assert_eq!(
            set.terms,
            vec![Term::heard("SvelteKit", &["svelt kit"]), Term::new("Vulkan")]
        );
    }

    /// The pairing the phonetic pass can never reach on its own: twins, but two edits
    /// apart, against a budget of one.
    #[test]
    fn a_named_spoken_form_is_corrected() {
        let vocab = Vocabulary {
            sets: vec![Set {
                name: "ai".into(),
                enabled: true,
                terms: vec![Term::heard("Claude", &["cloud"])],
            }],
            ..Vocabulary::default()
        };
        let (out, n) = vocab.correct("I asked cloud about it");
        assert_eq!(out, "I asked Claude about it");
        assert_eq!(n, 1);
        // Sentence-initial, and the term's own casing still wins.
        let (out, _) = vocab.correct("Cloud said no");
        assert_eq!(out, "Claude said no");
    }

    /// The cost is paid only where it was asked for. Nothing else gets looser, and a set
    /// that is switched off gives the word back.
    #[test]
    fn a_spoken_form_does_not_loosen_anything_else() {
        let vocab = Vocabulary {
            sets: vec![Set {
                name: "ai".into(),
                enabled: true,
                terms: vec![Term::heard("Claude", &["cloud"]), Term::new("Rust")],
            }],
            ..Vocabulary::default()
        };
        // "roast" and "rust" are phonetic twins two edits apart, exactly like
        // cloud/claude. Naming one pairing must not reach the other.
        let (out, n) = vocab.correct("I roast the beans");
        assert_eq!(out, "I roast the beans");
        assert_eq!(n, 0);

        let mut off = vocab.clone();
        off.sets[0].enabled = false;
        assert_eq!(off.correct("I asked cloud about it").1, 0);
    }

    #[test]
    fn a_multi_word_spoken_form_is_ignored() {
        // The pass works one word at a time, so a phrase could only ever half-match.
        let vocab = Vocabulary {
            sets: vec![Set {
                name: "ai".into(),
                enabled: true,
                terms: vec![Term::heard("Claude", &["cloud model"])],
            }],
            ..Vocabulary::default()
        };
        assert_eq!(vocab.correct("the cloud model is fine").1, 0);
    }

    /// The config file is hand-editable, and a term without spoken forms must stay a
    /// bare string in it rather than turning the whole list into tables.
    #[test]
    fn terms_round_trip_through_toml() {
        let set = Set {
            name: "ai".into(),
            enabled: true,
            terms: vec![Term::new("Vulkan"), Term::heard("Claude", &["cloud"])],
        };
        let text = toml::to_string(&set).unwrap();
        assert!(text.contains(r#""Vulkan""#), "got {text}");
        let back: Set = toml::from_str(&text).unwrap();
        assert_eq!(back.terms, set.terms);
    }

    #[test]
    fn replacements_are_literal_by_default() {
        let rules = default_replacements();
        let out = apply_replacements("first line new paragraph second line", &rules);
        assert_eq!(out, "first line \n\n second line");
    }

    #[test]
    fn polish_diacritics_survive_the_correction_pass() {
        // Every word here is over three characters and accented, which is exactly what
        // used to reach the phonetic encoder and take the whole dictation down.
        let text = "No wi\u{119}c dzisiaj pr\u{f3}bowa\u{142}em zrobi\u{107} porz\u{105}dek, ale si\u{119} nie uda\u{142}o \u{17c}eby \u{17a}le";
        let (out, _) = vocab().correct(text);
        assert_eq!(out, text);
    }

    #[test]
    fn an_accented_vocabulary_term_still_matches_an_unaccented_guess() {
        let vocab = Vocabulary {
            sets: vec![Set {
                name: "pl".into(),
                enabled: true,
                terms: vec![Term::new("Wa\u{142}\u{119}sa")],
            }],
            ..Vocabulary::default()
        };
        let (out, n) = vocab.correct("rozmawia\u{142}em z Walesa wczoraj");
        assert_eq!(n, 1, "got {out}");
        assert!(out.contains("Wa\u{142}\u{119}sa"), "got {out}");
    }

    #[test]
    fn a_broken_regex_does_not_lose_the_text() {
        let rules = vec![Replacement {
            find: "([unclosed".into(),
            replace: "x".into(),
            regex: true,
        }];
        assert_eq!(apply_replacements("keep me", &rules), "keep me");
    }
}
