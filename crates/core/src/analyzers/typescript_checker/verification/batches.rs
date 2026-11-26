use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use super::{CallBatch, CheckerConfig, TypeScriptRequest};
use crate::analyzers::typescript_checker::{TypeScriptCall, TypeScriptResult, MAX_BYTES};

type Positions = BTreeMap<usize, BTreeSet<usize>>;

// Reserve space for each response's status field as well as the JSON envelope.
// A single overall deadline bounds repeated compiler startups.
pub(super) fn verify(
    options: &CheckerConfig<'_>, batch: &CallBatch, verified: &mut Positions, classified: &mut Positions,
    mut check: impl FnMut(&str, &TypeScriptRequest<'_>, Duration) -> Option<TypeScriptResult>,
) -> Result<(), &'static str> {
    let started = Instant::now();
    let Some((ranges, skipped)) = ranges(options, &batch.calls) else { return Err("typescript_checker_incomplete") };
    for range in ranges {
        let remaining = options.timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err("typescript_checker_incomplete"); }
        let request = TypeScriptRequest { typescript_module: options.compiler, config: options.config, calls: &batch.calls[range.clone()] };
        let Some(result) = check(options.node, &request, remaining) else { return Err("typescript_checker_incomplete") };
        if result.status != "ok" { return Err(limitation_reason(result.reason.as_deref())); }
        record_classifications(&batch.positions[range], result, verified, classified);
    }
    if skipped { Err("typescript_checker_incomplete") } else { Ok(()) }
}

fn record_classifications(positions: &[(usize, usize)], result: TypeScriptResult, verified: &mut Positions, classified: &mut Positions) {
    for ((unit, start), result) in positions.iter().copied().zip(result.calls.unwrap_or_default()) {
        if result.status == "promise_like" { verified.entry(unit).or_default().insert(start); }
        if matches!(result.status.as_str(), "promise_like" | "not_promise") { classified.entry(unit).or_default().insert(start); }
    }
}

fn limitation_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("compiler_api_unsupported") => "typescript_compiler_api_unsupported",
        Some("project_type_errors") => "typescript_project_type_errors",
        Some("config_invalid") => "typescript_config_invalid",
        Some("config_unreadable") => "typescript_config_unreadable",
        _ => "typescript_checker_incomplete",
    }
}

fn ranges(options: &CheckerConfig<'_>, calls: &[TypeScriptCall]) -> Option<(Vec<Range<usize>>, bool)> {
    let header = TypeScriptRequest { typescript_module: options.compiler, config: options.config, calls: &[] };
    let limit = MAX_BYTES.checked_sub(1024 + serde_json::to_vec(&header).ok()?.len())?;
    let mut ranges = Vec::new();
    let mut first = 0;
    let mut used = 0;
    let mut skipped = false;
    for (index, call) in calls.iter().enumerate() {
        let cost = serde_json::to_vec(call).ok()?.len().checked_add(32)?;
        if cost > limit {
            if used > 0 { ranges.push(first..index); }
            first = index + 1;
            used = 0;
            skipped = true;
            continue;
        }
        if used + cost > limit {
            ranges.push(first..index);
            first = index;
            used = 0;
        }
        used += cost;
    }
    if used > 0 { ranges.push(first..calls.len()); }
    Some((ranges, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::typescript_checker::TypeScriptCallResult;
    use std::path::Path;

    #[test]
    fn checker_failures_use_only_known_structured_reasons() {
        assert_eq!(limitation_reason(Some("compiler_api_unsupported")), "typescript_compiler_api_unsupported");
        assert_eq!(limitation_reason(Some("project_type_errors")), "typescript_project_type_errors");
        assert_eq!(limitation_reason(Some("config_invalid")), "typescript_config_invalid");
        assert_eq!(limitation_reason(Some("config_unreadable")), "typescript_config_unreadable");
        assert_eq!(limitation_reason(Some("untrusted arbitrary output")), "typescript_checker_incomplete");
    }

    #[test]
    fn batches_fit_both_protocol_bounds_and_keep_completed_results_on_failure() {
        let options = CheckerConfig { node: "node", compiler: "compiler", config: "config", root: Path::new("."), timeout: Duration::from_secs(10) };
        let calls: Vec<_> = (0..2000).map(|start| TypeScriptCall { path: format!("{}é\n", "x".repeat(160)), start, source_sha256: "a".repeat(64) }).collect();
        let (ranges, skipped) = ranges(&options, &calls).unwrap();
        assert!(!skipped);
        assert!(ranges.len() > 1);
        for range in &ranges {
            let request = TypeScriptRequest { typescript_module: options.compiler, config: options.config, calls: &calls[range.clone()] };
            assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_BYTES);
            let response = serde_json::json!({"status":"ok","calls":calls[range.clone()].iter().map(|call| serde_json::json!({"path":call.path,"start":call.start,"source_sha256":call.source_sha256,"status":"source_changed"})).collect::<Vec<_>>()});
            assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_BYTES);
        }
        let first_len = ranges[0].len();
        let batch = CallBatch { positions: (0..calls.len()).map(|start| (0, start)).collect(), calls };
        let mut verified = Positions::new();
        let mut classified = Positions::new();
        let mut invocations = 0;
        let complete = verify(&options, &batch, &mut verified, &mut classified, |_, request, _| {
            invocations += 1;
            (invocations == 1).then(|| TypeScriptResult { status: "ok".into(), reason: None, calls: Some(request.calls.iter().map(|call| TypeScriptCallResult { path: call.path.clone(), start: call.start, source_sha256: call.source_sha256.clone(), status: "promise_like".into() }).collect()) })
        });
        assert_eq!(complete, Err("typescript_checker_incomplete"));
        assert_eq!(verified[&0].len(), first_len);
        assert_eq!(classified[&0].len(), first_len);
        assert_eq!(invocations, 2);
    }

    #[test]
    fn oversized_call_does_not_discard_other_batches() {
        let options = CheckerConfig { node: "node", compiler: "compiler", config: "config", root: Path::new("."), timeout: Duration::from_secs(10) };
        let make = |start, length| TypeScriptCall { path: "x".repeat(length), start, source_sha256: "a".repeat(64) };
        let calls = vec![make(0, 10), make(1, MAX_BYTES), make(2, 10)];
        let (ranges, skipped) = ranges(&options, &calls).unwrap();
        assert!(skipped);
        assert_eq!(ranges, [0..1, 2..3]);
        let batch = CallBatch { positions: vec![(0, 0), (0, 1), (0, 2)], calls };
        let mut verified = Positions::new();
        let mut classified = Positions::new();
        let complete = verify(&options, &batch, &mut verified, &mut classified, |_, request, _| {
            Some(TypeScriptResult { status: "ok".into(), reason: None, calls: Some(request.calls.iter().map(|call| TypeScriptCallResult { path: call.path.clone(), start: call.start, source_sha256: call.source_sha256.clone(), status: "promise_like".into() }).collect()) })
        });
        assert_eq!(complete, Err("typescript_checker_incomplete"));
        assert_eq!(verified[&0], BTreeSet::from([0, 2]));
        assert_eq!(classified[&0], BTreeSet::from([0, 2]));
    }
}
