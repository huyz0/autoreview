//! Compares a mined candidate seed against every rule already shipped
//! (builtin plus any registered pack) before it ever reaches drafting —
//! closes a gap where nothing previously stopped a mining source from
//! proposing a rule that duplicates one that already exists, leaving a
//! human at `rules review` as the only backstop. Reuses the exact
//! `title_similarity` lexical-similarity bar `mine::mine_candidates`
//! already uses to decide "these describe the same underlying issue,"
//! rather than inventing a second precision/recall tradeoff to reason
//! about — a candidate that would have clustered with an existing
//! finding at that threshold is treated the same way here: probably the
//! same issue.

use serde::Deserialize;

use crate::analyzers::ast_grep::{rule_roots, walk_rule_contents};
use crate::report::dedupe::title_similarity;
use crate::rule_factory::mine::{CandidateSeed, SIMILARITY_THRESHOLD};
use crate::rule_packs::ResolvedRulePack;

/// Just enough of an already-shipped rule's own YAML to compare against a
/// mined candidate — deliberately not `ast_grep::RuleMeta`, which doesn't
/// carry a `message` field at all (it only exists to drive execution
/// dispatch, not human-readable comparison).
#[derive(Debug, Deserialize)]
struct RawRuleFields {
    id: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExistingRuleSummary {
    pub id: String,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExistingRuleMatch {
    pub rule_id: String,
    pub similarity: f64,
}

/// Loads an `id`/`category`/`message` summary for every rule already
/// shipped — builtin plus any registered, resolved pack — reusing the
/// exact `rule_roots`/`walk_rule_contents` walker every rule-kind loader
/// (`taint_rules`, `call_order_rules`, `threshold_rules`) already shares,
/// rather than a fourth hand-rolled directory walk. A rule file with no
/// `message` field (some `kind: threshold` variants) still summarizes
/// cleanly — its `message` is just empty, which can never score above 0
/// similarity, never treated as an error.
pub fn load_existing_rule_summaries(registered_packs: &[ResolvedRulePack]) -> Vec<ExistingRuleSummary> {
    let roots = rule_roots(registered_packs);
    let mut summaries = Vec::new();
    walk_rule_contents(&roots, &mut |contents| {
        if let Ok(raw) = serde_yaml::from_str::<RawRuleFields>(contents) {
            summaries.push(ExistingRuleSummary { id: raw.id, category: raw.category, message: raw.message });
        }
    });
    summaries
}

/// The most similar already-shipped rule to `seed`, if any clears the
/// same lexical-similarity bar `mine_candidates` uses for its own
/// clustering — gated to the same category first, mirroring
/// `mine_candidates`'s own per-category grouping so a security finding
/// can never "match" an unrelated design rule just because the words
/// happen to overlap. Compares against the seed's first representative
/// snippet (the cluster's own representative) rather than every member,
/// same "one representative stands in for the cluster" convention
/// `cluster_id_for` already uses.
pub fn find_similar_existing_rule(seed: &CandidateSeed, existing: &[ExistingRuleSummary]) -> Option<ExistingRuleMatch> {
    let representative = seed.representative_snippets.first()?;
    let seed_text = format!("{} {}", representative.title, representative.message);
    existing
        .iter()
        .filter(|rule| rule.category == seed.category)
        .filter_map(|rule| {
            let similarity = title_similarity(&seed_text, &rule.message);
            (similarity >= SIMILARITY_THRESHOLD).then_some(ExistingRuleMatch { rule_id: rule.id.clone(), similarity })
        })
        .max_by(|a, b| a.similarity.partial_cmp(&b.similarity).unwrap_or(std::cmp::Ordering::Equal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_factory::mine::RepresentativeSnippet;

    fn seed(category: &str, title: &str, message: &str) -> CandidateSeed {
        CandidateSeed {
            cluster_id: "test-cluster".to_string(),
            category: category.to_string(),
            rule_id_or_aspect: "correctness-specialist".to_string(),
            member_fingerprints: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            distinct_run_count: 3,
            representative_snippets: vec![RepresentativeSnippet { fingerprint: "a".to_string(), title: title.to_string(), message: message.to_string() }],
        }
    }

    fn rule(id: &str, category: &str, message: &str) -> ExistingRuleSummary {
        ExistingRuleSummary { id: id.to_string(), category: category.to_string(), message: message.to_string() }
    }

    #[test]
    fn flags_a_seed_that_closely_matches_an_existing_rules_message() {
        let s = seed("security", "Insecure cookie", "Cookie is set without the Secure flag, exposing it over plain HTTP");
        let existing = vec![rule("go-insecure-cookie", "security", "Cookie is set without the Secure flag, exposing it over plain HTTP connections")];
        let m = find_similar_existing_rule(&s, &existing).unwrap();
        assert_eq!(m.rule_id, "go-insecure-cookie");
        assert!(m.similarity >= 0.55, "got: {m:?}");
    }

    #[test]
    fn does_not_match_a_lexically_dissimilar_existing_rule() {
        let s = seed("security", "SQL injection risk", "User input flows into a raw query string");
        let existing = vec![rule("go-insecure-cookie", "security", "Cookie is set without the Secure flag")];
        assert!(find_similar_existing_rule(&s, &existing).is_none());
    }

    #[test]
    fn does_not_match_across_categories_even_with_identical_text() {
        let s = seed("design", "Insecure cookie", "Cookie is set without the Secure flag");
        let existing = vec![rule("go-insecure-cookie", "security", "Cookie is set without the Secure flag")];
        assert!(find_similar_existing_rule(&s, &existing).is_none());
    }

    #[test]
    fn returns_none_against_an_empty_existing_rule_set() {
        let s = seed("security", "Insecure cookie", "Cookie is set without the Secure flag");
        assert!(find_similar_existing_rule(&s, &[]).is_none());
    }

    #[test]
    fn loads_real_builtin_rule_summaries_and_detects_a_seed_matching_a_shipped_rule() {
        // Real end-to-end check against production data: walks the actual
        // embedded `rules-builtin/` tree (no fixtures, no packs) through
        // the same `rule_roots`/`walk_rule_contents` every other loader
        // uses, confirming every shipped rule file parses cleanly into a
        // summary and that a seed closely echoing a real shipped rule's
        // own message gets caught.
        let existing = load_existing_rule_summaries(&[]);
        assert!(existing.len() > 150, "expected ~194 builtin rules, got {}", existing.len());
        let cookie_rule = existing.iter().find(|r| r.id == "go-insecure-cookie").unwrap_or_else(|| panic!("go-insecure-cookie not found among {} loaded rules", existing.len()));
        assert_eq!(cookie_rule.category, "security");
        assert!(!cookie_rule.message.is_empty());

        let s = seed("security", "Insecure cookie found", &cookie_rule.message.clone());
        let m = find_similar_existing_rule(&s, &existing).unwrap();
        assert_eq!(m.rule_id, "go-insecure-cookie", "got: {m:?}");
    }

    #[test]
    fn picks_the_highest_scoring_match_when_multiple_rules_are_similar() {
        let s = seed("security", "Insecure cookie", "Cookie is set without the Secure flag, exposing it over plain HTTP");
        let existing = vec![
            rule("go-insecure-cookie", "security", "Cookie is set without the Secure flag, exposing it over plain HTTP connections"),
            rule("java-insecure-cookie", "security", "Cookie lacks Secure flag"),
        ];
        let m = find_similar_existing_rule(&s, &existing).unwrap();
        assert_eq!(m.rule_id, "go-insecure-cookie", "the closer textual match should win, got: {m:?}");
    }
}
