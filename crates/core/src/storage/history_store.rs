use std::path::Path;

use rusqlite::Connection;

use autoreview_schema::ReviewReport;

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
                invalid_at TEXT
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
                created_at TEXT NOT NULL
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
            ",
        )?;
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
                "INSERT INTO findings (finding_id, fingerprint, category, severity, source_kind, source_tool, rule_id, run_id, created_at, valid_from)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
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
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Looks up the most recently recorded finding with this id (the report's
    /// `finding.id`, e.g. `f-<fingerprint prefix>`), used by `feedback` to
    /// resolve a human-supplied id back to its fingerprint/category/severity
    /// without the caller having to know those details.
    pub fn find_finding_by_id(&self, finding_id: &str) -> anyhow::Result<Option<FindingLookup>> {
        let result = self.conn.query_row(
            "SELECT fingerprint, category, severity, rule_id, source_tool
             FROM findings WHERE finding_id = ?1 ORDER BY created_at DESC LIMIT 1",
            [finding_id],
            |row| {
                Ok(FindingLookup {
                    fingerprint: row.get(0)?,
                    category: row.get(1)?,
                    severity: row.get(2)?,
                    rule_id_or_tool: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| row.get::<_, String>(4).unwrap_or_default()),
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
    pub fn record_feedback(&self, finding_id: &str, lookup: &FindingLookup, verdict: &str, note: Option<&str>, created_at: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO feedback (finding_id, fingerprint, category, rule_id_or_aspect, severity, verdict, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![finding_id, lookup.fingerprint, lookup.category, lookup.rule_id_or_tool, lookup.severity, verdict, note, created_at],
        )?;
        Ok(())
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

    fn make_report(run_id: &str, findings: Vec<Finding>) -> ReviewReport {
        ReviewReport {
            schema_version: "1".into(),
            run_id: run_id.into(),
            created_at: "2026-07-12T00:00:00Z".into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: HashMap::new() } },
            plan: ReviewPlan { tier: Tier::Quick, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings,
            suppressed: vec![],
            costs: RunCosts { total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
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
        store.record_feedback("f-a", &lookup, "fp", Some("intentional pattern here"), "2026-07-12T00:00:00Z").unwrap();
        assert_eq!(store.count_feedback_for_fingerprint("a").unwrap(), 1);
        assert_eq!(store.count_findings().unwrap(), 1);
    }

    #[test]
    fn record_feedback_accumulates_multiple_verdicts_for_the_same_finding() {
        let store = HistoryStore::open_in_memory().unwrap();
        store.record_run(&make_report("run-1", vec![make_finding("a")])).unwrap();
        let lookup = store.find_finding_by_id("f-a").unwrap().unwrap();
        store.record_feedback("f-a", &lookup, "fp", None, "2026-07-12T00:00:00Z").unwrap();
        store.record_feedback("f-a", &lookup, "fp", None, "2026-07-13T00:00:00Z").unwrap();
        assert_eq!(store.count_feedback_for_fingerprint("a").unwrap(), 2);
    }
}
