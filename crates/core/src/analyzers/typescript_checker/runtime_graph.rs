//! Optional conservative graph of the compiler-emitted CommonJS imports.
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use autoreview_dataflow::call_order::KeyedUnit;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use super::{TypeScriptGraphEvidence, TypeScriptGraphSource};

const SCRIPT: &str = include_str!("../typescript_runtime_graph.cjs");

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> { typescript_module: &'a str, config: &'a str, root: &'a Path, files: Vec<TypeScriptGraphSource>, entries: Vec<usize> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response { status: String, reachable: Option<Vec<usize>>, project_sources: Option<Vec<TypeScriptGraphSource>>, options_sha256: Option<String> }

pub(in crate::analyzers) struct Graph { pub reachable: BTreeSet<usize>, pub evidence: TypeScriptGraphEvidence }

pub(in crate::analyzers) fn reachable(root: &Path, units: &[KeyedUnit<'_, '_, '_>], node: &str, compiler: &str, config: &str) -> Option<Graph> {
    if std::env::var("AUTOREVIEW_TYPESCRIPT_TRUST_EMITTED_GRAPH").ok().as_deref() != Some("1") || units.len() > 512 { return None; }
    let files = manifest(root, units)?;
    let mut entries = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        if unit.entry { entries.push(index); }
    }
    let request = Request { typescript_module: compiler, config, root, files, entries };
    let output = super::run_bounded(Command::new(node).args(["-e", SCRIPT]), serde_json::to_vec(&request).ok()?, Duration::from_secs(30))?;
    let response: Response = serde_json::from_slice(&output).ok()?;
    if response.status != "ok" { return None; }
    let indexes = response.reachable?;
    let unique: BTreeSet<_> = indexes.iter().copied().collect();
    if unique.len() != indexes.len() || unique.iter().any(|index| *index >= units.len()) || request.entries.iter().any(|index| !unique.contains(index)) { return None; }
    let sources = response.project_sources?;
    let options_sha256 = response.options_sha256?;
    if sources.is_empty() || sources.len() > 512 || options_sha256.len() != 64 || !options_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) { return None; }
    let mut paths = BTreeSet::new();
    for source in &sources {
        let path = Path::new(&source.path).canonicalize().ok()?;
        if source.sha256.len() != 64 || !source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) || !paths.insert(path) { return None; }
    }
    for file in &request.files {
        let path = Path::new(&file.path).canonicalize().ok()?;
        if !sources.iter().any(|source| Path::new(&source.path).canonicalize().is_ok_and(|candidate| candidate == path) && source.sha256 == file.sha256) { return None; }
    }
    Some(Graph { reachable: unique, evidence: TypeScriptGraphEvidence { sources, options_sha256 } })
}

pub(in crate::analyzers) fn manifest(root: &Path, units: &[KeyedUnit<'_, '_, '_>]) -> Option<Vec<TypeScriptGraphSource>> {
    units.iter().map(|unit| Some(TypeScriptGraphSource { path: root.join(unit.path).to_str()?.to_owned(), sha256: format!("{:x}", Sha256::digest(unit.source)) })).collect()
}
