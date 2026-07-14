//! Mines channel 2 (repeated `--fp` feedback with a human note) into
//! negative-guidance proposal seeds, then drafts one candidate instruction
//! line per seed via the configured agent backend — single-shot, not the
//! rule factory's 5x ensemble, since a prose instruction edit is lower risk
//! than a deterministic pattern and always goes through the human
//! `skills review` gate (still a stub) before it can touch a live skill.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::agents::claude_code::{AgentBackend, InvokeRequest, Usage};
use crate::agents::contract::extract_last_fenced_block;
use crate::report::dedupe::title_similarity;
use crate::storage::history_store::FpFeedbackRow;

const SIMILARITY_THRESHOLD: f64 = 0.55;
/// The plan's own threshold for this channel: "if >= 3 similar FPs...".
const MIN_CLUSTER_MEMBERS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegativeGuidanceSeed {
    pub cluster_id: String,
    pub category: String,
    pub representative_title: String,
    pub representative_message: String,
    pub notes: Vec<String>,
}

fn cluster_id_for(category: &str, representative_title: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(category.as_bytes());
    hasher.update([0u8]);
    hasher.update(representative_title.to_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

struct Cluster {
    representative: FpFeedbackRow,
    members: Vec<FpFeedbackRow>,
}

/// Clusters `--fp`-with-note feedback rows by category, using the same
/// trigram-shingle title/message similarity as rule mining (kept lexical
/// so a human reviewing a proposal can see why its members were grouped).
/// Pure — no I/O, directly testable.
pub fn mine_negative_guidance(rows: Vec<FpFeedbackRow>) -> Vec<NegativeGuidanceSeed> {
    let mut by_category: std::collections::HashMap<String, Vec<FpFeedbackRow>> = std::collections::HashMap::new();
    for row in rows {
        by_category.entry(row.category.clone()).or_default().push(row);
    }

    let mut seeds = Vec::new();
    for (category, group) in by_category {
        let mut clusters: Vec<Cluster> = Vec::new();
        for row in group {
            let text = format!("{} {}", row.title, row.message);
            let existing = clusters.iter_mut().find(|c| {
                let rep_text = format!("{} {}", c.representative.title, c.representative.message);
                title_similarity(&text, &rep_text) >= SIMILARITY_THRESHOLD
            });
            match existing {
                Some(cluster) => cluster.members.push(row),
                None => clusters.push(Cluster { representative: row.clone(), members: vec![row] }),
            }
        }

        for cluster in clusters {
            if cluster.members.len() < MIN_CLUSTER_MEMBERS {
                continue;
            }
            let cluster_id = cluster_id_for(&category, &cluster.representative.title);
            seeds.push(NegativeGuidanceSeed {
                cluster_id,
                category: category.clone(),
                representative_title: cluster.representative.title.clone(),
                representative_message: cluster.representative.message.clone(),
                notes: cluster.members.iter().map(|m| m.note.clone()).collect(),
            });
        }
    }

    seeds
}

fn build_prompt(seed: &NegativeGuidanceSeed) -> String {
    let notes: String = seed.notes.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n");
    format!(
        "A code-review skill for category '{}' keeps getting reported as a false positive on a recurring pattern. \
        Example finding: title \"{}\", message \"{}\". \
        Reviewers' own stated reasons across {} reports:\n{}\n\n\
        Draft exactly ONE imperative instruction line to append to this skill's instructions, in the form \"Do not flag X because Y.\" \
        Base it on the common thread across the reviewers' reasons above, not just the first one. \
        Respond with ONLY a fenced ```text block containing that single line.",
        seed.category, seed.representative_title, seed.representative_message, seed.notes.len(), notes
    )
}

/// Drafts one negative-guidance instruction line for a seed. Returns
/// `None` (not an error) if the backend fails to invoke or the response
/// doesn't contain a fenced block — fail-soft, since a broken draft attempt
/// shouldn't crash `skills mine` for every other seed.
pub fn draft_negative_guidance(backend: &dyn AgentBackend, seed: &NegativeGuidanceSeed, model: &str, max_turns: u32, cwd: &Path) -> (Option<String>, Usage) {
    let request = InvokeRequest {
        prompt: build_prompt(seed),
        system_prompt: "You are a precise editor of code-review skill instructions. You write exactly one clear, specific line, not a paragraph.".to_string(),
        allowed_tools: vec![],
        max_turns,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
    };
    match backend.invoke(&request) {
        Ok(result) => {
            let line = extract_last_fenced_block(&result.final_text).map(|b| b.trim().to_string());
            (line, result.usage)
        }
        Err(_) => (None, Usage::default()),
    }
}

/// Writes a negative-guidance proposal to
/// `.autoreview/skills/<category>/proposals/<clusterId>.md` — a simplified
/// stand-in for the plan's `proposals/<proposalId>/diff.patch` shape (no
/// diff-patch tooling exists for skill instructions yet): a human-readable
/// document with the drafted line plus its supporting evidence, for
/// `skills review` (still a stub) to eventually surface properly.
pub fn write_proposal_file(repo_root: &Path, seed: &NegativeGuidanceSeed, drafted_line: Option<&str>) -> anyhow::Result<std::path::PathBuf> {
    let dir = repo_root.join(".autoreview").join("skills").join(&seed.category).join("proposals");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", seed.cluster_id));

    let notes: String = seed.notes.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n");
    let body = format!(
        "# Negative-guidance proposal {}\n\ncategory: {}\nsource: fp-feedback\nmembers: {}\n\n## Representative finding\n\ntitle: {}\nmessage: {}\n\n## Reviewer notes\n\n{}\n\n## Drafted instruction line\n\n{}\n",
        seed.cluster_id,
        seed.category,
        seed.notes.len(),
        seed.representative_title,
        seed.representative_message,
        notes,
        drafted_line.unwrap_or("(drafting skipped or failed — no agent backend available)")
    );
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(category: &str, title: &str, message: &str, note: &str) -> FpFeedbackRow {
        FpFeedbackRow { category: category.to_string(), rule_id_or_aspect: "correctness-specialist".to_string(), title: title.to_string(), message: message.to_string(), note: note.to_string() }
    }

    #[test]
    fn clusters_recurring_fp_feedback_by_category_and_similarity() {
        let rows = vec![
            row("correctness", "Missing null check", "Parameter x is not null-checked before use", "validation happens at the boundary"),
            row("correctness", "Missing null check", "Parameter y is not null-checked before use", "we always validate upstream"),
            row("correctness", "Missing null-check", "Parameter z lacks a null check before use", "boundary validation covers this"),
        ];
        let seeds = mine_negative_guidance(rows);
        assert_eq!(seeds.len(), 1, "got: {seeds:#?}");
        assert_eq!(seeds[0].notes.len(), 3);
    }

    #[test]
    fn does_not_form_a_seed_below_the_minimum_member_count() {
        let rows = vec![row("correctness", "Missing null check", "m1", "n1"), row("correctness", "Missing null check", "m2", "n2")];
        assert!(mine_negative_guidance(rows).is_empty());
    }

    #[test]
    fn does_not_cluster_dissimilar_findings_together() {
        let rows = vec![
            row("correctness", "Missing null check", "Parameter x is not null-checked", "n1"),
            row("correctness", "SQL injection risk", "raw query built from input", "n2"),
            row("correctness", "Hardcoded credential", "password literal assigned", "n3"),
        ];
        assert!(mine_negative_guidance(rows).is_empty());
    }

    #[test]
    fn write_proposal_file_writes_a_readable_markdown_document() {
        let dir = tempfile::tempdir().unwrap();
        let seed = NegativeGuidanceSeed {
            cluster_id: "abc123".to_string(),
            category: "correctness".to_string(),
            representative_title: "Missing null check".to_string(),
            representative_message: "Parameter x is not null-checked".to_string(),
            notes: vec!["validation happens at the boundary".to_string()],
        };
        let path = write_proposal_file(dir.path(), &seed, Some("Do not flag missing null checks on validated boundary parameters.")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Do not flag missing null checks"));
        assert!(contents.contains("validation happens at the boundary"));
    }
}
