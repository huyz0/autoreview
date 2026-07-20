//! Acceptance-criteria verification (Initiative 3, distilled from Aviator
//! Verify's spec-first model — see SESSION_NOTES.md): an optional
//! `.autoreview/spec.md` states what a change is *for* and a bullet list of
//! independently-verifiable claims, checked against the diff by an LLM
//! judge — a genuinely different question than "is this code clean" (what
//! every other stage in this pipeline asks), so it's additive to the
//! existing finding-based review, not a replacement for any of it.
//!
//! Same fail-open posture as Stage 3.5's `verify` module, adapted for the
//! fact that there's no "finding" to keep here: an invocation failure or a
//! malformed response marks every criterion `Uncertain` (with an evidence
//! note explaining why) rather than silently reporting nothing — a report
//! with zero verdicts looks identical to "everything passed," which would
//! be actively misleading for a check whose entire point is trustworthy
//! answers to "does this diff actually do what it claims."

pub mod parse;

use std::path::Path;

use autoreview_schema::{AcceptanceSpec, CriterionResult, CriterionVerdict};

use crate::agents::claude_code::{AgentBackend, InvokeRequest, Usage};
use crate::agents::contract::extract_last_fenced_block;

pub use parse::parse_spec;

fn build_spec_verify_prompt(spec: &AcceptanceSpec, diff_text: &str) -> String {
    let numbered: String = spec.criteria.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, c)).collect::<Vec<_>>().join("\n");
    format!(
        "You are verifying whether a code change satisfies a set of stated acceptance criteria — this is a factual check against the diff, not a general code review.\n\n\
        Change intent: {}\n\nAcceptance criteria to check, in order:\n{}\n\n\
        The diff:\n```diff\n{}\n```\n\n\
        For EACH criterion above, decide: \"satisfied\" (the diff clearly does this), \"not-satisfied\" (the diff clearly does NOT do this), or \"uncertain\" (the diff/surrounding code doesn't settle it either way — prefer this over guessing). \
        Respond with ONLY a fenced ```json block containing a JSON array with exactly {} object(s), one per criterion in the same order, of the exact shape \
        [{{\"criterion\": \"...\", \"verdict\": \"satisfied\"|\"not-satisfied\"|\"uncertain\", \"evidence\": \"one sentence citing what you saw (or didn't see) in the diff\"}}].",
        spec.intent,
        numbered,
        diff_text,
        spec.criteria.len()
    )
}

pub struct SpecVerifyResult {
    pub results: Vec<CriterionResult>,
    pub usage: Usage,
    pub wall_ms: u64,
}

fn uncertain_fallback(spec: &AcceptanceSpec, reason: &str, usage: Usage, wall_ms: u64) -> SpecVerifyResult {
    SpecVerifyResult {
        results: spec.criteria.iter().map(|c| CriterionResult { criterion: c.clone(), verdict: CriterionVerdict::Uncertain, evidence: reason.to_string() }).collect(),
        usage,
        wall_ms,
    }
}

/// Checks every criterion in `spec` against `diff_text` in a single batched
/// call (cheaper than one call per criterion, and lets the judge cross-
/// reference criteria against each other if useful) — same `Read`/`Grep`
/// tool access as Stage 3.5's verifier, since settling a criterion often
/// needs more context than the diff hunk alone shows.
pub fn run_spec_verify(backend: &dyn AgentBackend, spec: &AcceptanceSpec, diff_text: &str, model: &str, max_turns: u32, cwd: &Path) -> SpecVerifyResult {
    let prompt = build_spec_verify_prompt(spec, diff_text);
    let request = InvokeRequest {
        prompt,
        system_prompt: "You are a precise, factual verifier checking a code change against explicit acceptance criteria. You only mark a criterion satisfied when the diff clearly shows it, and prefer \"uncertain\" over guessing.".to_string(),
        allowed_tools: vec!["Read".to_string(), "Grep".to_string()],
        max_turns,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
    };

    let invoked = match backend.invoke(&request) {
        Ok(result) => result,
        Err(err) => return uncertain_fallback(spec, &format!("verification call failed to invoke ({err})"), Usage::default(), 0),
    };

    let parsed = extract_last_fenced_block(&invoked.final_text).and_then(|block| serde_json::from_str::<Vec<CriterionResult>>(block).ok());
    match parsed {
        Some(results) if results.len() == spec.criteria.len() => SpecVerifyResult { results, usage: invoked.usage, wall_ms: invoked.wall_ms },
        _ => uncertain_fallback(spec, "verification response did not match the expected contract (missing or malformed JSON array)", invoked.usage, invoked.wall_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn make_spec(criteria: Vec<&str>) -> AcceptanceSpec {
        AcceptanceSpec { title: "Add rate limiting".to_string(), intent: "Cap per-user request rate".to_string(), criteria: criteria.into_iter().map(str::to_string).collect() }
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
    fn parses_a_well_formed_batched_verdict_array() {
        let spec = make_spec(vec!["Returns 429 when exceeded", "Includes Retry-After header"]);
        let response = r#"```json
[
  {"criterion": "Returns 429 when exceeded", "verdict": "satisfied", "evidence": "the handler returns http.StatusTooManyRequests"},
  {"criterion": "Includes Retry-After header", "verdict": "not-satisfied", "evidence": "no Retry-After header is set anywhere in the diff"}
]
```"#;
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok(response.to_string())]) };
        let result = run_spec_verify(&backend, &spec, "diff text", "haiku", 4, Path::new("/repo"));
        assert_eq!(result.results.len(), 2);
        assert_eq!(result.results[0].verdict, CriterionVerdict::Satisfied);
        assert_eq!(result.results[1].verdict, CriterionVerdict::NotSatisfied);
    }

    #[test]
    fn an_invoke_failure_marks_every_criterion_uncertain_not_silently_empty() {
        let spec = make_spec(vec!["A", "B", "C"]);
        let backend = ScriptedBackend { responses: RefCell::new(vec![]) };
        let result = run_spec_verify(&backend, &spec, "diff text", "haiku", 4, Path::new("/repo"));
        assert_eq!(result.results.len(), 3, "a failed call must still report one uncertain verdict per criterion, not zero");
        assert!(result.results.iter().all(|r| r.verdict == CriterionVerdict::Uncertain));
    }

    #[test]
    fn a_malformed_response_marks_every_criterion_uncertain() {
        let spec = make_spec(vec!["A", "B"]);
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("no json here".to_string())]) };
        let result = run_spec_verify(&backend, &spec, "diff text", "haiku", 4, Path::new("/repo"));
        assert_eq!(result.results.len(), 2);
        assert!(result.results.iter().all(|r| r.verdict == CriterionVerdict::Uncertain));
    }

    #[test]
    fn a_response_with_the_wrong_number_of_verdicts_falls_back_to_uncertain() {
        // The model answered only 1 of 2 criteria — treated the same as a
        // malformed response, not partially trusted, since we can't safely
        // know which criterion the lone verdict corresponds to.
        let spec = make_spec(vec!["A", "B"]);
        let response = r#"```json
[{"criterion": "A", "verdict": "satisfied", "evidence": "yes"}]
```"#;
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok(response.to_string())]) };
        let result = run_spec_verify(&backend, &spec, "diff text", "haiku", 4, Path::new("/repo"));
        assert_eq!(result.results.len(), 2);
        assert!(result.results.iter().all(|r| r.verdict == CriterionVerdict::Uncertain));
    }
}
