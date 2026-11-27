use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use sha2::{Digest, Sha256};
use autoreview_dataflow::call_order::{FactorySourceDiscovery, KeyedLimitation, KeyedUnit};
use super::{check, TypeScriptCall, TypeScriptRequest};
mod batches;
type SpanMap = BTreeMap<usize, BTreeSet<(usize, usize)>>;
type SpanOrigin = (usize, usize, usize);

/// Keep only checker-confirmed candidates. Unverified positions retain their
/// coverage limitations. This does not select or trust a compiler for users.
pub struct CheckerConfig<'a> {
    pub node: &'a str,
    pub compiler: &'a str,
    pub config: &'a str,
    pub root: &'a Path,
    pub timeout: Duration,
}

struct CallBatch {
    positions: Vec<(usize, usize)>,
    calls: Vec<TypeScriptCall>,
}

pub fn verify_candidates(
    options: &CheckerConfig<'_>, units: &[KeyedUnit<'_, '_, '_>], discovery: FactorySourceDiscovery,
) -> FactorySourceDiscovery {
    let mut verified = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut classified = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut limitations = discovery.limitations;
    let Some(batch) = collect_calls(options.root, units, &discovery.candidates) else {
        limitations.push(checker_limitation());
        return FactorySourceDiscovery { candidates: verified, spans: BTreeMap::new(), limitations };
    };
    if !batch.calls.is_empty() {
        if let Err(reason) = batches::verify(options, &batch, &mut verified, &mut classified, check) {
            limitations.push(KeyedLimitation { reason, path: None, line: None });
        }
    }
    let (spans, unclassified) = verify_spans(options, units, &discovery.spans, &mut limitations);
    retain_unresolved(&mut limitations, units, &discovery.candidates, &classified, &unclassified);
    FactorySourceDiscovery { candidates: verified, spans, limitations }
}

fn verify_spans(
    options: &CheckerConfig<'_>, units: &[KeyedUnit<'_, '_, '_>], spans: &SpanMap,
    limitations: &mut Vec<KeyedLimitation>,
) -> (SpanMap, SpanMap) {
    let mut verified = BTreeMap::new();
    let mut pending = spans.clone();
    let Some((batch, originals)) = collect_span_calls(options.root, units, spans) else {
        limitations.push(checker_limitation());
        return (verified, pending);
    };
    let mut checked = BTreeMap::new();
    let mut classified = BTreeMap::new();
    if !batch.calls.is_empty() {
        if let Err(reason) = batches::verify(options, &batch, &mut checked, &mut classified, check) {
            limitations.push(KeyedLimitation { reason, path: None, line: None });
        }
    }
    for (ordinal, (unit, start, end)) in originals.into_iter().enumerate() {
        if checked.get(&ordinal).is_some_and(|sites| sites.contains(&start)) { verified.entry(unit).or_insert_with(BTreeSet::new).insert((start, end)); }
        if classified.get(&ordinal).is_some_and(|sites| sites.contains(&start)) {
            if let Some(sites) = pending.get_mut(&unit) { sites.remove(&(start, end)); }
        }
    }
    (verified, pending)
}

fn collect_span_calls(
    root: &Path, units: &[KeyedUnit<'_, '_, '_>], spans: &SpanMap,
) -> Option<(CallBatch, Vec<SpanOrigin>)> {
    let root_abs = if root.is_absolute() { root.to_path_buf() } else { std::env::current_dir().ok()?.join(root) };
    let canonical_root = root_abs.canonicalize().ok()?;
    let mut batch = CallBatch { positions: Vec::new(), calls: Vec::new() };
    let mut originals = Vec::new();
    for (&index, sites) in spans {
        let Some(unit) = units.get(index) else { continue };
        let file = root_abs.join(unit.path);
        if !file.canonicalize().is_ok_and(|resolved| resolved.starts_with(&canonical_root)) { continue; }
        let Some(path) = file.to_str() else { continue };
        let digest = format!("{:x}", Sha256::digest(unit.source));
        for &(start, end) in sites {
            if start >= end || end > unit.source.len() { continue; }
            batch.positions.push((originals.len(), start));
            originals.push((index, start, end));
            batch.calls.push(TypeScriptCall { path: path.to_owned(), start, end: Some(end), source_sha256: digest.clone() });
        }
    }
    Some((batch, originals))
}

fn collect_calls(
    root: &Path, units: &[KeyedUnit<'_, '_, '_>], candidates: &BTreeMap<usize, BTreeSet<usize>>,
) -> Option<CallBatch> {
    let root_abs = if root.is_absolute() { root.to_path_buf() } else { std::env::current_dir().ok()?.join(root) };
    let root = root_abs.canonicalize().ok()?;
    let mut positions = Vec::new();
    let mut calls = Vec::new();
    for (unit_index, offsets) in candidates {
        let Some(unit) = units.get(*unit_index) else { continue };
        let file = root_abs.join(unit.path);
        if !file.canonicalize().is_ok_and(|resolved| resolved.starts_with(&root)) { continue; }
        let Some(path) = file.to_str() else { continue };
        let digest = format!("{:x}", Sha256::digest(unit.source));
        for &start in offsets {
            if start >= unit.source.len() { continue; }
            positions.push((*unit_index, start));
            calls.push(TypeScriptCall { path: path.to_owned(), start, end: None, source_sha256: digest.clone() });
        }
    }
    Some(CallBatch { positions, calls })
}

fn retain_unresolved(
    limitations: &mut Vec<KeyedLimitation>, units: &[KeyedUnit<'_, '_, '_>],
    candidates: &BTreeMap<usize, BTreeSet<usize>>, verified: &BTreeMap<usize, BTreeSet<usize>>,
    unclassified: &SpanMap,
) {
    let mut unresolved = BTreeSet::new();
    let missing = candidates.iter().flat_map(|(&unit, offsets)| offsets.iter().copied().filter_map(move |start| {
        (!verified.get(&unit).is_some_and(|sites| sites.contains(&start))).then_some((unit, start))
    }));
    let missing_spans = unclassified.iter().flat_map(|(&unit, sites)| sites.iter().map(move |&(start, _)| (unit, start)));
    for (index, start) in missing.chain(missing_spans) {
        let Some(unit) = units.get(index) else { continue };
        let line = unit.source.get(..start).map(|bytes| bytes.iter().filter(|byte| **byte == b'\n').count() as u32 + 1);
        unresolved.insert((unit.path.to_owned(), line));
    }
    if candidates.keys().chain(unclassified.keys()).all(|index| *index < units.len()) {
        limitations.retain(|issue| issue.reason != "promise_type_unresolved" || issue.path.as_ref().is_none_or(|path| unresolved.contains(&(path.clone(), issue.line))));
    } else {
        limitations.push(checker_limitation());
    }
}

fn checker_limitation() -> KeyedLimitation {
    KeyedLimitation { reason: "typescript_checker_incomplete", path: None, line: None }
}

