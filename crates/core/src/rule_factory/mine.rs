//! Rule-factory mining: groups recurring agent findings into candidate
//! seeds for the (not-yet-built) draft stage. Per the plan's Self-improvement
//! section: cluster by lexical similarity — trigram-shingle Jaccard over
//! `title + message`, threshold >= 0.55 — deliberately not embeddings, since
//! clustering candidates for a *deterministic* rule should stay as
//! explainable to the human reviewer as the rule itself will be. A cluster
//! becomes a candidate seed when it has >= 3 members spanning >= 2 distinct
//! runs (a one-off or a single large run's repeats isn't "recurring").
//!
//! Scoping note: the plan groups by `(aspect, category, language)`, but
//! today's `findings` table records neither a per-finding language nor a
//! distinct aspect field wide enough to group by (see `MinedFindingRow`) —
//! `category` is the coarser, actually-available grouping key, so that's
//! what's used here. Revisit once the schema carries language/aspect
//! per-row.

use std::collections::HashSet;

use crate::report::dedupe::title_similarity;
use crate::storage::history_store::MinedFindingRow;

const SIMILARITY_THRESHOLD: f64 = 0.55;
const MIN_CLUSTER_MEMBERS: usize = 3;
const MIN_DISTINCT_RUNS: usize = 2;
/// How many representative snippets a seed carries for the (future) draft
/// stage to work from — enough to draft against without embedding every
/// member's full text.
const MAX_REPRESENTATIVE_SNIPPETS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepresentativeSnippet {
    pub fingerprint: String,
    pub title: String,
    pub message: String,
}

/// A cluster of recurring, lexically-similar agent findings, large and
/// spread-out enough across runs to be worth drafting a deterministic rule
/// for. `cluster_id` is stable across mining runs given the same category
/// and representative title, so re-running `mine` doesn't mint a new id for
/// the same real cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSeed {
    pub cluster_id: String,
    pub category: String,
    pub rule_id_or_aspect: String,
    pub member_fingerprints: Vec<String>,
    pub distinct_run_count: usize,
    pub representative_snippets: Vec<RepresentativeSnippet>,
}

fn cluster_id_for(category: &str, rule_id_or_aspect: &str, representative_title: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(category.as_bytes());
    hasher.update([0u8]);
    hasher.update(rule_id_or_aspect.as_bytes());
    hasher.update([0u8]);
    hasher.update(representative_title.to_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

struct Cluster {
    representative: MinedFindingRow,
    members: Vec<MinedFindingRow>,
}

/// Clusters mined agent findings into candidate seeds. Pure (no I/O, no
/// HistoryStore dependency) so the clustering algorithm is directly
/// testable against literal fixtures.
pub fn mine_candidates(findings: Vec<MinedFindingRow>) -> Vec<CandidateSeed> {
    let mut by_category: std::collections::HashMap<String, Vec<MinedFindingRow>> = std::collections::HashMap::new();
    for finding in findings {
        by_category.entry(finding.category.clone()).or_default().push(finding);
    }

    let mut seeds = Vec::new();
    for (category, group) in by_category {
        let mut clusters: Vec<Cluster> = Vec::new();
        for finding in group {
            let text = format!("{} {}", finding.title, finding.message);
            let existing = clusters.iter_mut().find(|c| {
                let rep_text = format!("{} {}", c.representative.title, c.representative.message);
                title_similarity(&text, &rep_text) >= SIMILARITY_THRESHOLD
            });
            match existing {
                Some(cluster) => cluster.members.push(finding),
                None => clusters.push(Cluster { representative: finding.clone(), members: vec![finding] }),
            }
        }

        for cluster in clusters {
            if cluster.members.len() < MIN_CLUSTER_MEMBERS {
                continue;
            }
            let distinct_runs: HashSet<&str> = cluster.members.iter().map(|m| m.run_id.as_str()).collect();
            if distinct_runs.len() < MIN_DISTINCT_RUNS {
                continue;
            }
            let cluster_id = cluster_id_for(&category, &cluster.representative.rule_id_or_aspect, &cluster.representative.title);
            let representative_snippets = cluster
                .members
                .iter()
                .take(MAX_REPRESENTATIVE_SNIPPETS)
                .map(|m| RepresentativeSnippet { fingerprint: m.fingerprint.clone(), title: m.title.clone(), message: m.message.clone() })
                .collect();
            seeds.push(CandidateSeed {
                cluster_id,
                category: category.clone(),
                rule_id_or_aspect: cluster.representative.rule_id_or_aspect.clone(),
                member_fingerprints: cluster.members.iter().map(|m| m.fingerprint.clone()).collect(),
                distinct_run_count: distinct_runs.len(),
                representative_snippets,
            });
        }
    }

    seeds
}

/// Writes a candidate seed to `.autoreview/rules/candidates/<clusterId>/seed.json`,
/// per the plan's file layout. Idempotent — overwrites on re-mine, since the
/// seed is a derived artifact, not something a human edits directly.
pub fn write_seed_file(repo_root: &std::path::Path, seed: &CandidateSeed) -> anyhow::Result<std::path::PathBuf> {
    let dir = repo_root.join(".autoreview").join("rules").join("candidates").join(&seed.cluster_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("seed.json");
    let json = serde_json::to_string_pretty(&SeedFile {
        cluster_id: seed.cluster_id.clone(),
        category: seed.category.clone(),
        rule_id_or_aspect: seed.rule_id_or_aspect.clone(),
        member_fingerprints: seed.member_fingerprints.clone(),
        distinct_run_count: seed.distinct_run_count,
        representative_snippets: seed.representative_snippets.iter().map(|s| SeedSnippet { fingerprint: s.fingerprint.clone(), title: s.title.clone(), message: s.message.clone() }).collect(),
    })?;
    std::fs::write(&path, json)?;
    Ok(path)
}

#[derive(serde::Serialize)]
struct SeedFile {
    cluster_id: String,
    category: String,
    rule_id_or_aspect: String,
    member_fingerprints: Vec<String>,
    distinct_run_count: usize,
    representative_snippets: Vec<SeedSnippet>,
}

#[derive(serde::Serialize)]
struct SeedSnippet {
    fingerprint: String,
    title: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fingerprint: &str, category: &str, title: &str, message: &str, run_id: &str) -> MinedFindingRow {
        MinedFindingRow { fingerprint: fingerprint.to_string(), category: category.to_string(), rule_id_or_aspect: "correctness-specialist".to_string(), title: title.to_string(), message: message.to_string(), run_id: run_id.to_string() }
    }

    #[test]
    fn clusters_similar_recurring_findings_across_distinct_runs() {
        let findings = vec![
            row("a", "correctness", "Missing null check", "Parameter x is not null-checked before use", "run-1"),
            row("b", "correctness", "Missing null check", "Parameter y is not null-checked before use", "run-2"),
            row("c", "correctness", "Missing null-check", "Parameter z lacks a null check before use", "run-3"),
        ];
        let seeds = mine_candidates(findings);
        assert_eq!(seeds.len(), 1, "got: {seeds:#?}");
        assert_eq!(seeds[0].category, "correctness");
        assert_eq!(seeds[0].member_fingerprints.len(), 3);
        assert_eq!(seeds[0].distinct_run_count, 3);
    }

    #[test]
    fn does_not_form_a_seed_below_the_minimum_member_count() {
        let findings = vec![
            row("a", "correctness", "Missing null check", "Parameter x is not null-checked before use", "run-1"),
            row("b", "correctness", "Missing null check", "Parameter y is not null-checked before use", "run-2"),
        ];
        assert!(mine_candidates(findings).is_empty());
    }

    #[test]
    fn does_not_form_a_seed_when_all_members_share_one_run() {
        let findings = vec![
            row("a", "correctness", "Missing null check", "Parameter x is not null-checked before use", "run-1"),
            row("b", "correctness", "Missing null check", "Parameter y is not null-checked before use", "run-1"),
            row("c", "correctness", "Missing null check", "Parameter z is not null-checked before use", "run-1"),
        ];
        assert!(mine_candidates(findings).is_empty(), "3 repeats in one run isn't recurring across time, just one big diff");
    }

    #[test]
    fn does_not_cluster_lexically_dissimilar_findings_together() {
        let findings = vec![
            row("a", "correctness", "Missing null check", "Parameter x is not null-checked before use", "run-1"),
            row("b", "correctness", "SQL injection risk", "User input flows into a raw query string", "run-2"),
            row("c", "correctness", "Hardcoded credential", "A password literal is assigned to a variable", "run-3"),
        ];
        assert!(mine_candidates(findings).is_empty());
    }

    #[test]
    fn does_not_cluster_across_categories_even_with_similar_text() {
        let findings = vec![
            row("a", "correctness", "Missing null check", "Parameter x is not null-checked before use", "run-1"),
            row("b", "design", "Missing null check", "Parameter y is not null-checked before use", "run-2"),
            row("c", "correctness", "Missing null check", "Parameter z is not null-checked before use", "run-3"),
        ];
        // Only 2 members land in "correctness" — below the minimum, so no seed forms.
        assert!(mine_candidates(findings).is_empty());
    }

    #[test]
    fn cluster_id_is_stable_for_the_same_category_and_representative_title() {
        let findings_a = vec![
            row("a", "correctness", "Missing null check", "m1", "run-1"),
            row("b", "correctness", "Missing null check", "m2", "run-2"),
            row("c", "correctness", "Missing null check", "m3", "run-3"),
        ];
        let findings_b = findings_a.clone();
        let seeds_a = mine_candidates(findings_a);
        let seeds_b = mine_candidates(findings_b);
        assert_eq!(seeds_a[0].cluster_id, seeds_b[0].cluster_id);
    }

    #[test]
    fn write_seed_file_writes_valid_json_to_the_expected_path() {
        let dir = tempfile::tempdir().unwrap();
        let seed = CandidateSeed {
            cluster_id: "abc123".to_string(),
            category: "correctness".to_string(),
            rule_id_or_aspect: "correctness-specialist".to_string(),
            member_fingerprints: vec!["a".to_string(), "b".to_string()],
            distinct_run_count: 2,
            representative_snippets: vec![RepresentativeSnippet { fingerprint: "a".to_string(), title: "t".to_string(), message: "m".to_string() }],
        };
        let path = write_seed_file(dir.path(), &seed).unwrap();
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["clusterId"].as_str(), None); // fields are snake_case here (internal artifact, not wire schema)
        assert_eq!(parsed["cluster_id"].as_str(), Some("abc123"));
    }
}
