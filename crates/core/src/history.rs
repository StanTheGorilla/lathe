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

        Ok(Self { conn, limit })
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub fn record(
        &self,
        preset: &str,
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
            "INSERT INTO dictations (at, preset, raw, cleaned, audio_secs, asr_ms, cleanup_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                now,
                preset,
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
            "SELECT id, at, preset, raw, cleaned, audio_secs, asr_ms, cleanup_ms
             FROM dictations
             WHERE (?1 = '%%') OR raw LIKE ?1 COLLATE NOCASE OR cleaned LIKE ?1 COLLATE NOCASE
             ORDER BY id DESC LIMIT ?2",
        )?;

        let rows = statement.query_map(params![pattern, limit as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                at: row.get(1)?,
                preset: row.get(2)?,
                raw: row.get(3)?,
                cleaned: row.get(4)?,
                audio_secs: row.get::<_, f64>(5)? as f32,
                asr_ms: row.get(6)?,
                cleanup_ms: row.get(7)?,
            })
        })?;

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

    pub fn wipe(&self) -> Result<()> {
        self.conn.execute("DELETE FROM dictations", [])?;
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let items = self.conn.query_row(
            "SELECT id, at, preset, raw, cleaned, audio_secs, asr_ms, cleanup_ms
             FROM dictations WHERE id = ?1",
            params![id],
            |row| {
                Ok(Item {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    preset: row.get(2)?,
                    raw: row.get(3)?,
                    cleaned: row.get(4)?,
                    audio_secs: row.get::<_, f64>(5)? as f32,
                    asr_ms: row.get(6)?,
                    cleanup_ms: row.get(7)?,
                })
            },
        );
        match items {
            Ok(item) => Ok(Some(item)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
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
            h.record("Prompt", &format!("raw {i}"), &format!("clean {i}"), 1.0, 10, 5)
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
        h.record("Prompt", "needle in raw", "nothing here", 1.0, 1, 1)
            .unwrap();
        h.record("Prompt", "nothing", "needle in cleaned", 1.0, 1, 1)
            .unwrap();
        assert_eq!(h.recent(100, "needle").unwrap().len(), 2);
        assert_eq!(h.recent(100, "absent").unwrap().len(), 0);
    }

    #[test]
    fn counts_words_and_time_saved() {
        let h = temp();
        // 40 words at 40 WPM is one minute of typing; spoken in 10 seconds.
        let text = (0..40).map(|_| "word").collect::<Vec<_>>().join(" ");
        h.record("Prompt", &text, &text, 10.0, 100, 50).unwrap();
        let s = h.stats().unwrap();
        assert_eq!(s.words, 40);
        assert!((s.seconds_saved - 50.0).abs() < 0.01, "got {}", s.seconds_saved);
        assert!((s.avg_latency_ms - 150.0).abs() < 0.01);
    }

    #[test]
    fn wipe_empties_it() {
        let h = temp();
        h.record("Prompt", "a", "b", 1.0, 1, 1).unwrap();
        h.wipe().unwrap();
        assert_eq!(h.recent(10, "").unwrap().len(), 0);
        assert_eq!(h.stats().unwrap().dictations, 0);
    }
}
