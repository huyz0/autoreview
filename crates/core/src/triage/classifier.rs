//! LLM triage classifier (Stage 2, M2) — consulted only inside the
//! ambiguity band `ambiguous_tier_boundary` identifies, per the plan:
//! "Optional cheap-LLM classifier only within an ambiguity band, never
//! exceeding budget ceiling." Outside that band the heuristic score alone
//! decides — this never runs on the common case, only the genuinely
//! borderline one.

use std::path::Path;

use serde::Deserialize;

use autoreview_schema::Tier;

use crate::agents::claude_code::{AgentBackend, InvokeRequest};
use crate::agents::contract::extract_last_fenced_block;
use crate::triage::signals::DiffFacts;

#[derive(Debug, Deserialize)]
struct ClassifierVerdict {
    tier: String,
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "quick" => Some(Tier::Quick),
        "standard" => Some(Tier::Standard),
        "deep" => Some(Tier::Deep),
        _ => None,
    }
}

fn build_prompt(facts: &DiffFacts, score: f64, lower: Tier, upper: Tier) -> String {
    let file_list: String = facts.files.iter().take(20).map(|f| format!("- {} (+{} -{})", f.path, f.additions, f.deletions)).collect::<Vec<_>>().join("\n");
    format!(
        "A code-review triage heuristic scored this diff at {score:.1}, which falls right on the boundary between the '{lower}' and '{upper}' review tiers — the heuristic itself is not confident here. \
        You are the tie-breaker: pick whichever tier actually fits the risk of this diff, based on what changed, not the score.\n\n\
        Changed files ({} total, showing up to 20):\n{file_list}\n\n\
        sensitive path touched: {}\ndependency/lockfile changed: {}\nCI/infra changed: {}\ntests touched alongside source: {}\n\n\
        Respond with ONLY a fenced ```json block of the exact shape {{\"tier\": \"{lower}\"|\"{upper}\"}}. Pick exactly one of those two tiers — do not pick a third tier outside this boundary.",
        facts.files.len(),
        facts.sensitive_path_hit,
        facts.dependency_change,
        facts.ci_or_infra_change,
        facts.tests_touched,
    )
}

/// Asks a cheap model to resolve which of the two boundary tiers fits best.
/// Fails soft: any invoke failure, malformed response, or a tier outside the
/// `{lower, upper}` pair the classifier was actually asked about returns
/// `None`, and the caller keeps the heuristic's own tier — an unreachable or
/// confused classifier should never be able to silently escalate or
/// downgrade a review.
pub fn classify_ambiguous_tier(backend: &dyn AgentBackend, facts: &DiffFacts, score: f64, lower: Tier, upper: Tier, model: &str, cwd: &Path) -> Option<Tier> {
    let request = InvokeRequest {
        prompt: build_prompt(facts, score, lower, upper),
        system_prompt: "You are a precise, low-latency triage classifier for a code review tool. You answer with only the requested JSON, no other tools or exploration needed.".to_string(),
        allowed_tools: vec![],
        max_turns: 1,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
    };

    let invoked = backend.invoke(&request).ok()?;
    let block = extract_last_fenced_block(&invoked.final_text)?;
    let verdict: ClassifierVerdict = serde_json::from_str(block).ok()?;
    let tier = parse_tier(&verdict.tier)?;

    if tier == lower || tier == upper {
        Some(tier)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::InvokeResult;
    use crate::triage::signals::FileChange;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn make_facts() -> DiffFacts {
        DiffFacts {
            repo_root: "/repo".into(),
            base_ref: "main~1".into(),
            head_ref: "main".into(),
            files: vec![FileChange { path: "src/main.go".into(), additions: 5, deletions: 0 }],
            languages: HashMap::from([("go".to_string(), 1)]),
            sensitive_path_hit: false,
            sensitive_paths: vec![],
            dependency_change: false,
            ci_or_infra_change: false,
            tests_touched: false,
            source_touched_without_tests: true,
            added_branch_keywords: 0,
        }
    }

    struct ScriptedBackend {
        response: RefCell<Option<anyhow::Result<String>>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
            match self.response.borrow_mut().take() {
                Some(Ok(text)) => Ok(InvokeResult { final_text: text, usage: Default::default(), wall_ms: 1 }),
                Some(Err(err)) => Err(err),
                None => anyhow::bail!("no scripted response configured"),
            }
        }
    }

    #[test]
    fn picks_the_classified_tier_when_it_is_one_of_the_two_candidates() {
        let backend = ScriptedBackend { response: RefCell::new(Some(Ok("```json\n{\"tier\": \"standard\"}\n```".to_string()))) };
        let tier = classify_ambiguous_tier(&backend, &make_facts(), 22.0, Tier::Quick, Tier::Standard, "haiku", Path::new("/repo"));
        assert_eq!(tier, Some(Tier::Standard));
    }

    #[test]
    fn fails_soft_to_none_on_invoke_error() {
        let backend = ScriptedBackend { response: RefCell::new(Some(Err(anyhow::anyhow!("process failed")))) };
        let tier = classify_ambiguous_tier(&backend, &make_facts(), 22.0, Tier::Quick, Tier::Standard, "haiku", Path::new("/repo"));
        assert_eq!(tier, None);
    }

    #[test]
    fn fails_soft_to_none_on_malformed_response() {
        let backend = ScriptedBackend { response: RefCell::new(Some(Ok("not json at all".to_string()))) };
        let tier = classify_ambiguous_tier(&backend, &make_facts(), 22.0, Tier::Quick, Tier::Standard, "haiku", Path::new("/repo"));
        assert_eq!(tier, None);
    }

    #[test]
    fn rejects_a_tier_outside_the_two_candidates_asked_about() {
        // The classifier was only asked to choose between quick/standard —
        // a "deep" answer would be an escalation it wasn't authorized to
        // make, so this must fail soft rather than honor it.
        let backend = ScriptedBackend { response: RefCell::new(Some(Ok("```json\n{\"tier\": \"deep\"}\n```".to_string()))) };
        let tier = classify_ambiguous_tier(&backend, &make_facts(), 22.0, Tier::Quick, Tier::Standard, "haiku", Path::new("/repo"));
        assert_eq!(tier, None);
    }
}
