//! LLM-assisted code-pattern extraction — a ninth (and final, for this
//! pass) mining source, and a genuinely different one: every prior
//! source clusters things a human already flagged (findings, PR
//! comments) or counts things mechanically (bug-fix commits,
//! suppressions, call-pairs). This one asks an agent to *read* sampled
//! code and propose conventions worth checking — the value-add an LLM
//! brings that `mine_from_code.rs`'s own mechanical scan structurally
//! can't: noticing a convention from context/naming/intent, not just
//! counting co-occurring identifiers.
//!
//! Scoped deliberately to the *same shape* `mine_from_code.rs` already
//! knows how to verify — call-pair "must-follow" conventions — rather
//! than an open-ended "propose any convention in English" extractor,
//! which would need a new, much harder verification modality to match.
//! The LLM's job here is narrower and more tractable: read real code,
//! propose `(call_a, call_b)` pairs worth checking, with a rationale.
//!
//! **The mandatory anti-hallucination gate**: every proposal is
//! mechanically re-verified via `mine_from_code::consistency_for_pair`
//! against the *entire real repo* before it's allowed anywhere near a
//! `CandidateSeed` — not a courtesy check, the reason this source is
//! trustworthy at all despite its input (an LLM's read of a handful of
//! sampled files) being far weaker evidence than every other source's
//! (a real recurring finding, a real recurring PR comment, a real
//! recurring diff). `verify_proposed_conventions` has no parameter to
//! skip this. A convention that doesn't hold up repo-wide is dropped
//! silently — the same fate a sub-threshold cluster meets in
//! `mine::mine_candidates` today.
//!
//! A verified convention maps straight to a `CandidateSeed`, bypassing
//! `mine_candidates`'s clustering — there's nothing to cluster, one
//! verified convention already *is* one candidate. This deliberately
//! repurposes `distinct_run_count`/`member_fingerprints`: neither field
//! has an obvious meaning for "one repo-wide consistency ratio," so
//! `distinct_run_count` holds `occurrences_of_a` (how many real call
//! sites back this) and `member_fingerprints` holds a single synthetic
//! fingerprint — the CLI's own "N member(s) across M run(s)" progress
//! line ends up reading as "1 member(s) across <real evidence count>
//! run(s))," a known, accepted quirk of the reuse, not a bug.
//!
//! **Privacy note, stated plainly**: unlike every other source (small
//! free-text titles/messages), this one sends whole sampled file
//! *contents* to the configured `AgentBackend` — a materially larger
//! trust boundary than anything else in this pipeline. Defaults to
//! `enabled: false` for exactly this reason.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::claude_code::{AgentBackend, InvokeRequest};
use crate::agents::contract::extract_last_fenced_block;
use crate::rule_factory::mine::{CandidateSeed, RepresentativeSnippet};
use crate::rule_factory::mine_from_code::{consistency_for_pair, source_files, CallPairConvention, SOURCE_EXTENSIONS};

/// Bounds how much of one sampled file's content reaches the prompt —
/// keeps token cost bounded regardless of how large the repo's biggest
/// files happen to be; a convention worth proposing should be visible
/// well within this window.
const MAX_SAMPLE_FILE_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq)]
pub struct LlmProposedConvention {
    pub call_a: String,
    pub call_b: String,
    pub rationale: String,
}

/// Ranks packages by `ImportGraph::fan_in` (Go if `go.mod` exists, else
/// Java/Kotlin) and takes each top package's largest source file — the
/// intuition being that heavily-imported packages are more likely to be
/// where a repo's own real conventions get *established*, not just
/// followed. Falls back to the repo's largest files overall (by byte
/// size) for anything archgraph doesn't build a graph for at all (a
/// pure JS/TS repo, or one with neither `go.mod` nor any Java/Kotlin
/// source) — `symindex` has no file-level "hub" ranking to reach for
/// instead, so this is the honest fallback, not a placeholder for one.
fn select_representative_files(repo_root: &Path, max_files: usize) -> Vec<PathBuf> {
    if let Some(module_path) = autoreview_archgraph::discover_go_module_path(repo_root) {
        let graph = autoreview_archgraph::build_go_import_graph(repo_root, &module_path);
        if !graph.edges.is_empty() {
            let files = top_files_by_package_fan_in(repo_root, &graph, &["go"], max_files, |rel| autoreview_archgraph::go_package_for_file(rel, &module_path));
            if !files.is_empty() {
                return files;
            }
        }
    }

    let graph = autoreview_archgraph::build_java_kotlin_import_graph(repo_root);
    if !graph.edges.is_empty() {
        let files = top_files_by_package_fan_in(repo_root, &graph, &["java", "kt", "kts"], max_files, |rel| autoreview_archgraph::java_kotlin_package_for_file(repo_root, rel));
        if !files.is_empty() {
            return files;
        }
    }

    largest_files_fallback(repo_root, max_files)
}

fn top_files_by_package_fan_in(repo_root: &Path, graph: &autoreview_archgraph::ImportGraph, extensions: &[&str], max_files: usize, package_for_file: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut largest_file_for_package: std::collections::HashMap<String, (PathBuf, u64)> = std::collections::HashMap::new();
    for path in source_files(repo_root, extensions) {
        let Ok(rel) = path.strip_prefix(repo_root) else { continue };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let Some(package) = package_for_file(&rel_str) else { continue };
        let Ok(metadata) = std::fs::metadata(&path) else { continue };
        let size = metadata.len();
        largest_file_for_package.entry(package).and_modify(|(cur_path, cur_size)| {
            if size > *cur_size {
                *cur_path = path.clone();
                *cur_size = size;
            }
        }).or_insert((path, size));
    }

    let mut ranked: Vec<(String, PathBuf)> = largest_file_for_package.into_iter().map(|(package, (path, _))| (package, path)).collect();
    // `Reverse` rather than a flipped comparator: clippy's
    // `unnecessary_sort_by` rejects the latter, and both sorts are stable
    // so packages with equal fan-in keep their existing relative order.
    ranked.sort_by_key(|(package, _)| std::cmp::Reverse(graph.fan_in(package)));
    ranked.into_iter().take(max_files).map(|(_, path)| path).collect()
}

fn largest_files_fallback(repo_root: &Path, max_files: usize) -> Vec<PathBuf> {
    let mut files: Vec<(PathBuf, u64)> = source_files(repo_root, SOURCE_EXTENSIONS).into_iter().filter_map(|path| std::fs::metadata(&path).ok().map(|m| (path, m.len()))).collect();
    files.sort_by(|(_, a), (_, b)| b.cmp(a));
    files.into_iter().take(max_files).map(|(path, _)| path).collect()
}

fn build_propose_prompt(samples: &[(String, String)]) -> String {
    let joined: String = samples.iter().map(|(rel_path, content)| format!("--- {rel_path} ---\n{content}\n")).collect::<Vec<_>>().join("\n");
    format!(
        "You are looking for \"must-call-pair\" conventions in a codebase — cases where a call to method A is almost always followed nearby by a call to method B (e.g. a Lock() paired with an Unlock(), a Query(...) paired with a .Close() on its result). \
        Read the sampled files below and propose any such conventions you notice, based on the method names and context (not just brute counting — that mechanical check happens separately afterward). \
        Only propose pairs you have real reason to believe are a genuine, repo-wide convention worth enforcing, not a one-off. \
        Respond with ONLY a fenced ```json block containing a JSON array, each element shaped {{\"call_a\": \"MethodName\", \"call_b\": \"OtherMethodName\", \"rationale\": \"one sentence\"}}. An empty array [] is a completely valid answer if you don't see a real convention.\n\n{joined}"
    )
}

#[derive(Debug, Deserialize)]
struct RawProposal {
    call_a: String,
    call_b: String,
    #[serde(default)]
    rationale: String,
}

fn parse_llm_proposals(block: &str) -> anyhow::Result<Vec<LlmProposedConvention>> {
    let raw: Vec<RawProposal> = serde_json::from_str(block.trim())?;
    Ok(raw.into_iter().map(|r| LlmProposedConvention { call_a: r.call_a, call_b: r.call_b, rationale: r.rationale }).collect())
}

/// Samples representative files, asks `backend` once to propose
/// call-pair conventions from reading them. A failed invocation or a
/// response with no parseable fenced JSON block yields an empty
/// proposal list (fail-soft, matching `draft.rs`'s own "a broken
/// invocation isn't a considered opinion" posture) rather than an error
/// — an LLM declining to see a convention is a completely valid, common
/// outcome, not a failure.
pub fn propose_conventions_via_llm(backend: &dyn AgentBackend, repo_root: &Path, model: &str, max_turns: u32, max_sample_files: usize) -> Vec<LlmProposedConvention> {
    let files = select_representative_files(repo_root, max_sample_files);
    if files.is_empty() {
        return Vec::new();
    }
    let samples: Vec<(String, String)> = files
        .iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(repo_root).unwrap_or(path).to_string_lossy().replace('\\', "/");
            std::fs::read_to_string(path).ok().map(|content| (rel, content.chars().take(MAX_SAMPLE_FILE_CHARS).collect()))
        })
        .collect();
    if samples.is_empty() {
        return Vec::new();
    }

    let request = InvokeRequest {
        prompt: build_propose_prompt(&samples),
        system_prompt: "You are a careful static-analysis assistant. You only propose a convention when the sampled code gives you real reason to believe it, and you say so plainly with an empty array when you don't see one.".to_string(),
        allowed_tools: vec![],
        max_turns,
        model: model.to_string(),
        cwd: repo_root.to_path_buf(),
    };
    let Ok(result) = backend.invoke(&request) else { return Vec::new() };
    let Some(block) = extract_last_fenced_block(&result.final_text) else { return Vec::new() };
    parse_llm_proposals(block).unwrap_or_default()
}

/// The mandatory anti-hallucination gate — see the module doc. Every
/// proposal is checked against the whole real repo via
/// `consistency_for_pair`; only what clears `min_occurrences`/
/// `min_consistency` survives. No way to bypass this check exists in
/// this module's own API on purpose.
pub fn verify_proposed_conventions(repo_root: &Path, proposals: &[LlmProposedConvention], min_occurrences: usize, min_consistency: f64) -> Vec<CallPairConvention> {
    proposals
        .iter()
        .filter_map(|p| {
            let convention = consistency_for_pair(repo_root, &p.call_a, &p.call_b, min_occurrences)?;
            (convention.consistency >= min_consistency).then_some(convention)
        })
        .collect()
}

fn verified_convention_to_seed(convention: &CallPairConvention) -> CandidateSeed {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(convention.call_a.as_bytes());
    hasher.update([0u8]);
    hasher.update(convention.call_b.as_bytes());
    let cluster_id = format!("{:x}", hasher.finalize())[..16].to_string();
    let fingerprint = format!("llm-pair-{}-{}", convention.call_a, convention.call_b);

    CandidateSeed {
        cluster_id,
        category: "correctness".to_string(),
        rule_id_or_aspect: "llm-proposed-call-pair".to_string(),
        member_fingerprints: vec![fingerprint.clone()],
        // Repurposed to hold the real evidence count backing this
        // convention (how many real `call_a` sites were checked), not a
        // count of distinct mining runs — see the module doc.
        distinct_run_count: convention.occurrences_of_a,
        representative_snippets: vec![RepresentativeSnippet {
            fingerprint,
            title: format!("`{}` should be paired with `{}`", convention.call_a, convention.call_b),
            message: format!(
                "Across this repo, {} of {} calls to `{}` are followed nearby by a call to `{}` ({:.0}% consistency), e.g. {}.",
                convention.co_occurrences,
                convention.occurrences_of_a,
                convention.call_a,
                convention.call_b,
                convention.consistency * 100.0,
                convention.example_location
            ),
        }],
    }
}

/// The full pipeline: propose, then mandatorily verify, then map
/// surviving conventions to `CandidateSeed`s ready for `draft_candidate`.
/// Returns `(seeds, proposed_count)` so a caller can report "N proposed,
/// M survived" — never drop the proposed count silently, per this
/// session's own "no silent caps" discipline.
pub fn mine_from_llm_patterns(backend: &dyn AgentBackend, repo_root: &Path, model: &str, max_turns: u32, max_sample_files: usize, min_occurrences: usize, min_consistency: f64) -> (Vec<CandidateSeed>, usize) {
    let proposals = propose_conventions_via_llm(backend, repo_root, model, max_turns, max_sample_files);
    let proposed_count = proposals.len();
    let verified = verify_proposed_conventions(repo_root, &proposals, min_occurrences, min_consistency);
    let seeds = verified.iter().map(verified_convention_to_seed).collect();
    (seeds, proposed_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::{InvokeResult, Usage};
    use std::cell::RefCell;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    struct ScriptedBackend {
        response: RefCell<Option<&'static str>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
            let text = self.response.borrow_mut().take().ok_or_else(|| anyhow::anyhow!("no scripted response left"))?;
            Ok(InvokeResult { final_text: text.to_string(), usage: Usage::default(), wall_ms: 1 })
        }
    }

    #[test]
    fn parses_a_real_shaped_llm_proposal_block() {
        let block = r#"[{"call_a": "Lock", "call_b": "Unlock", "rationale": "mutex critical section"}]"#;
        let proposals = parse_llm_proposals(block).unwrap();
        assert_eq!(proposals, vec![LlmProposedConvention { call_a: "Lock".to_string(), call_b: "Unlock".to_string(), rationale: "mutex critical section".to_string() }]);
    }

    #[test]
    fn parses_an_empty_proposal_array() {
        assert!(parse_llm_proposals("[]").unwrap().is_empty());
    }

    #[test]
    fn errors_clearly_on_malformed_json() {
        assert!(parse_llm_proposals("not json").is_err());
    }

    #[test]
    fn propose_conventions_extracts_a_fenced_block_from_a_real_backend_response() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("main.go"), "package main\n\nfunc f() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        let backend = ScriptedBackend { response: RefCell::new(Some("```json\n[{\"call_a\": \"Lock\", \"call_b\": \"Unlock\", \"rationale\": \"mutex\"}]\n```")) };
        let proposals = propose_conventions_via_llm(&backend, dir.path(), "cheap-model", 2, 8);
        assert_eq!(proposals, vec![LlmProposedConvention { call_a: "Lock".to_string(), call_b: "Unlock".to_string(), rationale: "mutex".to_string() }]);
    }

    #[test]
    fn propose_conventions_fails_soft_on_a_broken_invocation() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("main.go"), "package main\n\nfunc f() {}\n");
        let backend = ScriptedBackend { response: RefCell::new(None) };
        assert!(propose_conventions_via_llm(&backend, dir.path(), "cheap-model", 2, 8).is_empty());
    }

    #[test]
    fn verify_proposed_conventions_drops_a_fabricated_pair_and_keeps_a_real_one() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(&dir.path().join(format!("f{i}.go")), &format!("package main\n\nfunc f{i}() {{\n\tmu.Lock()\n\tdoWork()\n\tmu.Unlock()\n}}\n"));
        }
        let proposals = vec![
            LlmProposedConvention { call_a: "Lock".to_string(), call_b: "Unlock".to_string(), rationale: "real".to_string() },
            LlmProposedConvention { call_a: "Lock".to_string(), call_b: "TotallyFabricated".to_string(), rationale: "hallucinated".to_string() },
        ];
        let verified = verify_proposed_conventions(dir.path(), &proposals, 3, 0.9);
        assert_eq!(verified.len(), 1, "got: {verified:#?}");
        assert_eq!(verified[0].call_b, "Unlock");
    }

    #[test]
    fn mine_from_llm_patterns_reports_both_proposed_and_survived_counts() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(&dir.path().join(format!("f{i}.go")), &format!("package main\n\nfunc f{i}() {{\n\tmu.Lock()\n\tdoWork()\n\tmu.Unlock()\n}}\n"));
        }
        let backend = ScriptedBackend {
            response: RefCell::new(Some(
                "```json\n[{\"call_a\": \"Lock\", \"call_b\": \"Unlock\", \"rationale\": \"real\"}, {\"call_a\": \"Lock\", \"call_b\": \"Fabricated\", \"rationale\": \"hallucinated\"}]\n```",
            )),
        };
        let (seeds, proposed_count) = mine_from_llm_patterns(&backend, dir.path(), "cheap-model", 2, 8, 3, 0.9);
        assert_eq!(proposed_count, 2);
        assert_eq!(seeds.len(), 1, "got: {seeds:#?}");
        assert_eq!(seeds[0].rule_id_or_aspect, "llm-proposed-call-pair");
        assert_eq!(seeds[0].distinct_run_count, 5);
    }

    #[test]
    fn select_representative_files_ranks_a_real_go_repo_by_package_fan_in() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module example.com/app\n\ngo 1.22\n");
        // hub is imported by both a and b; leaf imports nothing.
        write(&dir.path().join("hub/hub.go"), "package hub\n\nfunc Do() {}\n");
        write(&dir.path().join("a/a.go"), "package a\n\nimport \"example.com/app/hub\"\n\nfunc F() { hub.Do() }\n");
        write(&dir.path().join("b/b.go"), "package b\n\nimport \"example.com/app/hub\"\n\nfunc F() { hub.Do() }\n");
        write(&dir.path().join("leaf/leaf.go"), "package leaf\n\nfunc F() {}\n");

        let files = select_representative_files(dir.path(), 1);
        assert_eq!(files, vec![dir.path().join("hub/hub.go")], "got: {files:#?}");
    }
}
