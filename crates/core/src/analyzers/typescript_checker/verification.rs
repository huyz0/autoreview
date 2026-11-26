use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use sha2::{Digest, Sha256};
use autoreview_dataflow::call_order::{FactorySourceDiscovery, KeyedLimitation, KeyedUnit};
use super::{check, TypeScriptCall, TypeScriptRequest};
mod batches;

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
        return FactorySourceDiscovery { candidates: verified, limitations };
    };
    if !batch.calls.is_empty() && !batches::verify(options, &batch, &mut verified, &mut classified, check) {
        limitations.push(checker_limitation());
    }
    retain_unresolved(&mut limitations, units, &discovery.candidates, &classified);
    FactorySourceDiscovery { candidates: verified, limitations }
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
            calls.push(TypeScriptCall { path: path.to_owned(), start, source_sha256: digest.clone() });
        }
    }
    Some(CallBatch { positions, calls })
}

fn retain_unresolved(
    limitations: &mut Vec<KeyedLimitation>, units: &[KeyedUnit<'_, '_, '_>],
    candidates: &BTreeMap<usize, BTreeSet<usize>>, verified: &BTreeMap<usize, BTreeSet<usize>>,
) {
    let mut unresolved = BTreeSet::new();
    for (unit_index, offsets) in candidates {
        if let Some(unit) = units.get(*unit_index) {
            for &start in offsets {
                if !verified.get(unit_index).is_some_and(|sites| sites.contains(&start)) {
                    let line = unit.source.get(..start).map(|bytes| bytes.iter().filter(|byte| **byte == b'\n').count() as u32 + 1);
                    unresolved.insert((unit.path.to_owned(), line));
                }
            }
        }
    }
    if candidates.keys().all(|index| *index < units.len()) {
        limitations.retain(|issue| issue.reason != "promise_type_unresolved" || issue.path.as_ref().is_none_or(|path| unresolved.contains(&(path.clone(), issue.line))));
    } else {
        limitations.push(checker_limitation());
    }
}

fn checker_limitation() -> KeyedLimitation {
    KeyedLimitation { reason: "typescript_checker_incomplete", path: None, line: None }
}

