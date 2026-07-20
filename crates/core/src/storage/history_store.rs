use std::path::Path;

use rusqlite::Connection;

use autoreview_schema::{FeedbackVerdict, ReviewReport};

/// Embedded SQLite index over the event log (`rusqlite`, `bundled` feature —
/// no separate native-module install step, since SQLite compiles directly
/// into the binary). Per the plan's Storage design, this is a *derived*
/// cache, never the source of truth: `history rebuild` (M2) can always
/// reconstruct it from the flat-file event log alone. M1 only stands up the
/// schema and the write path; queries land in M2 once `feedback` needs them.
pub struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    pub fn open(history_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(history_dir)?;
        let conn = Connection::open(history_dir.join("index.db"))?;
        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// In-memory store for tests — same schema, no file on disk.
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    fn ensure_schema(&self) -> anyhow::Result<()> {
        // `valid_from`/`invalid_at` on findings follow the plan's bi-temporal
        // note (Storage section): rows are superseded, not overwritten, so
        // "why did run X flag this" stays answerable from the table itself.
        // Nothing currently sets `invalid_at` in M1 — that's the demotion/
        // rollback machinery in M3 — but the column exists from the start so
        // a later migration doesn't have to retrofit it onto live data.
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS findings (
                row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                finding_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                category TEXT NOT NULL,
                severity TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_tool TEXT NOT NULL,
                rule_id TEXT,
                run_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                valid_from TEXT NOT NULL,
                invalid_at TEXT,
                title TEXT NOT NULL DEFAULT '',
                message TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_findings_fingerprint ON findings(fingerprint);
            CREATE INDEX IF NOT EXISTS idx_findings_run_id ON findings(run_id);
            CREATE INDEX IF NOT EXISTS idx_findings_finding_id ON findings(finding_id);

            CREATE TABLE IF NOT EXISTS feedback (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                finding_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                category TEXT NOT NULL,
                rule_id_or_aspect TEXT NOT NULL,
                severity TEXT NOT NULL,
                verdict TEXT NOT NULL,
                note TEXT,
                created_at TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                message TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_feedback_finding_id ON feedback(finding_id);

            CREATE TABLE IF NOT EXISTS rules (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                firings INTEGER NOT NULL DEFAULT 0,
                agent_agreed INTEGER NOT NULL DEFAULT 0,
                agent_disagreed INTEGER NOT NULL DEFAULT 0,
                valid_from TEXT NOT NULL,
                invalid_at TEXT
            );

            CREATE TABLE IF NOT EXISTS skill_versions (
                aspect TEXT NOT NULL,
                version TEXT NOT NULL,
                source TEXT,
                valid_from TEXT NOT NULL,
                invalid_at TEXT,
                PRIMARY KEY (aspect, version)
            );

            CREATE TABLE IF NOT EXISTS embeddings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                fingerprint TEXT NOT NULL,
                verdict TEXT NOT NULL,
                embedding BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_embeddings_verdict ON embeddings(verdict);

            CREATE TABLE IF NOT EXISTS shadow_firings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                run_id TEXT NOT NULL,
                location_path TEXT NOT NULL,
                location_line INTEGER NOT NULL,
                agreement TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_shadow_firings_rule_id ON shadow_firings(rule_id);
            ",
        )?;
        // Best-effort migration for a pre-existing `findings` table created
        // before `title`/`message` were added — `CREATE TABLE IF NOT EXISTS`
        // above is a no-op against an already-existing table, so these
        // columns need their own add-if-missing step. Errors (column already
        // present) are expected and ignored.
        let _ = self.conn.execute("ALTER TABLE findings ADD COLUMN title TEXT NOT NULL DEFAULT ''", []);
        let _ = self.conn.execute("ALTER TABLE findings ADD COLUMN message TEXT NOT NULL DEFAULT ''", []);
        let _ = self.conn.execute("ALTER TABLE feedback ADD COLUMN title TEXT NOT NULL DEFAULT ''", []);
        let _ = self.conn.execute("ALTER TABLE feedback ADD COLUMN message TEXT NOT NULL DEFAULT ''", []);
        Ok(())
    }

    /// Writes one row per finding for this run. Write-only in M1 — no
    /// `query_findings`/`nearest_by_embedding` yet (M2), but the schema and
    /// insert path exist now so M2's read path has real data to query
    /// against from day one rather than starting from an empty table.
    pub fn record_run(&self, report: &ReviewReport) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for finding in &report.findings {
            tx.execute(
                "INSERT INTO findings (finding_id, fingerprint, category, severity, source_kind, source_tool, rule_id, run_id, created_at, valid_from, title, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)",
                rusqlite::params![
                    finding.id,
                    finding.fingerprints.primary,
                    finding.category,
                    format!("{:?}", finding.severity).to_lowercase(),
                    format!("{:?}", finding.source.kind).to_lowercase(),
                    finding.source.tool,
                    finding.source.rule_id,
                    report.run_id,
                    report.created_at,
                    finding.title,
                    finding.message,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The most recently recorded run's id, by `created_at` — must be called
    /// *before* `record_run` for the current run, or it returns itself.
    /// Backs `--incremental`: `diff` calls this first, then
    /// `fingerprints_for_run` on the result, to know what the *previous*
    /// review on this repo already reported.
    pub fn most_recent_run_id(&self) -> anyhow::Result<Option<String>> {
        let result = self.conn.query_row("SELECT run_id FROM findings ORDER BY created_at DESC LIMIT 1", [], |row| row.get::<_, String>(0));
        match result {
            Ok(run_id) => Ok(Some(run_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// All fingerprints recorded for a given run — used with
    /// `most_recent_run_id` to build the baseline set for `--incremental`.
    pub fn fingerprints_for_run(&self, run_id: &str) -> anyhow::Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT fingerprint FROM findings WHERE run_id = ?1")?;
        let rows = stmt.query_map([run_id], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }

    /// Looks up the most recently recorded finding with this id (the report's
    /// `finding.id`, e.g. `f-<fingerprint prefix>`), used by `feedback` to
    /// resolve a human-supplied id back to its fingerprint/category/severity
    /// without the caller having to know those details.
    pub fn find_finding_by_id(&self, finding_id: &str) -> anyhow::Result<Option<FindingLookup>> {
        let result = self.conn.query_row(
            "SELECT fingerprint, category, severity, rule_id, source_tool, title, message
             FROM findings WHERE finding_id = ?1 ORDER BY created_at DESC LIMIT 1",
            [finding_id],
            |row| {
                Ok(FindingLookup {
                    fingerprint: row.get(0)?,
                    category: row.get(1)?,
                    severity: row.get(2)?,
                    rule_id_or_tool: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| row.get::<_, String>(4).unwrap_or_default()),
                    title: row.get(5)?,
                    message: row.get(6)?,
                })
            },
        );
        match result {
            Ok(lookup) => Ok(Some(lookup)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Appends a feedback row — insert-only, mirroring the event log's
    /// append-only design (see `event_log.rs`): feedback is never a mutation
    /// of the original finding row, only a new fact recorded alongside it.
    pub fn record_feedback(&self, finding_id: &str, lookup: &FindingLookup, verdict: FeedbackVerdict, note: Option<&str>, created_at: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO feedback (finding_id, fingerprint, category, rule_id_or_aspect, severity, verdict, note, created_at, title, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![finding_id, lookup.fingerprint, lookup.category, lookup.rule_id_or_tool, lookup.severity, verdict.as_str(), note, created_at, lookup.title, lookup.message],
        )?;
        Ok(())
    }

    /// `--false-positive` feedback rows that carry a human-supplied
    /// `--note`, per category — skill evolution's channel 2 input (repeated
    /// false-positive feedback with the human's own stated reason).
    /// Feedback without a note is excluded: an unexplained "this was wrong"
    /// has no thread for negative-guidance drafting to work from.
    pub fn fp_feedback_with_notes(&self) -> anyhow::Result<Vec<FpFeedbackRow>> {
        let mut stmt = self.conn.prepare("SELECT category, rule_id_or_aspect, title, message, note FROM feedback WHERE verdict = 'false_positive' AND note IS NOT NULL AND note != ''")?;
        let rows = stmt.query_map([], |row| {
            Ok(FpFeedbackRow { category: row.get(0)?, rule_id_or_aspect: row.get(1)?, title: row.get(2)?, message: row.get(3)?, note: row.get(4)? })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Known-outcome findings for one recorded run — a replay corpus entry
    /// for the skill-evolution bench harness: joins that run's findings
    /// against any feedback recorded against them, so a replay can check
    /// "did the candidate skill still flag (or stop flagging) the thing a
    /// human already told us was a false/true positive."
    pub fn known_verdicts_for_run(&self, run_id: &str) -> anyhow::Result<Vec<KnownVerdict>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.title, f.message, f.fingerprint, fb.verdict
             FROM findings f JOIN feedback fb ON fb.fingerprint = f.fingerprint
             WHERE f.run_id = ?1",
        )?;
        let rows = stmt.query_map([run_id], |row| Ok(KnownVerdict { title: row.get(0)?, message: row.get(1)?, fingerprint: row.get(2)?, verdict: row.get(3)? }))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Distinct `(run_id, base_ref, head_ref)` triples for runs that have
    /// at least one known verdict — the replay corpus's candidate list,
    /// before the caller filters down to ones actually reachable via git
    /// worktree (old runs whose refs no longer exist get skipped there).
    pub fn runs_with_known_verdicts(&self) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT f.run_id FROM findings f JOIN feedback fb ON fb.fingerprint = f.fingerprint",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// All recorded agent-sourced findings (source_kind = "agent"), the
    /// mining input for the rule factory: analyzer/learned-rule findings
    /// are already deterministic and have nothing to mine into a new rule.
    /// Deliberately no distinct-fingerprint dedup here — mining needs every
    /// occurrence (including repeats across runs) to judge recurrence.
    pub fn agent_findings_for_mining(&self) -> anyhow::Result<Vec<MinedFindingRow>> {
        let mut stmt = self.conn.prepare("SELECT fingerprint, category, rule_id, source_tool, title, message, run_id FROM findings WHERE source_kind = 'agent'")?;
        let rows = stmt.query_map([], |row| {
            let rule_id: Option<String> = row.get(2)?;
            let source_tool: String = row.get(3)?;
            Ok(MinedFindingRow {
                fingerprint: row.get(0)?,
                category: row.get(1)?,
                rule_id_or_aspect: rule_id.unwrap_or(source_tool),
                title: row.get(4)?,
                message: row.get(5)?,
                run_id: row.get(6)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Records an embedding vector for a piece of feedback, keyed by
    /// fingerprint — best-effort input to the Stage 4 similarity filter.
    /// Insert-only, same append-only shape as `record_feedback`; a
    /// fingerprint can accumulate multiple embedding rows over time (e.g.
    /// re-computed with a different model) without needing an update path.
    pub fn record_embedding(&self, fingerprint: &str, verdict: FeedbackVerdict, embedding: &[f32], created_at: &str) -> anyhow::Result<()> {
        let bytes = crate::agents::embedding::embedding_to_bytes(embedding);
        self.conn.execute(
            "INSERT INTO embeddings (fingerprint, verdict, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![fingerprint, verdict.as_str(), bytes, created_at],
        )?;
        Ok(())
    }

    /// Counts distinct fingerprints, among those recorded with any verdict
    /// in `verdicts`, whose stored embedding is cosine-similar to
    /// `embedding` at or above `threshold`. Used by the Stage 4 filter to
    /// test both the `fpBlockThreshold` and `tpOverrideThreshold`
    /// conditions with the same query, just a different verdict-set/
    /// threshold pair — `verdicts` takes a slice (not one verdict) so the
    /// "tp-like" side of that check can span `TruePositive`/`AcceptedRisk`/
    /// `FixInFollowup` in one query rather than three.
    /// Only the most recent `MAX_SCANNED_EMBEDDINGS` rows across those
    /// verdicts are compared — this runs a full cosine-similarity pass per
    /// call, so without a cap it scales linearly with total historical
    /// feedback, unbounded. Recency is the right bound here (matches this
    /// store's general "recent signal matters more" posture), not an
    /// arbitrary cutoff.
    const MAX_SCANNED_EMBEDDINGS: u32 = 2_000;

    pub fn count_similar_embeddings(&self, embedding: &[f32], verdicts: &[FeedbackVerdict], threshold: f64) -> anyhow::Result<u32> {
        let placeholders = verdicts.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!("SELECT fingerprint, embedding FROM embeddings WHERE verdict IN ({placeholders}) ORDER BY id DESC LIMIT {}", Self::MAX_SCANNED_EMBEDDINGS);
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&str> = verdicts.iter().map(|v| v.as_str()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        let mut matched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in rows {
            let (fingerprint, bytes) = row?;
            let Ok(stored) = crate::agents::embedding::embedding_from_bytes(&bytes) else { continue };
            if crate::agents::embedding::cosine_similarity(embedding, &stored) >= threshold {
                matched.insert(fingerprint);
            }
        }
        Ok(matched.len() as u32)
    }

    /// Registers a rule with the shadow-mode lifecycle table if it isn't
    /// already tracked — insert-only-once (`INSERT OR IGNORE`), so calling
    /// this on every shadow firing is safe and idempotent. `initial_status`
    /// is normally `"shadow"`; a rule already tracked keeps its existing
    /// status untouched.
    pub fn ensure_rule_tracked(&self, rule_id: &str, initial_status: &str, valid_from: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO rules (id, status, firings, agent_agreed, agent_disagreed, valid_from) VALUES (?1, ?2, 0, 0, 0, ?3)",
            rusqlite::params![rule_id, initial_status, valid_from],
        )?;
        Ok(())
    }

    /// Records one shadow/promoted rule firing: appends a row to the
    /// per-occurrence `shadow_firings` log (for `rules shadow-log`'s
    /// spot-check listing) and increments the rolled-up counters on the
    /// `rules` table (for cheap promotion/demotion threshold checks, so
    /// those don't need a `GROUP BY` scan over every firing on every run).
    // Every parameter maps 1:1 onto its own `shadow_firings` column
    // (matches the INSERT below) — none of them naturally group into a
    // struct without inventing one just to satisfy this lint's count.
    #[allow(clippy::too_many_arguments)]
    pub fn record_shadow_firing(&self, rule_id: &str, fingerprint: &str, run_id: &str, location_path: &str, location_line: u32, agreement: &str, created_at: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO shadow_firings (rule_id, fingerprint, run_id, location_path, location_line, agreement, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![rule_id, fingerprint, run_id, location_path, location_line, agreement, created_at],
        )?;
        let agreed_delta = if agreement == "agreed" { 1 } else { 0 };
        let disagreed_delta = if agreement == "disagreed" { 1 } else { 0 };
        self.conn.execute(
            "UPDATE rules SET firings = firings + 1, agent_agreed = agent_agreed + ?2, agent_disagreed = agent_disagreed + ?3 WHERE id = ?1",
            rusqlite::params![rule_id, agreed_delta, disagreed_delta],
        )?;
        Ok(())
    }

    /// Current lifecycle state for a tracked rule, or `None` if it's never
    /// been through `ensure_rule_tracked` (i.e. never fired in shadow mode).
    pub fn rule_state(&self, rule_id: &str) -> anyhow::Result<Option<RuleState>> {
        let result = self.conn.query_row(
            "SELECT status, firings, agent_agreed, agent_disagreed, valid_from, invalid_at FROM rules WHERE id = ?1",
            [rule_id],
            |row| {
                Ok(RuleState {
                    status: row.get(0)?,
                    firings: row.get(1)?,
                    agent_agreed: row.get(2)?,
                    agent_disagreed: row.get(3)?,
                    valid_from: row.get(4)?,
                    invalid_at: row.get(5)?,
                })
            },
        );
        match result {
            Ok(state) => Ok(Some(state)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// How many distinct runs a rule has fired in — the plan's promotion
    /// gate needs both a firing count *and* a distinct-run count, since 20
    /// firings in one large commit isn't the same evidence as 20 firings
    /// spread across 20 reviews.
    pub fn distinct_shadow_run_count(&self, rule_id: &str) -> anyhow::Result<usize> {
        let count: i64 = self.conn.query_row("SELECT COUNT(DISTINCT run_id) FROM shadow_firings WHERE rule_id = ?1", [rule_id], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Recent firings for a rule, most recent first — the listing
    /// `rules shadow-log <ruleId>` shows for human spot-checking.
    pub fn recent_shadow_firings(&self, rule_id: &str, limit: u32) -> anyhow::Result<Vec<ShadowFiringRow>> {
        let mut stmt = self.conn.prepare("SELECT fingerprint, run_id, location_path, location_line, agreement, created_at FROM shadow_firings WHERE rule_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2")?;
        let rows = stmt.query_map(rusqlite::params![rule_id, limit], |row| {
            Ok(ShadowFiringRow { fingerprint: row.get(0)?, run_id: row.get(1)?, location_path: row.get(2)?, location_line: row.get(3)?, agreement: row.get(4)?, created_at: row.get(5)? })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Count of `--false-positive` feedback recorded against any finding
    /// attributed to this rule — the demotion signal, independent of the
    /// shadow-firing agreement ratio (a user's own explicit feedback always
    /// counts). Deliberately excludes `doesnt_apply`: that verdict says the
    /// rule is valid in general but this instance wasn't relevant, which
    /// isn't evidence the rule itself misjudged the code shape the way a
    /// real false positive is (see `FeedbackVerdict`'s own doc comment).
    pub fn count_fp_feedback_for_rule(&self, rule_id: &str) -> anyhow::Result<u32> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM feedback WHERE rule_id_or_aspect = ?1 AND verdict = ?2", rusqlite::params![rule_id, FeedbackVerdict::FalsePositive.as_str()], |row| row.get(0))?)
    }

    /// Flips a rule's lifecycle status in place (`"shadow"` <-> `"promoted"`)
    /// — the actual state transition `should_promote`/`should_demote`
    /// gate. Does not touch firing counters; those keep accumulating across
    /// a promote/demote cycle so agreement history isn't lost on demotion.
    pub fn set_rule_status(&self, rule_id: &str, status: &str) -> anyhow::Result<()> {
        self.conn.execute("UPDATE rules SET status = ?2 WHERE id = ?1", rusqlite::params![rule_id, status])?;
        Ok(())
    }

    /// Sets `invalid_at` on a tracked rule — the bi-temporal "this row is
    /// superseded" marker the schema's own doc comment anticipated for
    /// "the demotion/rollback machinery in M3" (`ensure_schema`'s doc
    /// comment). Used by `rules rollback` when a shadow rule is rejected
    /// outright (not just demoted from promoted back to shadow, which
    /// keeps the row live via `set_rule_status` alone).
    pub fn invalidate_rule(&self, rule_id: &str, invalid_at: &str) -> anyhow::Result<()> {
        self.conn.execute("UPDATE rules SET invalid_at = ?2 WHERE id = ?1", rusqlite::params![rule_id, invalid_at])?;
        Ok(())
    }

    /// Snapshots one skill aspect's full `instructions.md` content as a
    /// named version — the write side of `skill_versions`, a table that's
    /// existed in the schema since M1 but had no reader/writer until now
    /// (`skills rollback` is its first real consumer). `version` is a
    /// plain incrementing integer string per aspect (`"0"` for the
    /// pre-evolution baseline, `"1"`, `"2"`, ... for each approved
    /// proposal) — `INSERT OR REPLACE` so re-recording an existing version
    /// (shouldn't normally happen) overwrites rather than erroring.
    pub fn record_skill_version(&self, aspect: &str, version: &str, source: &str, valid_from: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO skill_versions (aspect, version, source, valid_from) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![aspect, version, source, valid_from],
        )?;
        Ok(())
    }

    /// The highest recorded version number for an aspect, or `None` if
    /// none has ever been snapshotted (the aspect has never gone through
    /// `skills review --approve`).
    pub fn latest_skill_version(&self, aspect: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT MAX(CAST(version AS INTEGER)) FROM skill_versions WHERE aspect = ?1", [aspect], |row| row.get(0))?)
    }

    /// The full `instructions.md` snapshot recorded for `(aspect, version)`,
    /// or `None` if that exact version was never recorded.
    pub fn skill_version_source(&self, aspect: &str, version: &str) -> anyhow::Result<Option<String>> {
        let result = self.conn.query_row("SELECT source FROM skill_versions WHERE aspect = ?1 AND version = ?2", rusqlite::params![aspect, version], |row| row.get(0));
        match result {
            Ok(source) => Ok(Some(source)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Every recorded `(version, valid_from)` pair for an aspect, oldest
    /// first — used to list available rollback targets when a requested
    /// version doesn't exist.
    pub fn list_skill_versions(&self, aspect: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT version, valid_from FROM skill_versions WHERE aspect = ?1 ORDER BY CAST(version AS INTEGER) ASC")?;
        let rows = stmt.query_map([aspect], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    #[cfg(test)]
    pub fn count_findings(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))?)
    }

    #[cfg(test)]
    pub fn count_findings_for_run(&self, run_id: &str) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM findings WHERE run_id = ?1", [run_id], |row| row.get(0))?)
    }

    #[cfg(test)]
    pub fn count_feedback_for_fingerprint(&self, fingerprint: &str) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM feedback WHERE fingerprint = ?1", [fingerprint], |row| row.get(0))?)
    }
}

/// The subset of a stored finding's fields feedback needs — enough to write a
/// self-describing feedback row / event without the caller re-deriving them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingLookup {
    pub fingerprint: String,
    pub category: String,
    pub severity: String,
    pub rule_id_or_tool: String,
    pub title: String,
    pub message: String,
}

/// One recorded agent finding, as the rule-factory miner needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinedFindingRow {
    pub fingerprint: String,
    pub category: String,
    pub rule_id_or_aspect: String,
    pub title: String,
    pub message: String,
    pub run_id: String,
}

/// A tracked rule's current shadow-mode lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleState {
    pub status: String,
    pub firings: u32,
    pub agent_agreed: u32,
    pub agent_disagreed: u32,
    pub valid_from: String,
    pub invalid_at: Option<String>,
}

/// One recorded shadow/promoted rule firing, as `rules shadow-log` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowFiringRow {
    pub fingerprint: String,
    pub run_id: String,
    pub location_path: String,
    pub location_line: u32,
    pub agreement: String,
    pub created_at: String,
}

/// One `--fp` feedback row with a human-supplied note — skill evolution's
/// raw mining input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpFeedbackRow {
    pub category: String,
    pub rule_id_or_aspect: String,
    pub title: String,
    pub message: String,
    pub note: String,
}

/// One finding from a past run with a known human verdict — the ground
/// truth a replay bench compares a candidate skill's output against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownVerdict {
    pub title: String,
    pub message: String,
    pub fingerprint: String,
    pub verdict: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{
        CostEntry, DiffStats, Finding, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, PlanBudgets, ReviewPlan,
        ReviewSummary, ReviewTarget, RunCosts, Severity, Side, Tier,
    };
    use std::collections::HashMap;

    fn make_finding(fingerprint: &str) -> Finding {
        Finding {
            id: format!("f-{fingerprint}"),
            fingerprints: FindingFingerprints { primary: fingerprint.to_string(), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("go-no-self-comparison".into()), aspect: None, backend: None },
            category: "correctness".into(),
            severity: Severity::Medium,
            confidence: 1.0,
            title: "t".into(),
            message: "m".into(),
            location: Location { path: "a.go".into(), range: LocationRange { start_line: 1, start_col: None, end_line: None, end_col: None }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    fn make_agent_finding(fingerprint: &str, title: &str, message: &str) -> Finding {
        let mut finding = make_finding(fingerprint);
        finding.source = FindingSource { kind: FindingSourceKind::Agent, tool: "claude".into(), rule_id: None, aspect: Some("correctness".into()), backend: None };
        finding.title = title.to_string();
        finding.message = message.to_string();
        finding
    }

    fn make_report(run_id: &str, findings: Vec<Finding>) -> ReviewReport {
        make_report_at(run_id, findings, "2026-07-12T00:00:00Z")
    }

    fn make_report_at(run_id: &str, findings: Vec<Finding>, created_at: &str) -> ReviewReport {
        ReviewReport {
            schema_version: "1".into(),
            run_id: run_id.into(),
            created_at: created_at.into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: HashMap::new() } },
            plan: ReviewPlan { tier: Tier::Quick, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings,
            suppressed: vec![],
            costs: RunCosts { total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
            spec_verdicts: vec![],
        }
    }

    #[test]
    fn opening_a_store_creates_the_schema_idempotently() {
        let store = HistoryStore::open_in_memory().unwrap();
        // Re-running schema creation must not error (CREATE TABLE IF NOT EXISTS).
        store.ensure_schema().unwrap();
        assert_eq!(store.count_findings().unwrap(), 0);
    }

    #[test]
    fn record_run_inserts_one_row_per_finding() {
        let store = HistoryStore::open_in_memory().unwrap();
        let report = make_report("run-1", vec![make_finding("a"), make_finding("b")]);
        store.record_run(&report).unwrap();
        assert_eq!(store.count_findings().unwrap(), 2);
        assert_eq!(store.count_findings_for_run("run-1").unwrap(), 2);
    }

    #[test]
    fn multiple_runs_accumulate_rather_than_overwrite() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        store.record_run(&make_report("run-2", vec![make_finding("b")])).unwrap();
        assert_eq!(store.count_findings().unwrap(), 2);
        assert_eq!(store.count_findings_for_run("run-1").unwrap(), 1);
        assert_eq!(store.count_findings_for_run("run-2").unwrap(), 1);
    }

    #[test]
    fn open_creates_the_db_file_on_disk_at_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(dir.path()).unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        assert!(dir.path().join("index.db").exists());
    }

    #[test]
    fn find_finding_by_id_resolves_a_recorded_finding() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().expect("finding should be found");
        assert_eq!(lookup.fingerprint, "a");
        assert_eq!(lookup.category, "correctness");
        assert_eq!(lookup.severity, "medium");
        assert_eq!(lookup.rule_id_or_tool, "go-no-self-comparison");
    }

    #[test]
    fn find_finding_by_id_returns_none_for_an_unknown_id() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        assert!(store.find_finding_by_id("f-does-not-exist").unwrap().is_none());
    }

    #[test]
    fn record_feedback_inserts_a_row_without_touching_the_findings_table() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::FalsePositive, Some("intentional pattern here"), "2026-07-12T00:00:00Z").unwrap();
        assert_eq!(store.count_feedback_for_fingerprint("a").unwrap(), 1);
        assert_eq!(store.count_findings().unwrap(), 1);
    }

    #[test]
    fn record_feedback_accumulates_multiple_verdicts_for_the_same_finding() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::FalsePositive, None, "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::FalsePositive, None, "2026-07-13T00:00:00Z").unwrap();
        assert_eq!(store.count_feedback_for_fingerprint("a").unwrap(), 2);
    }

    #[test]
    fn most_recent_run_id_is_none_when_history_is_empty() {
        let store = HistoryStore::open_in_memory().unwrap();
        assert_eq!(store.most_recent_run_id().unwrap(), None);
    }

    #[test]
    fn most_recent_run_id_picks_the_latest_by_created_at() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report_at("run-1", vec![make_finding("a")], "2026-07-10T00:00:00Z")).unwrap();
        store.record_run(&make_report_at("run-2", vec![make_finding("b")], "2026-07-12T00:00:00Z")).unwrap();
        assert_eq!(store.most_recent_run_id().unwrap(), Some("run-2".to_string()));
    }

    #[test]
    fn fingerprints_for_run_returns_only_that_runs_fingerprints() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report_at("run-1", vec![make_finding("a"), make_finding("b")], "2026-07-10T00:00:00Z")).unwrap();
        store.record_run(&make_report_at("run-2", vec![make_finding("c")], "2026-07-12T00:00:00Z")).unwrap();
        let fps = store.fingerprints_for_run("run-1").unwrap();
        assert_eq!(fps, std::collections::HashSet::from(["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn agent_findings_for_mining_returns_only_agent_sourced_findings() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a"), make_agent_finding("b", "Missing null check", "Parameter x is not null-checked before use")])).unwrap();
        let mined = store.agent_findings_for_mining().unwrap();
        assert_eq!(mined.len(), 1);
        assert_eq!(mined[0].fingerprint, "b");
        assert_eq!(mined[0].title, "Missing null check");
    }

    #[test]
    fn count_similar_embeddings_counts_only_vectors_above_the_threshold() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_embedding("a", FeedbackVerdict::FalsePositive, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        store.record_embedding("b", FeedbackVerdict::FalsePositive, &[0.0, 1.0], "2026-07-12T00:00:00Z").unwrap();
        assert_eq!(store.count_similar_embeddings(&[1.0, 0.0], &[FeedbackVerdict::FalsePositive], 0.9).unwrap(), 1);
    }

    #[test]
    fn count_similar_embeddings_ignores_a_different_verdict() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_embedding("a", FeedbackVerdict::TruePositive, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        assert_eq!(store.count_similar_embeddings(&[1.0, 0.0], &[FeedbackVerdict::FalsePositive], 0.9).unwrap(), 0);
    }

    #[test]
    fn count_similar_embeddings_counts_each_matching_fingerprint_once() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_embedding("a", FeedbackVerdict::FalsePositive, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        store.record_embedding("a", FeedbackVerdict::FalsePositive, &[1.0, 0.01], "2026-07-13T00:00:00Z").unwrap();
        assert_eq!(store.count_similar_embeddings(&[1.0, 0.0], &[FeedbackVerdict::FalsePositive], 0.9).unwrap(), 1);
    }

    #[test]
    fn count_similar_embeddings_aggregates_across_multiple_verdicts_in_one_call() {
        // The Stage 4 filter's tp-side check spans TruePositive/
        // AcceptedRisk/FixInFollowup in one query — all three confirm the
        // finding was correct, so they must all count toward the same
        // "tp-like" bucket.
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_embedding("a", FeedbackVerdict::TruePositive, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        store.record_embedding("b", FeedbackVerdict::AcceptedRisk, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        store.record_embedding("c", FeedbackVerdict::FixInFollowup, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        store.record_embedding("d", FeedbackVerdict::DoesntApply, &[1.0, 0.0], "2026-07-12T00:00:00Z").unwrap();
        let tp_like = &[FeedbackVerdict::TruePositive, FeedbackVerdict::AcceptedRisk, FeedbackVerdict::FixInFollowup];
        assert_eq!(store.count_similar_embeddings(&[1.0, 0.0], tp_like, 0.9).unwrap(), 3, "DoesntApply must not count toward the tp-like bucket");
    }

    #[test]
    fn ensure_rule_tracked_is_idempotent_and_does_not_reset_status() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        store.set_rule_status("go-example", "promoted").unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.status, "promoted");
    }

    #[test]
    fn invalidate_rule_sets_invalid_at_without_touching_status() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        assert!(store.rule_state("go-example").unwrap().unwrap().invalid_at.is_none());
        store.invalidate_rule("go-example", "2026-07-10T00:00:00Z").unwrap();
        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.invalid_at.as_deref(), Some("2026-07-10T00:00:00Z"));
        assert_eq!(state.status, "shadow", "invalidating must not change the status column");
    }

    #[test]
    fn latest_skill_version_is_none_for_an_untracked_aspect() {
        let store = HistoryStore::open_in_memory().unwrap();
        assert_eq!(store.latest_skill_version("correctness").unwrap(), None);
    }

    #[test]
    fn record_and_read_back_a_skill_version() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_skill_version("correctness", "0", "baseline instructions", "2026-07-01T00:00:00Z").unwrap();
        store.record_skill_version("correctness", "1", "baseline instructions\n\nnew guidance line", "2026-07-02T00:00:00Z").unwrap();

        assert_eq!(store.latest_skill_version("correctness").unwrap(), Some(1));
        assert_eq!(store.skill_version_source("correctness", "0").unwrap().as_deref(), Some("baseline instructions"));
        assert_eq!(store.skill_version_source("correctness", "1").unwrap().as_deref(), Some("baseline instructions\n\nnew guidance line"));
        assert_eq!(store.skill_version_source("correctness", "2").unwrap(), None);
    }

    #[test]
    fn list_skill_versions_orders_numerically_not_lexically() {
        let store = HistoryStore::open_in_memory().unwrap();
        for v in ["0", "1", "2", "10"] {
            store.record_skill_version("correctness", v, "x", "2026-07-01T00:00:00Z").unwrap();
        }
        let versions: Vec<String> = store.list_skill_versions("correctness").unwrap().into_iter().map(|(v, _)| v).collect();
        assert_eq!(versions, vec!["0", "1", "2", "10"], "numeric ordering must put 10 after 2, not between 1 and 2 lexically");
    }

    #[test]
    fn different_aspects_track_versions_independently() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_skill_version("correctness", "0", "a", "2026-07-01T00:00:00Z").unwrap();
        store.record_skill_version("security", "0", "b", "2026-07-01T00:00:00Z").unwrap();
        store.record_skill_version("security", "1", "b2", "2026-07-02T00:00:00Z").unwrap();
        assert_eq!(store.latest_skill_version("correctness").unwrap(), Some(0));
        assert_eq!(store.latest_skill_version("security").unwrap(), Some(1));
    }

    #[test]
    fn record_shadow_firing_updates_counters_and_appends_a_log_row() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        store.record_shadow_firing("go-example", "fp1", "run-1", "a.go", 10, "agreed", "2026-07-02T00:00:00Z").unwrap();
        store.record_shadow_firing("go-example", "fp2", "run-2", "b.go", 20, "disagreed", "2026-07-03T00:00:00Z").unwrap();
        store.record_shadow_firing("go-example", "fp3", "run-2", "c.go", 30, "no_signal", "2026-07-03T00:00:00Z").unwrap();

        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.firings, 3);
        assert_eq!(state.agent_agreed, 1);
        assert_eq!(state.agent_disagreed, 1);
        assert_eq!(store.distinct_shadow_run_count("go-example").unwrap(), 2);

        let recent = store.recent_shadow_firings("go-example", 10).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].fingerprint, "fp3", "most recent first");
    }

    #[test]
    fn rule_state_is_none_for_an_untracked_rule() {
        let store = HistoryStore::open_in_memory().unwrap();
        assert!(store.rule_state("never-fired").unwrap().is_none());
    }

    #[test]
    fn count_fp_feedback_for_rule_counts_only_fp_verdicts_for_that_rule() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::FalsePositive, None, "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::TruePositive, None, "2026-07-13T00:00:00Z").unwrap();
        assert_eq!(store.count_fp_feedback_for_rule("go-no-self-comparison").unwrap(), 1);
        assert_eq!(store.count_fp_feedback_for_rule("some-other-rule").unwrap(), 0);
    }

    #[test]
    fn count_fp_feedback_for_rule_excludes_doesnt_apply() {
        // Regression test for the whole point of this waiver taxonomy:
        // `DoesntApply` says the rule is valid in general, just not
        // relevant here — that's not evidence the rule itself misjudged
        // the code shape, so it must never count toward the demotion gate
        // the way a real `FalsePositive` does.
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::DoesntApply, None, "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-a", &lookup, FeedbackVerdict::DoesntApply, None, "2026-07-13T00:00:00Z").unwrap();
        assert_eq!(store.count_fp_feedback_for_rule("go-no-self-comparison").unwrap(), 0);
    }

    #[test]
    fn fp_feedback_with_notes_excludes_unnoted_and_non_fp_feedback() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a"), make_finding("b"), make_finding("c")])).unwrap();
        let a = store.find_finding_by_id("f-a").unwrap().unwrap();
        let b = store.find_finding_by_id("f-b").unwrap().unwrap();
        let c = store.find_finding_by_id("f-c").unwrap().unwrap();
        store.record_feedback("f-a", &a, FeedbackVerdict::FalsePositive, Some("intentional pattern here"), "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-b", &b, FeedbackVerdict::FalsePositive, None, "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-c", &c, FeedbackVerdict::TruePositive, Some("real bug"), "2026-07-12T00:00:00Z").unwrap();

        let rows = store.fp_feedback_with_notes().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].note, "intentional pattern here");
    }

    #[test]
    fn known_verdicts_for_run_joins_findings_with_their_feedback() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a"), make_finding("b")])).unwrap();
        let a = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &a, FeedbackVerdict::FalsePositive, None, "2026-07-12T00:00:00Z").unwrap();

        let known = store.known_verdicts_for_run("run-1").unwrap();
        assert_eq!(known.len(), 1, "only 'a' has feedback recorded");
        assert_eq!(known[0].fingerprint, "a");
        assert_eq!(known[0].verdict, "false_positive");
    }

    #[test]
    fn runs_with_known_verdicts_returns_only_runs_with_feedback() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        store.record_run(&make_report("run-2", vec![make_finding("b")])).unwrap();
        let a = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &a, FeedbackVerdict::TruePositive, None, "2026-07-12T00:00:00Z").unwrap();

        let runs = store.runs_with_known_verdicts().unwrap();
        assert_eq!(runs, vec!["run-1".to_string()]);
    }
}
