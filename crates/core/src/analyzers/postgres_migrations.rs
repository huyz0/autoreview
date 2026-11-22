mod sql;

use std::collections::BTreeSet;
use std::path::Path;

use autoreview_schema::{AgentFinding, Location, LocationRange, Side};
use globset::{Glob, GlobSetBuilder};

use super::artifact_policy::{read_snapshot_file, Rule};

pub(super) const RULE: Rule = Rule {
    id: "postgres-concurrent-index-in-transaction",
    category: "correctness",
    subcategory: "deployment.migrations",
    tool: "autoreview-postgres-migrations",
    title: "Concurrent PostgreSQL index creation inside a transaction",
    reference: "https://www.postgresql.org/docs/16/sql-createindex.html#SQL-CREATEINDEX-CONCURRENTLY",
    enabled_by: "artifactChecks.transactionalPostgresMigrations",
};

pub fn run_postgres_migration_policy(
    root: &Path, changed: &[String], patterns: &[String],
) -> anyhow::Result<Vec<AgentFinding>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    let matches = builder.build()?;
    let root = root.canonicalize()?;
    let migrations: BTreeSet<_> = changed.iter().map(String::as_str)
        .map(|path| if path.ends_with(".sql.conf") { path.strip_suffix(".conf").unwrap_or(path) } else { path })
        .filter(|path| path.ends_with(".sql") && matches.is_match(path)).collect();
    let mut findings = Vec::new();
    for path in migrations {
        let sidecar = format!("{path}.conf");
        let setting = read_snapshot_file(&root, Path::new(&sidecar)).as_deref().and_then(transaction_setting);
        if setting.is_some_and(|(transactional, _)| !transactional) {
            continue;
        }
        let Some(source) = read_snapshot_file(&root, Path::new(path)) else { continue };
        for range in sql::concurrent_indexes(&source) {
            let start = source[..range.start].bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
            let end = start + source[range.clone()].bytes().filter(|byte| *byte == b'\n').count() as u32;
            let mut finding = RULE.finding(path, LocationRange { start_line: start, end_line: Some(end), ..Default::default() },
                &source[range], "Run CREATE INDEX CONCURRENTLY outside the migration transaction.".into());
            if let Some((_, config_line)) = setting {
                finding.related_locations = Some(vec![Location {
                    path: sidecar.clone(),
                    range: LocationRange { start_line: config_line, ..Default::default() },
                    snippet: "executeInTransaction=true".into(),
                    side: Side::New,
                }]);
            }
            findings.push(finding);
        }
    }
    Ok(findings)
}

fn transaction_setting(source: &str) -> Option<(bool, u32)> {
    source.lines().enumerate().filter_map(|(line, content)| {
        let (key, value) = content.trim().split_once(['=', ':'])?;
        if key.trim() != "executeInTransaction" {
            return None;
        }
        match value.trim() {
            "true" => Some((true, line as u32 + 1)),
            "false" => Some((false, line as u32 + 1)),
            _ => None,
        }
    }).last()
}
