//! Stage 3.5: a judge pass over findings before they're finalized, distinct
//! from Stage 3's output-contract validation. Per the plan's prior-art
//! research pass, "grounded generation + a separate judge" measurably beats
//! generation alone at killing false positives (an 88.6% FP reduction for
//! 3.1% recall loss is the reference number) — folding this into dedupe as
//! an afterthought would bury that gain, so it's its own stage with its own
//! `perStage` cost entry.
//!
//! Only ever *demotes* (suppresses) a finding on an explicit refute vote;
//! never invents new findings. Fails open: if the judge call itself fails to
//! invoke or its response fails the output contract, the finding is kept
//! rather than silently dropped — an unreachable judge should never be able
//! to suppress a real issue.

use std::path::Path;

use serde::Deserialize;

use autoreview_schema::{Finding, FindingSourceKind, Severity, SuppressedFinding, SuppressedReason};

use crate::agents::claude_code::{AgentBackend, InvokeRequest, Usage};
use crate::agents::contract::extract_last_fenced_block;

#[derive(Debug, Clone, Deserialize)]
struct VerdictJson {
    keep: bool,
    reason: String,
}

/// Selects the findings this pass should re-check: agent findings at
/// high/blocker severity (the plan's stated target), analyzer findings from
/// configured noisy categories, and any finding from a rule explicitly
/// marked `semantic: true` (syntactically precise, semantically
/// approximate — always double-checked regardless of its own
/// severity/category). Everything else passes through unexamined — this
/// pass is deliberately narrow, not a second full review.
pub fn select_for_verification<'a>(findings: &'a [Finding], noisy_categories: &[String], semantic_rule_ids: &std::collections::HashSet<String>) -> Vec<&'a Finding> {
    findings
        .iter()
        .filter(|f| {
            let is_high_severity_agent = matches!(f.source.kind, FindingSourceKind::Agent) && matches!(f.severity, Severity::Blocker | Severity::High);
            let is_noisy_analyzer = matches!(f.source.kind, FindingSourceKind::Analyzer) && noisy_categories.iter().any(|c| c == &f.category);
            let is_semantic_rule = f.source.rule_id.as_deref().is_some_and(|id| semantic_rule_ids.contains(id));
            is_high_severity_agent || is_noisy_analyzer || is_semantic_rule
        })
        .collect()
}

fn build_verify_prompt(finding: &Finding, diff_text: &str) -> String {
    format!(
        "You are a skeptical verifier, not the original reviewer. A prior review step flagged the following finding. Your ONLY job is to check whether the flagged code, as it actually appears in the diff below, genuinely has this problem — not to find new issues.\n\n\
        Finding to verify:\n- category: {}\n- severity: {:?}\n- title: {}\n- message: {}\n- location: {}:{}\n- snippet:\n{}\n\n\
        The diff:\n```diff\n{}\n```\n\n\
        Respond with ONLY a fenced ```json block of the exact shape {{\"keep\": true|false, \"reason\": \"...\"}}. \
        Set \"keep\": false ONLY if you are confident the flagged code does not actually have this problem (e.g. the snippet doesn't match, the condition described isn't present, or it's already handled elsewhere in the diff). \
        If you are unsure, default to \"keep\": true — this check exists to catch clear false positives, not to second-guess judgment calls.",
        finding.category, finding.severity, finding.title, finding.message, finding.location.path, finding.location.range.start_line, finding.location.snippet, diff_text
    )
}

/// Outcome of verifying one finding: whether to keep it, why, and the
/// resources spent finding out.
pub struct VerifyResult {
    pub keep: bool,
    pub reason: String,
    pub usage: Usage,
    pub wall_ms: u64,
}

pub fn verify_finding(backend: &dyn AgentBackend, finding: &Finding, diff_text: &str, model: &str, max_turns: u32, cwd: &Path) -> VerifyResult {
    let prompt = build_verify_prompt(finding, diff_text);
    let request = InvokeRequest {
        prompt,
        system_prompt: "You are a careful, skeptical code-review verifier. You only refute findings you are confident are wrong.".to_string(),
        allowed_tools: vec!["Read".to_string(), "Grep".to_string()],
        max_turns,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
    };

    let invoked = match backend.invoke(&request) {
        Ok(result) => result,
        Err(err) => {
            return VerifyResult { keep: true, reason: format!("verify pass failed to invoke ({err}) — keeping finding (fail-open)"), usage: Usage::default(), wall_ms: 0 };
        }
    };

    let parsed = extract_last_fenced_block(&invoked.final_text).and_then(|block| serde_json::from_str::<VerdictJson>(block).ok());

    match parsed {
        Some(verdict) => VerifyResult { keep: verdict.keep, reason: verdict.reason, usage: invoked.usage, wall_ms: invoked.wall_ms },
        None => VerifyResult {
            keep: true,
            reason: "verify pass response did not match the verdict contract — keeping finding (fail-open)".to_string(),
            usage: invoked.usage,
            wall_ms: invoked.wall_ms,
        },
    }
}

pub struct VerifyPassResult {
    pub kept: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
    pub usage: Usage,
    pub wall_ms: u64,
    pub checked: usize,
}

/// Runs the verify pass over a finding set: findings selected by
/// `select_for_verification` are judged one at a time; everything else
/// passes through untouched. Findings are matched by id (stable within one
/// run) rather than re-filtering, so the selection logic and the
/// keep/suppress split can never disagree about which findings were checked.
pub fn run_verify_pass(backend: &dyn AgentBackend, findings: Vec<Finding>, diff_text: &str, model: &str, max_turns: u32, cwd: &Path, noisy_categories: &[String], semantic_rule_ids: &std::collections::HashSet<String>) -> VerifyPassResult {
    let to_verify: std::collections::HashSet<String> = select_for_verification(&findings, noisy_categories, semantic_rule_ids).into_iter().map(|f| f.id.clone()).collect();

    let mut kept = Vec::with_capacity(findings.len());
    let mut suppressed = Vec::new();
    let mut usage = Usage::default();
    let mut wall_ms = 0u64;
    let mut checked = 0usize;

    for finding in findings {
        if !to_verify.contains(&finding.id) {
            kept.push(finding);
            continue;
        }
        checked += 1;
        let result = verify_finding(backend, &finding, diff_text, model, max_turns, cwd);
        usage.input_tokens += result.usage.input_tokens;
        usage.output_tokens += result.usage.output_tokens;
        wall_ms += result.wall_ms;

        if result.keep {
            kept.push(finding);
        } else {
            suppressed.push(SuppressedFinding { finding, reason: SuppressedReason::Refuted });
        }
    }

    VerifyPassResult { kept, suppressed, usage, wall_ms, checked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{FindingFingerprints, FindingSource, Location, LocationRange, Side};
    use std::cell::RefCell;

    fn make_finding(id: &str, kind: FindingSourceKind, category: &str, severity: Severity) -> Finding {
        Finding {
            id: id.to_string(),
            fingerprints: FindingFingerprints { primary: id.to_string(), secondary: None },
            source: FindingSource { kind, tool: "claude-code".into(), rule_id: None, aspect: Some("security".into()), backend: None },
            category: category.to_string(),
            severity,
            confidence: 0.8,
            title: "t".into(),
            message: "m".into(),
            location: Location { path: "a.ts".into(), range: LocationRange { start_line: 1, ..Default::default() }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    #[test]
    fn selects_high_and_blocker_agent_findings_regardless_of_category() {
        let findings = vec![
            make_finding("a", FindingSourceKind::Agent, "correctness", Severity::High),
            make_finding("b", FindingSourceKind::Agent, "correctness", Severity::Blocker),
            make_finding("c", FindingSourceKind::Agent, "correctness", Severity::Medium),
        ];
        let selected: Vec<&str> = select_for_verification(&findings, &[], &std::collections::HashSet::new()).into_iter().map(|f| f.id.as_str()).collect();
        assert_eq!(selected, vec!["a", "b"]);
    }

    #[test]
    fn selects_analyzer_findings_only_from_noisy_categories() {
        let findings = vec![
            make_finding("a", FindingSourceKind::Analyzer, "style", Severity::Low),
            make_finding("b", FindingSourceKind::Analyzer, "correctness", Severity::Low),
        ];
        let selected: Vec<&str> =
            select_for_verification(&findings, &["style".to_string()], &std::collections::HashSet::new()).into_iter().map(|f| f.id.as_str()).collect();
        assert_eq!(selected, vec!["a"]);
    }

    #[test]
    fn selects_a_finding_whose_rule_is_marked_semantic_regardless_of_category_or_severity() {
        let findings = vec![
            make_finding("a", FindingSourceKind::Analyzer, "performance", Severity::Low),
            make_finding("b", FindingSourceKind::Analyzer, "performance", Severity::Low),
        ];
        let mut findings = findings;
        findings[0].source.rule_id = Some("go-nested-loop-linear-search".to_string());
        let semantic: std::collections::HashSet<String> = ["go-nested-loop-linear-search".to_string()].into_iter().collect();
        let selected: Vec<&str> = select_for_verification(&findings, &[], &semantic).into_iter().map(|f| f.id.as_str()).collect();
        assert_eq!(selected, vec!["a"]);
    }

    struct ScriptedBackend {
        responses: RefCell<Vec<anyhow::Result<String>>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<crate::agents::claude_code::InvokeResult> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                anyhow::bail!("no more scripted responses");
            }
            responses.remove(0).map(|text| crate::agents::claude_code::InvokeResult { final_text: text, usage: Usage::default(), wall_ms: 5 })
        }
    }

    #[test]
    fn a_refute_verdict_suppresses_the_finding() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("```json\n{\"keep\": false, \"reason\": \"snippet doesn't match the diff\"}\n```".to_string())]) };
        let findings = vec![make_finding("a", FindingSourceKind::Agent, "correctness", Severity::High)];
        let result = run_verify_pass(&backend, findings, "diff text", "haiku", 2, Path::new("/repo"), &[], &std::collections::HashSet::new());
        assert!(result.kept.is_empty());
        assert_eq!(result.suppressed.len(), 1);
        assert!(matches!(result.suppressed[0].reason, SuppressedReason::Refuted));
        assert_eq!(result.checked, 1);
    }

    #[test]
    fn a_keep_verdict_leaves_the_finding_in_place() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("```json\n{\"keep\": true, \"reason\": \"confirmed\"}\n```".to_string())]) };
        let findings = vec![make_finding("a", FindingSourceKind::Agent, "correctness", Severity::High)];
        let result = run_verify_pass(&backend, findings, "diff text", "haiku", 2, Path::new("/repo"), &[], &std::collections::HashSet::new());
        assert_eq!(result.kept.len(), 1);
        assert!(result.suppressed.is_empty());
    }

    #[test]
    fn findings_outside_the_selection_are_never_invoked_on() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![]) };
        let findings = vec![make_finding("a", FindingSourceKind::Agent, "correctness", Severity::Medium)];
        let result = run_verify_pass(&backend, findings, "diff text", "haiku", 2, Path::new("/repo"), &[], &std::collections::HashSet::new());
        assert_eq!(result.kept.len(), 1);
        assert_eq!(result.checked, 0);
    }

    #[test]
    fn an_invoke_failure_fails_open_and_keeps_the_finding() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![]) };
        let findings = vec![make_finding("a", FindingSourceKind::Agent, "correctness", Severity::Blocker)];
        let result = run_verify_pass(&backend, findings, "diff text", "haiku", 2, Path::new("/repo"), &[], &std::collections::HashSet::new());
        assert_eq!(result.kept.len(), 1);
        assert!(result.suppressed.is_empty());
    }

    #[test]
    fn a_malformed_verdict_response_fails_open_and_keeps_the_finding() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("no json here".to_string())]) };
        let findings = vec![make_finding("a", FindingSourceKind::Agent, "correctness", Severity::Blocker)];
        let result = run_verify_pass(&backend, findings, "diff text", "haiku", 2, Path::new("/repo"), &[], &std::collections::HashSet::new());
        assert_eq!(result.kept.len(), 1);
        assert!(result.suppressed.is_empty());
    }
}
