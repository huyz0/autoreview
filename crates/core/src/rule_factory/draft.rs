//! Rule-factory drafting: for a mined `CandidateSeed`, ask an LLM backend
//! to draft an ast-grep rule 5 times independently and keep only the
//! pattern the attempts agree on — SemOpt's ensemble-generation approach
//! (drafted-5-times-independently, kept-if-cross-generation-agreement),
//! precedented in the plan's prior-art research pass as a cheap pre-filter
//! ahead of the (not-yet-built) bench stage, rather than benching every
//! single-shot draft.
//!
//! Known scope limitation, stated plainly rather than glossed over: a
//! `CandidateSeed`'s representative snippets are the *finding's own*
//! title/message text (what the specialist said was wrong), not the
//! original source code the specialist was looking at — the history store
//! doesn't retain that. So this stage drafts a rule against a *description*
//! of the recurring issue, not real code, and a human reviewing a resulting
//! candidate (via `rules review`) needs to supply real positive/negative
//! fixtures before it can be benched. Attaching the original snippet to the
//! mined finding row is a natural follow-up once bench needs it.

use std::path::Path;

use crate::agents::claude_code::{AgentBackend, InvokeRequest, Usage};
use crate::agents::contract::extract_last_fenced_block;
use crate::rule_factory::mine::CandidateSeed;

const ATTEMPTS: usize = 5;
const MIN_AGREEMENT: usize = 3;
const INEXPRESSIBLE_MARKER: &str = "INEXPRESSIBLE:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    /// At least `MIN_AGREEMENT` of `ATTEMPTS` independent drafts converged
    /// on the same pattern (compared after whitespace normalization).
    /// Carries one representative attempt's raw text (not normalized) as
    /// the actual candidate rule YAML.
    Drafted { rule_yaml: String, agreement_count: usize },
    /// Fewer than `MIN_AGREEMENT` attempts agreed on any single pattern, or
    /// a majority of attempts themselves declared the cluster inexpressible
    /// as a single-file syntactic rule (e.g. needs cross-file type info).
    Inexpressible { rationale: String },
}

fn build_draft_prompt(seed: &CandidateSeed) -> String {
    let examples: String = seed
        .representative_snippets
        .iter()
        .map(|s| format!("- title: {}\n  message: {}", s.title, s.message))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "You are drafting a deterministic ast-grep YAML rule to catch a class of code-review finding a human/AI reviewer keeps flagging by hand. \
        Category: {}\nRecurring finding examples from this cluster:\n{}\n\n\
        Decide: can this class of issue be caught by a single-file, syntactic ast-grep pattern (tree-sitter based, no cross-file/type information)? \
        If YES, respond with ONLY a fenced ```yaml block containing one ast-grep rule with fields: id, language, category, severity, message, rule (and constraints if needed), following ast-grep's YAML rule schema. \
        If NO — the issue genuinely needs cross-file reasoning, type information, or subjective judgment a syntactic pattern can't express — respond with ONLY a fenced ```text block whose content starts with \"{INEXPRESSIBLE_MARKER}\" followed by a one-sentence reason.",
        seed.category, examples
    )
}

fn normalize_yaml(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs `ATTEMPTS` independent drafting calls against `backend` for one
/// seed and returns the ensemble-agreement verdict. Each attempt that fails
/// to invoke or produces no fenced block is simply dropped from the
/// agreement count (fail-soft) — it does not itself count as a vote either
/// way, since a broken invocation isn't a considered opinion.
pub fn draft_candidate(backend: &dyn AgentBackend, seed: &CandidateSeed, model: &str, max_turns: u32, cwd: &Path) -> (DraftOutcome, Usage) {
    let prompt = build_draft_prompt(seed);
    let mut total_usage = Usage::default();
    let mut yaml_attempts: Vec<String> = Vec::new();
    let mut inexpressible_count = 0usize;

    for _ in 0..ATTEMPTS {
        let request = InvokeRequest {
            prompt: prompt.clone(),
            system_prompt: "You are a precise rule-authoring assistant for a static-analysis rule pack. You only claim a pattern is expressible when you are confident a syntactic tree-sitter match can express it.".to_string(),
            allowed_tools: vec![],
            max_turns,
            model: model.to_string(),
            cwd: cwd.to_path_buf(),
        };
        let Ok(result) = backend.invoke(&request) else { continue };
        total_usage.add(&result.usage);
        let Some(block) = extract_last_fenced_block(&result.final_text) else { continue };
        if block.trim_start().starts_with(INEXPRESSIBLE_MARKER) {
            inexpressible_count += 1;
        } else {
            yaml_attempts.push(block.to_string());
        }
    }

    let outcome = best_agreement(&yaml_attempts, inexpressible_count);
    (outcome, total_usage)
}

fn best_agreement(yaml_attempts: &[String], inexpressible_count: usize) -> DraftOutcome {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (idx, attempt) in yaml_attempts.iter().enumerate() {
        let key = normalize_yaml(attempt);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(idx),
            None => groups.push((key, vec![idx])),
        }
    }

    let best = groups.iter().max_by_key(|(_, members)| members.len());
    if let Some((_, members)) = best {
        if members.len() >= MIN_AGREEMENT {
            return DraftOutcome::Drafted { rule_yaml: yaml_attempts[members[0]].clone(), agreement_count: members.len() };
        }
    }

    if inexpressible_count >= MIN_AGREEMENT {
        return DraftOutcome::Inexpressible { rationale: "a majority of drafting attempts judged this cluster inexpressible as a syntactic rule".to_string() };
    }

    DraftOutcome::Inexpressible { rationale: format!("fewer than {MIN_AGREEMENT} of {ATTEMPTS} drafting attempts agreed on a single pattern") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::InvokeResult;
    use crate::rule_factory::mine::RepresentativeSnippet;
    use std::cell::RefCell;

    fn make_seed() -> CandidateSeed {
        CandidateSeed {
            cluster_id: "abc123".to_string(),
            category: "correctness".to_string(),
            rule_id_or_aspect: "correctness-specialist".to_string(),
            member_fingerprints: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            distinct_run_count: 3,
            representative_snippets: vec![RepresentativeSnippet { fingerprint: "a".to_string(), title: "Missing null check".to_string(), message: "Parameter x is not null-checked".to_string() }],
        }
    }

    struct ScriptedBackend {
        responses: RefCell<Vec<&'static str>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                anyhow::bail!("no more scripted responses");
            }
            let text = responses.remove(0);
            Ok(InvokeResult { final_text: text.to_string(), usage: Usage::default(), wall_ms: 1 })
        }
    }

    const YAML_A: &str = "```yaml\nid: go-example\nlanguage: Go\ncategory: correctness\nrule:\n  pattern: foo($X)\n```";
    const YAML_A_REWRAPPED: &str = "```yaml\n  id: go-example\n  language: Go\n  category: correctness\n  rule:\n    pattern: foo($X)\n```";
    const YAML_B: &str = "```yaml\nid: go-other\nlanguage: Go\ncategory: correctness\nrule:\n  pattern: bar($X)\n```";
    const INEXPRESSIBLE: &str = "```text\nINEXPRESSIBLE: needs cross-file type information\n```";

    #[test]
    fn drafts_a_rule_when_a_majority_of_attempts_agree() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![YAML_A, YAML_A_REWRAPPED, YAML_A, YAML_B, INEXPRESSIBLE]) };
        let (outcome, _usage) = draft_candidate(&backend, &make_seed(), "cheap-model", 4, Path::new("/tmp"));
        match outcome {
            DraftOutcome::Drafted { agreement_count, .. } => assert_eq!(agreement_count, 3),
            other => panic!("expected Drafted, got {other:?}"),
        }
    }

    #[test]
    fn marks_inexpressible_when_no_pattern_reaches_majority_agreement() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![YAML_A, YAML_B, INEXPRESSIBLE, INEXPRESSIBLE, INEXPRESSIBLE]) };
        let (outcome, _usage) = draft_candidate(&backend, &make_seed(), "cheap-model", 4, Path::new("/tmp"));
        assert!(matches!(outcome, DraftOutcome::Inexpressible { .. }));
    }

    #[test]
    fn marks_inexpressible_when_attempts_are_split_with_no_majority() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![YAML_A, YAML_B, YAML_A, YAML_B, INEXPRESSIBLE]) };
        let (outcome, _usage) = draft_candidate(&backend, &make_seed(), "cheap-model", 4, Path::new("/tmp"));
        assert!(matches!(outcome, DraftOutcome::Inexpressible { .. }));
    }

    #[test]
    fn failed_invocations_are_dropped_not_counted_either_way() {
        // Only 3 of 5 "attempts" actually succeed (backend errors on the
        // rest, having run out of scripted responses); all 3 agree, which
        // still clears the majority bar.
        let backend = ScriptedBackend { responses: RefCell::new(vec![YAML_A, YAML_A, YAML_A]) };
        let (outcome, _usage) = draft_candidate(&backend, &make_seed(), "cheap-model", 4, Path::new("/tmp"));
        match outcome {
            DraftOutcome::Drafted { agreement_count, .. } => assert_eq!(agreement_count, 3),
            other => panic!("expected Drafted, got {other:?}"),
        }
    }
}
