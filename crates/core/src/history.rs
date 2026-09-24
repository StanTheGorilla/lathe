// Transcript history and dictation statistics. Brief sections 6.4 and 6.7.
//
// A ring buffer of recent dictations, so a transcript that was pasted into the wrong
// window, or mangled, or simply wanted again, can be recovered without re-speaking it.
//
// Everything stays on this machine. Brief section 2 forbids telemetry, and this is the
// feature most likely to be mistaken for it: the file is local, nothing reads it but the
// settings window, and there is a wipe button.

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

pub struct History {
    conn: Connection,
    limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    pub preset: String,
    /// ISO 639-1 code the dictation was recognised in. Empty for rows recorded before
    /// the column existed.
    pub language: String,
    pub raw: String,
    pub cleaned: String,
    pub audio_secs: f32,
    pub asr_ms: i64,
    pub cleanup_ms: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub dictations: i64,
    pub words: i64,
    pub audio_secs: f64,
    /// Seconds saved against a 40 WPM typing baseline, brief 6.7.
    pub seconds_saved: f64,
    pub avg_latency_ms: f64,
    /// How much cleanup changed the raw text, as a fraction of characters. A rough proxy
    /// for how much work the normaliser is doing, not an accuracy measure.
    pub avg_change_ratio: f64,
}

/// Brief 6.7 names 40 WPM as the typing baseline to compare against.
const TYPING_WPM: f64 = 40.0;

impl History {
    pub fn open(path: &Path, limit: usize) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening {}", path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS dictations (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 at         INTEGER NOT NULL,
                 preset     TEXT    NOT NULL,
                 raw        TEXT    NOT NULL,
                 cleaned    TEXT    NOT NULL,
                 audio_secs REAL    NOT NULL,
                 asr_ms     INTEGER NOT NULL,
                 cleanup_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS dictations_at ON dictations (at DESC);",
        )
        .context("creating the history table")?;

        // Added after the table shipped. SQLite has no ADD COLUMN IF NOT EXISTS, so
        // look first.
        let has_language = conn
            .prepare("SELECT 1 FROM pragma_table_info('dictations') WHERE name = 'language'")?
            .exists([])?;
        if !has_language {
            conn.execute_batch(
                "ALTER TABLE dictations ADD COLUMN language TEXT NOT NULL DEFAULT ''",
            )
            .context("adding the language column")?;
        }

        Ok(Self { conn, limit })
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub fn record(
        &self,
        preset: &str,
        language: &str,
        raw: &str,
        cleaned: &str,
        audio_secs: f32,
        asr_ms: u128,
        cleanup_ms: u128,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.conn.execute(
            "INSERT INTO dictations (at, preset, language, raw, cleaned, audio_secs, asr_ms, cleanup_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now,
                preset,
                language,
                raw,
                cleaned,
                audio_secs as f64,
                asr_ms as i64,
                cleanup_ms as i64
            ],
        )?;

        // Ring buffer: trim anything past the retention limit. Statistics are computed
        // from these rows, so the totals only cover what is still kept -- which is the
        // honest thing to show given the user can wipe at any time.
        if self.limit > 0 {
            self.conn.execute(
                "DELETE FROM dictations WHERE id <= (
                     SELECT id FROM dictations ORDER BY id DESC LIMIT 1 OFFSET ?1
                 )",
                params![self.limit as i64],
            )?;
        }
        Ok(())
    }

    /// Most recent first. `search` matches either transcript, case-insensitively.
    pub fn recent(&self, limit: usize, search: &str) -> Result<Vec<Item>> {
        let pattern = format!("%{}%", search.trim());
        let mut statement = self.conn.prepare(
            "SELECT id, at, preset, language, raw, cleaned, audio_secs, asr_ms, cleanup_ms
             FROM dictations
             WHERE (?1 = '%%') OR raw LIKE ?1 COLLATE NOCASE OR cleaned LIKE ?1 COLLATE NOCASE
             ORDER BY id DESC LIMIT ?2",
        )?;

        let rows = statement.query_map(params![pattern, limit as i64], item_from_row)?;

        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Brief 6.7.
    pub fn stats(&self) -> Result<Stats> {
        let mut statement = self.conn.prepare(
            "SELECT cleaned, raw, audio_secs, asr_ms, cleanup_ms FROM dictations",
        )?;

        let mut stats = Stats::default();
        let mut change_total = 0.0f64;
        let mut latency_total = 0.0f64;

        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        for row in rows.filter_map(Result::ok) {
            let (cleaned, raw, audio_secs, asr_ms, cleanup_ms) = row;
            stats.dictations += 1;
            stats.words += cleaned.split_whitespace().count() as i64;
            stats.audio_secs += audio_secs;
            latency_total += (asr_ms + cleanup_ms) as f64;

            // Normalised edit distance between raw and cleaned.
            let longest = raw.chars().count().max(cleaned.chars().count());
            if longest > 0 {
                change_total += strsim::levenshtein(&raw, &cleaned) as f64 / longest as f64;
            }
        }

        if stats.dictations > 0 {
            stats.avg_latency_ms = latency_total / stats.dictations as f64;
            stats.avg_change_ratio = change_total / stats.dictations as f64;
        }

        // Time the same words would have taken to type, minus the time actually spent
        // speaking them. Speaking time is the honest thing to subtract; the processing
        // latency is already counted separately and is under a second.
        let typing_secs = (stats.words as f64 / TYPING_WPM) * 60.0;
        stats.seconds_saved = (typing_secs - stats.audio_secs).max(0.0);

        Ok(stats)
    }

    /// Replaces the cleaned text of one dictation with the user's correction. The raw
    /// transcript stays as heard: the pair is what the vocabulary learns from.
    pub fn correct(&self, id: i64, cleaned: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE dictations SET cleaned = ?2 WHERE id = ?1",
            params![id, cleaned],
        )?;
        Ok(())
    }

    pub fn wipe(&self) -> Result<()> {
        self.conn.execute("DELETE FROM dictations", [])?;
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let items = self.conn.query_row(
            "SELECT id, at, preset, language, raw, cleaned, audio_secs, asr_ms, cleanup_ms
             FROM dictations WHERE id = ?1",
            params![id],
            item_from_row,
        );
        match items {
            Ok(item) => Ok(Some(item)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// The words a correction swapped one for one, as (heard, wanted).
///
/// Open work item 4, option 2: a hand-fixed word is a mishearing the vocabulary could
/// carry as a spoken form, so the app offers it. The two texts are aligned word by
/// word, so a correction that also fixed punctuation, dropped a filler or swapped
/// several words still yields each swap. Only a single word replaced by a single word
/// counts -- the correction pass works one word at a time -- and a change of case or
/// punctuation alone is a styling matter, not a recognition error.
pub fn word_changes(before: &str, after: &str) -> Vec<(String, String)> {
    let strip = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
            .to_string()
    };
    let a: Vec<String> = before.split_whitespace().map(strip).filter(|w| !w.is_empty()).collect();
    let b: Vec<String> = after.split_whitespace().map(strip).filter(|w| !w.is_empty()).collect();
    let key = |w: &String| w.to_lowercase();

    // Longest common subsequence, ignoring case, then walk it: every stretch between
    // two matched words is what the correction replaced.
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if key(&a[i]) == key(&b[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut changes = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut gap_a, mut gap_b) = (Vec::new(), Vec::new());
    // Cleanup keeps some fillers and the user deletes them in the same edit; one
    // sitting beside a swapped word must not hide the swap.
    let filler = |w: &&String| {
        matches!(w.to_lowercase().as_str(), "um" | "uh" | "er" | "erm" | "ah" | "hmm" | "mm")
    };
    let mut flush = |gap_a: &mut Vec<&String>, gap_b: &mut Vec<&String>| {
        gap_a.retain(|w| !filler(w));
        if let ([heard], [wanted]) = (gap_a.as_slice(), gap_b.as_slice()) {
            changes.push(((*heard).clone(), (*wanted).clone()));
        }
        gap_a.clear();
        gap_b.clear();
    };
    while i < n || j < m {
        if i < n && j < m && key(&a[i]) == key(&b[j]) {
            flush(&mut gap_a, &mut gap_b);
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            gap_b.push(&b[j]);
            j += 1;
        } else {
            gap_a.push(&a[i]);
            i += 1;
        }
    }
    flush(&mut gap_a, &mut gap_b);
    changes
}

fn item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        at: row.get(1)?,
        preset: row.get(2)?,
        language: row.get(3)?,
        raw: row.get(4)?,
        cleaned: row.get(5)?,
        audio_secs: row.get::<_, f64>(6)? as f32,
        asr_ms: row.get(7)?,
        cleanup_ms: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database of its own per test. Tests run in parallel threads of one process, so
    /// keying the filename on the process id alone had them all opening the same file
    /// and deadlocking each other.
    fn temp() -> History {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "lathe-test-{}-{}.db",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        History::open(&path, 3).unwrap()
    }

    #[test]
    fn keeps_only_the_retention_limit() {
        let h = temp();
        for i in 0..6 {
            h.record("Prompt", "en", &format!("raw {i}"), &format!("clean {i}"), 1.0, 10, 5)
                .unwrap();
        }
        let items = h.recent(100, "").unwrap();
        assert_eq!(items.len(), 3);
        // Newest first, and the oldest three are gone.
        assert_eq!(items[0].cleaned, "clean 5");
    }

    #[test]
    fn searches_both_transcripts() {
        let h = temp();
        h.record("Prompt", "en", "needle in raw", "nothing here", 1.0, 1, 1)
            .unwrap();
        h.record("Prompt", "en", "nothing", "needle in cleaned", 1.0, 1, 1)
            .unwrap();
        assert_eq!(h.recent(100, "needle").unwrap().len(), 2);
        assert_eq!(h.recent(100, "absent").unwrap().len(), 0);
    }

    #[test]
    fn counts_words_and_time_saved() {
        let h = temp();
        // 40 words at 40 WPM is one minute of typing; spoken in 10 seconds.
        let text = (0..40).map(|_| "word").collect::<Vec<_>>().join(" ");
        h.record("Prompt", "en", &text, &text, 10.0, 100, 50).unwrap();
        let s = h.stats().unwrap();
        assert_eq!(s.words, 40);
        assert!((s.seconds_saved - 50.0).abs() < 0.01, "got {}", s.seconds_saved);
        assert!((s.avg_latency_ms - 150.0).abs() < 0.01);
    }

    /// A database from before the column existed opens, gains it, and keeps its rows.
    #[test]
    fn an_old_database_gains_the_language_column() {
        let path = std::env::temp_dir().join(format!("lathe-test-old-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE dictations (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL,
                     preset TEXT NOT NULL, raw TEXT NOT NULL, cleaned TEXT NOT NULL,
                     audio_secs REAL NOT NULL, asr_ms INTEGER NOT NULL,
                     cleanup_ms INTEGER NOT NULL);
                 INSERT INTO dictations VALUES (1, 0, 'Prompt', 'old', 'Old.', 1.0, 1, 1);",
            )
            .unwrap();
        }
        let h = History::open(&path, 10).unwrap();
        h.record("Prompt", "pl", "nowy", "Nowy.", 1.0, 1, 1).unwrap();
        let items = h.recent(10, "").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].language, "pl");
        assert_eq!(items[1].language, "", "rows from before carry no language");
        // Opening again must not try to add the column twice.
        drop(h);
        History::open(&path, 10).unwrap();
    }

    #[test]
    fn a_correction_replaces_the_cleaned_text_only() {
        let h = temp();
        h.record("Prompt", "en", "open grappify", "Open Grappify.", 1.0, 1, 1)
            .unwrap();
        let id = h.recent(1, "").unwrap()[0].id;
        h.correct(id, "Open graphify.").unwrap();
        let item = h.get(id).unwrap().unwrap();
        assert_eq!(item.cleaned, "Open graphify.");
        assert_eq!(item.raw, "open grappify");
    }

    #[test]
    fn one_substituted_word_is_offered_as_a_spoken_form() {
        assert_eq!(
            word_changes("Open Grappify, please.", "Open graphify, please."),
            vec![("Grappify".into(), "graphify".into())]
        );
        // Punctuation around the word is not part of it.
        assert_eq!(
            word_changes("It was Lata.", "It was Lathe."),
            vec![("Lata".into(), "Lathe".into())]
        );
    }

    /// A real correction rarely touches one word only: it drops a filler, fixes a
    /// comma, and swaps a name or two, all at once.
    #[test]
    fn every_swap_in_a_larger_correction_is_found() {
        assert_eq!(
            word_changes(
                "So I asked cloud, um, to port it to rest.",
                "So I asked Claude to port it to Rust."
            ),
            vec![
                ("cloud".into(), "Claude".into()),
                ("rest".into(), "Rust".into())
            ]
        );
    }

    #[test]
    fn nothing_but_a_one_for_one_swap_is_offered() {
        assert!(word_changes("a b c", "a b c").is_empty(), "nothing changed");
        assert!(word_changes("a b c", "a b").is_empty(), "a word removed");
        assert!(word_changes("a b c", "a b c d").is_empty(), "a word added");
        assert!(word_changes("a lathe c", "a Lathe c").is_empty(), "case only");
        assert!(word_changes("a b, c", "a b. c").is_empty(), "punctuation only");
        assert!(word_changes("a b c", "a , c").is_empty(), "replaced by nothing");
        assert!(word_changes("a b c d", "a x y d").is_empty(), "two words for two");
        assert!(word_changes("a b d", "a x y d").is_empty(), "one word for two");
    }

    #[test]
    fn wipe_empties_it() {
        let h = temp();
        h.record("Prompt", "en", "a", "b", 1.0, 1, 1).unwrap();
        h.wipe().unwrap();
        assert_eq!(h.recent(10, "").unwrap().len(), 0);
        assert_eq!(h.stats().unwrap().dictations, 0);
    }
}
