//! Optional diagnostic metadata only. A report is not an execution contract.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use autoreview_schema::AgentFinding;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tree_sitter::Point;

use super::ParsedSource;

const MAX_REPORT: u64 = 1024 * 1024;
const MAX_SOURCE: usize = 4 * 1024 * 1024;
const MAX_RECORDS_PER_FILE: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot { snapshot_version: u32, sources: Vec<Source>, evidence: Report }
#[derive(Deserialize)]
struct Source { path: PathBuf, sha256: String }
#[derive(Deserialize)]
struct Report { version: u32, compiler: String, records: Vec<Record> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    path: PathBuf,
    sha256: String,
    lambda_start_utf16: usize,
    lambda_end_utf16: usize,
    callee: String,
    parameter: usize,
    sam_type: String,
    modality: String,
}

pub(super) struct Evidence { by_file: HashMap<PathBuf, Vec<Record>> }

pub(super) fn from_environment(root: &Path) -> Result<Option<Evidence>, ()> {
    let Some(path) = std::env::var_os("AUTOREVIEW_KOTLIN_CALLBACK_EVIDENCE") else { return Ok(None); };
    let path = Path::new(&path);
    if !path.is_absolute() { return Err(()); }
    let checkout = std::env::var_os("AUTOREVIEW_KOTLIN_CALLBACK_ROOT").ok_or(())?;
    let checkout = Path::new(&checkout);
    if !checkout.is_absolute() { return Err(()); }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|_| ())?.take(MAX_REPORT + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > MAX_REPORT { return Err(()); }
    parse(root, checkout, &bytes).map(Some)
}

fn parse(root: &Path, checkout: &Path, bytes: &[u8]) -> Result<Evidence, ()> {
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| ())?;
    if snapshot.snapshot_version != 1 || snapshot.evidence.version != 2 || snapshot.evidence.compiler != "2.2.20" || snapshot.sources.len() > 512 { return Err(()); }
    let root = root.canonicalize().map_err(|_| ())?;
    let checkout = checkout.canonicalize().map_err(|_| ())?;
    let mut sources = HashMap::new();
    for source in snapshot.sources {
        let path = source.path.canonicalize().map_err(|_| ())?;
        let relative = path.strip_prefix(&checkout).map_err(|_| ())?;
        let target = root.join(relative).canonicalize().map_err(|_| ())?;
        if !target.starts_with(&root) || source.sha256.len() != 64 || sources.insert(path, (target, source.sha256)).is_some() { return Err(()); }
    }
    let mut by_file: HashMap<PathBuf, Vec<Record>> = HashMap::new();
    for record in snapshot.evidence.records {
        let path = record.path.canonicalize().map_err(|_| ())?;
        let (target, hash) = sources.get(&path).ok_or(())?;
        if hash != &record.sha256 || record.callee.len() > 1024 || record.sam_type.len() > 1024 || record.modality.len() > 32 { return Err(()); }
        let records = by_file.entry(target.clone()).or_default();
        if records.len() >= MAX_RECORDS_PER_FILE { return Err(()); }
        records.push(record);
    }
    Ok(Evidence { by_file })
}

impl Evidence {
    pub(super) fn for_finding(&self, root: &Path, path: &str, parsed: &ParsedSource, finding: &AgentFinding) -> Option<serde_json::Value> {
        if parsed.source.len() > MAX_SOURCE { return None; }
        let file = root.join(path).canonicalize().ok()?;
        let records = self.by_file.get(&file)?;
        let hash = format!("{:x}", Sha256::digest(parsed.source.as_bytes()));
        let position = Point::new(finding.location.range.start_line.checked_sub(1)? as usize,
            finding.location.range.start_col?.checked_sub(1)? as usize);
        let node = parsed.tree.root_node().descendant_for_point_range(position, position)?;
        let spans = lambda_spans(node, &parsed.source);
        let matched: Vec<_> = records.iter().filter(|record| record.sha256 == hash && spans.contains(&(record.lambda_start_utf16, record.lambda_end_utf16))).collect();
        let [record] = matched.as_slice() else { return None; };
        Some(serde_json::json!({ "status": "unverified", "callee": record.callee,
            "parameter": record.parameter, "samType": record.sam_type, "modality": record.modality }))
    }
}

fn lambda_spans(node: tree_sitter::Node<'_>, source: &str) -> Vec<(usize, usize)> {
    std::iter::successors(Some(node), |node| node.parent())
        .take(64)
        .filter(|node| node.kind() == "lambda_literal")
        .filter_map(|node| Some((source.get(..node.start_byte())?.encode_utf16().count(), source.get(..node.end_byte())?.encode_utf16().count())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

    fn finding() -> AgentFinding {
        AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("kotlin-runblocking-in-suspend-function".into()), aspect: None, backend: None },
            category: "correctness".into(), severity: Severity::Medium, confidence: 1.0, title: "test".into(), message: "test".into(),
            location: Location { path: "source.kt".into(), range: LocationRange { start_line: 3, start_col: Some(13), end_line: None, end_col: None }, snippet: String::new(), side: Side::New },
            related_locations: None, suggestion: None, tags: None, meta: None, suggested_patch: None,
        }
    }

    fn report(path: &Path, source: &str) -> Vec<u8> {
        let lambda = "{ runBlocking { work() } }";
        let start = source.find(lambda).unwrap();
        let lambda_start = source[..start].encode_utf16().count();
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        serde_json::to_vec(&serde_json::json!({
            "snapshotVersion": 1,
            "sources": [{"path": path, "sha256": hash}],
            "evidence": {"version": 2, "compiler": "2.2.20", "records": [{
                "path": path, "sha256": hash, "lambdaStartUtf16": lambda_start,
                "lambdaEndUtf16": lambda_start + lambda.encode_utf16().count(),
                "callee": "example.register", "parameter": 0, "samType": "example.Callback", "modality": "OPEN"
            }]}
        })).unwrap()
    }

    #[test]
    fn matching_evidence_is_diagnostic_only_and_stale_sources_do_not_match() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.kt");
        let source = "// 😀\nsuspend fun run() {\n register { runBlocking { work() } }\n}\n";
        std::fs::write(&path, source).unwrap();
        let evidence = parse(root.path(), root.path(), &report(&path, source)).unwrap();
        let mut findings = vec![finding()];
        let mut coverage = Vec::new();
        super::super::annotate_with(root.path(), &mut findings, Some(&evidence), &mut coverage);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].meta.as_ref().unwrap()["experimentalCallbackEvidence"]["status"], "unverified");
        assert_eq!(findings[0].meta.as_ref().unwrap()["experimentalCallbackEvidence"]["callee"], "example.register");
        assert_eq!(findings[0].confidence, 0.5);
        assert!(coverage.iter().any(|issue| issue.reason == "callback_execution_unresolved"));
        std::fs::write(&path, source.replace("work()", "other()" )).unwrap();
        let mut findings = vec![finding()];
        super::super::annotate_with(root.path(), &mut findings, Some(&evidence), &mut Vec::new());
        assert!(findings[0].meta.is_none());
    }

    #[test]
    fn rejects_external_sources() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("source.kt");
        let source = "// 😀\nsuspend fun run() {\n register { runBlocking { work() } }\n}\n";
        std::fs::write(&path, source).unwrap();
        assert!(parse(root.path(), root.path(), &report(&path, source)).is_err());
    }

    #[test]
    fn maps_checkout_paths_to_analyzed_snapshot() {
        let checkout = tempfile::tempdir().unwrap();
        let analyzed = tempfile::tempdir().unwrap();
        let source = "// 😀\nsuspend fun run() {\n register { runBlocking { work() } }\n}\n";
        let original = checkout.path().join("source.kt");
        std::fs::write(&original, source).unwrap();
        std::fs::write(analyzed.path().join("source.kt"), source).unwrap();
        let evidence = parse(analyzed.path(), checkout.path(), &report(&original, source)).unwrap();
        let mut findings = vec![finding()];
        let mut coverage = Vec::new();
        super::super::annotate_with(analyzed.path(), &mut findings, Some(&evidence), &mut coverage);
        assert_eq!(findings[0].meta.as_ref().unwrap()["experimentalCallbackEvidence"]["status"], "unverified");
        assert!(coverage.iter().any(|issue| issue.reason == "callback_execution_unresolved"));
    }
}
