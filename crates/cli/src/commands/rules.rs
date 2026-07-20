//! `autoreview rules mine`/`rules mine --from-comments`/`rules bench`/
//! `rules shadow-log`/`rules review` — the first five real (non-stub)
//! pieces of the M3 rule factory: clusters recorded agent findings (or,
//! opt-in, recurring human PR review comments — see
//! `rule_factory::mine_from_comments`) into candidate seeds
//! (`.autoreview/rules/candidates/<clusterId>/seed.json`), attempts to
//! draft an ast-grep rule for each seed via the configured agent backend,
//! 5x ensemble-agreement filtered (`rule_factory::draft`), benches a
//! drafted rule against human-supplied fixtures plus a current-repo FP
//! smoke test (`rule_factory::bench`), lists recent firings of a
//! shadow/promoted rule for spot-checking (`rule_factory::shadow`, wired
//! into every `diff` run — see `commands::diff`), and gives a human the
//! actual `--approve`/`--reject` gate the plan calls for before a candidate
//! ever reaches shadow mode, and `rules rollback` for the manual override
//! sitting alongside the automatic promote/demote gate `diff.rs` runs.
//! `rules packs` lists registered external rule packs
//! (`.autoreview/rulepacks.yaml`) — mirrors `skills list`'s "list
//! registered things" precedent. `rules mine --from-code` is a third
//! mining source (see `run_rules_mine_code`'s own docs) — a discovery
//! prototype, not yet integrated into the draft/bench/shadow pipeline.

use autoreview_core::{draft_candidate, mine_candidates, mine_from_pr_comments, run_bench, write_seed_file, BenchVerdict, CandidateSeed, DraftOutcome, HistoryStore};
use autoreview_schema::AgentBackendKind;
use serde::Deserialize;

use super::backend::{backend_available, build_backend};
use super::diff::move_shadow_rule_file;
use super::history::history_dir_for;

fn cheap_model_for(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => &config.budgets.models.cheap,
    }
}

/// Shared by both mining sources: writes each seed's file, attempts a
/// draft against the configured backend (skipped, not failed, when none is
/// available), and prints progress. `repo_root`/`config` are threaded
/// through rather than re-derived, since the comments-mining caller has
/// already loaded `config` for its own opt-in check.
fn draft_and_write_seeds(repo_root: &std::path::Path, config: &autoreview_schema::AutoreviewConfig, seeds: &[CandidateSeed]) -> anyhow::Result<()> {
    let backend_kind = config.agents.backend;
    let can_draft = backend_available(backend_kind, config);
    let backend = can_draft.then(|| build_backend(backend_kind, config));
    let model = cheap_model_for(backend_kind, config).to_string();

    println!("Found {} candidate cluster(s):", seeds.len());
    for seed in seeds {
        let seed_path = write_seed_file(repo_root, seed)?;
        println!(
            "  {} ({}, {} member(s) across {} run(s)) -> {}",
            seed.cluster_id,
            seed.category,
            seed.member_fingerprints.len(),
            seed.distinct_run_count,
            seed_path.display()
        );

        let Some(backend) = &backend else {
            println!("    [draft] skipped — no agent backend available (needed to attempt a rule draft)");
            continue;
        };
        let (outcome, _usage) = draft_candidate(backend.as_ref(), seed, &model, 2, repo_root);
        match outcome {
            DraftOutcome::Drafted { rule_yaml, agreement_count } => {
                let dir = repo_root.join(".autoreview").join("rules").join("candidates").join(&seed.cluster_id);
                std::fs::create_dir_all(&dir)?;
                let rule_path = dir.join("rule.yaml");
                std::fs::write(&rule_path, &rule_yaml)?;
                println!("    [draft] {agreement_count}/5 attempts agreed -> {}", rule_path.display());
            }
            DraftOutcome::Inexpressible { rationale } => {
                println!("    [draft] inexpressible as a syntactic rule: {rationale}");
            }
        }
    }
    println!("\n(Review/shadow/promote are not yet implemented. Run `autoreview rules bench <clusterId>` on a drafted candidate once you've added tests/positive and tests/negative fixtures under its candidate directory.)");
    Ok(())
}

pub fn run_rules_mine(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let findings = store.agent_findings_for_mining()?;

    if findings.is_empty() {
        println!("No agent findings recorded yet on this machine — nothing to mine. Run `autoreview diff` a few times first.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar findings spanning >= 2 distinct runs).");
        return Ok(());
    }

    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    draft_and_write_seeds(repo_root, &config, &seeds)
}

/// `autoreview rules mine --from-comments` — mines recurring human PR
/// review comments instead of autoreview's own past agent findings. Opt-in
/// (`mineFromComments.enabled: true` in `.autoreview/config.yaml`) since it
/// shells out to `gh api` against the real GitHub API for the configured
/// repo, unlike the default `agent_findings_for_mining` source which only
/// ever reads this machine's local history store.
pub fn run_rules_mine_comments(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    if !config.mine_from_comments.enabled {
        println!("mineFromComments.enabled is false in .autoreview/config.yaml — nothing to do. Set it to true to mine recurring PR review comments via `gh`.");
        return Ok(());
    }

    println!("Fetching the {} most recently merged PRs' review comments via `gh`...", config.mine_from_comments.lookback_prs);
    let findings = mine_from_pr_comments(&config.mine_from_comments.gh_binary, config.mine_from_comments.lookback_prs)?;
    if findings.is_empty() {
        println!("No substantive review comments found on recent merged PRs — nothing to mine.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar comments spanning >= 2 distinct PRs).");
        return Ok(());
    }

    draft_and_write_seeds(repo_root, &config, &seeds)
}

const CODE_MINE_MIN_OCCURRENCES: usize = 5;
const CODE_MINE_MIN_CONSISTENCY: f64 = 0.9;

/// `autoreview rules mine --from-code` — a third mining source, and a
/// genuinely different kind of one: not clustering pre-existing labeled
/// findings/comments, but discovering call-pair usage conventions
/// (`autoreview_core::mine_call_pair_conventions`) directly from how
/// consistently the repo's own Go source already uses its APIs (e.g. a
/// `Lock()` almost always paired with an `Unlock()` nearby). Prints
/// discovered conventions for inspection — a discovery prototype, not yet
/// wired into the mine -> draft -> bench -> shadow pipeline the other two
/// sources feed (see the module's own doc comment for why: there's no
/// natural `CandidateSeed` mapping for "one repo-wide consistency ratio").
pub fn run_rules_mine_code(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let conventions = autoreview_core::mine_call_pair_conventions(repo_root, CODE_MINE_MIN_OCCURRENCES, CODE_MINE_MIN_CONSISTENCY);
    if conventions.is_empty() {
        println!(
            "No call-pair conventions found meeting the >={CODE_MINE_MIN_OCCURRENCES} occurrences / >={:.0}% consistency bar.",
            CODE_MINE_MIN_CONSISTENCY * 100.0
        );
        return Ok(());
    }

    println!("Found {} candidate call-pair convention(s) (prototype — inspect only, not yet fed into the rule-draft pipeline):\n", conventions.len());
    for c in &conventions {
        println!(
            "  .{}(...) -> .{}(...)  [{:.0}% of {} occurrence(s), e.g. {}]",
            c.call_a,
            c.call_b,
            c.consistency * 100.0,
            c.occurrences_of_a,
            c.example_location
        );
    }
    println!("\n(a call to the first method with no accompanying call to the second nearby is a plausible candidate for a future rule — inspect before drafting one)");
    Ok(())
}

pub fn run_rules_bench(repo_root: &std::path::Path, cluster_id: &str) -> anyhow::Result<()> {
    let report = run_bench(repo_root, cluster_id)?;

    if let Some(self_test) = &report.self_test {
        println!(
            "  self-test: {}/{} positive matched, {}/{} negative matched ({})",
            self_test.positive_matched,
            self_test.positive_total,
            self_test.negative_matched,
            self_test.negative_total,
            if self_test.passed() { "passed" } else { "failed" }
        );
    } else {
        println!("  self-test: skipped — no tests/positive or tests/negative fixtures supplied yet");
    }

    if let Some(fp_smoke) = &report.fp_smoke {
        println!(
            "  fp-smoke:  {}/{} sampled repo file(s) matched ({})",
            fp_smoke.matched_files,
            fp_smoke.sampled_files,
            if fp_smoke.passed() { "passed" } else { "failed" }
        );
    } else {
        println!("  fp-smoke:  skipped — no sample files of this rule's language found in the current repo");
    }

    println!("  historical-precision: skipped — {}", report.historical_precision_skipped_reason);

    match report.verdict {
        BenchVerdict::Candidate => println!("\nverdict: candidate — ready for `autoreview rules review` (still a stub)"),
        BenchVerdict::NeedsFixtures => println!("\nverdict: needs-fixtures — add tests/positive/*, tests/negative/* under this candidate's directory, then re-run bench"),
        BenchVerdict::SelfTestFailed => println!("\nverdict: self-test-failed — the drafted rule doesn't cleanly match its own fixtures yet"),
        BenchVerdict::FailedFpSmoke => println!("\nverdict: failed-fp-smoke — the rule matches too many unrelated files in this repo"),
    }
    Ok(())
}

const SHADOW_LOG_LIMIT: u32 = 20;

pub fn run_rules_shadow_log(repo_root: &std::path::Path, rule_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;

    let Some(state) = store.rule_state(rule_id)? else {
        println!("Rule '{rule_id}' has never fired in shadow/promoted mode on this machine — nothing to show yet.");
        return Ok(());
    };

    let distinct_runs = store.distinct_shadow_run_count(rule_id)?;
    let user_fp_count = store.count_fp_feedback_for_rule(rule_id)?;
    println!(
        "rule '{rule_id}': status={}, firings={}, distinct_runs={distinct_runs}, agent_agreed={}, agent_disagreed={}, user_fp_reports={user_fp_count}, tracked_since={}",
        state.status, state.firings, state.agent_agreed, state.agent_disagreed, state.valid_from
    );

    let firings = store.recent_shadow_firings(rule_id, SHADOW_LOG_LIMIT)?;
    if firings.is_empty() {
        println!("(no firings recorded)");
        return Ok(());
    }
    println!("\nrecent firings (most recent first):");
    for firing in &firings {
        println!("  [{}] {}:{} (run {}, {}) — {}", firing.agreement, firing.location_path, firing.location_line, firing.run_id, firing.created_at, firing.fingerprint);
    }
    println!("\n(use `autoreview feedback <id> --fp|--tp` on a finding's own id to feed the promotion/demotion gates — use --doesnt-apply instead of --fp if the rule is valid but just not relevant here, since that does NOT count toward demotion)");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RuleIdOnly {
    id: String,
}

fn candidates_dir(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".autoreview").join("rules").join("candidates")
}

fn list_candidate_ids(repo_root: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(candidates_dir(repo_root)) else { return Vec::new() };
    entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect()
}

/// `autoreview rules review` — the human-approval gate the plan calls for
/// between bench and shadow mode. No args: lists every candidate with a
/// drafted rule, its bench verdict, and whether it's already been
/// approved/rejected. `--approve <clusterId>`: moves the candidate's
/// rule.yaml into `.autoreview/rules/shadow/`, registers it in the history
/// store's shadow lifecycle (status "shadow", so it starts accumulating
/// firings/agreement from the next `diff` run), and marks the candidate
/// dir approved. `--reject <clusterId> --reason <text>`: records why,
/// without deleting the candidate (kept for provenance, per the plan).
pub fn run_rules_review(repo_root: &std::path::Path, approve: Option<String>, reject: Option<String>, reason: Option<String>) -> anyhow::Result<()> {
    if let Some(cluster_id) = approve {
        return approve_candidate(repo_root, &cluster_id);
    }
    if let Some(cluster_id) = reject {
        let Some(reason) = reason else {
            anyhow::bail!("--reject requires --reason \"<why>\"");
        };
        return reject_candidate(repo_root, &cluster_id, &reason);
    }

    let ids = list_candidate_ids(repo_root);
    if ids.is_empty() {
        println!("No candidates found — run `autoreview rules mine` first.");
        return Ok(());
    }

    println!("Candidate rules pending review:\n");
    for cluster_id in ids {
        let dir = candidates_dir(repo_root).join(&cluster_id);
        if !dir.join("rule.yaml").exists() {
            continue;
        }
        let status = if dir.join("approved.json").exists() {
            "approved".to_string()
        } else if dir.join("rejected.json").exists() {
            "rejected".to_string()
        } else {
            "pending".to_string()
        };
        let bench_summary = match run_bench(repo_root, &cluster_id) {
            Ok(report) => format!("{:?}", report.verdict),
            Err(err) => format!("bench error: {err}"),
        };
        println!("  {cluster_id}  status={status}  bench={bench_summary}");
    }
    println!("\n(use --approve <clusterId> to move a candidate to shadow mode, or --reject <clusterId> --reason \"<why>\")");
    Ok(())
}

fn approve_candidate(repo_root: &std::path::Path, cluster_id: &str) -> anyhow::Result<()> {
    let dir = candidates_dir(repo_root).join(cluster_id);
    let rule_path = dir.join("rule.yaml");
    let contents = std::fs::read_to_string(&rule_path).map_err(|_| anyhow::anyhow!("no drafted rule found at {} — run `autoreview rules mine` first", rule_path.display()))?;
    let meta: RuleIdOnly = serde_yaml::from_str(&contents)?;

    let shadow_dir = repo_root.join(".autoreview").join("rules").join("shadow");
    std::fs::create_dir_all(&shadow_dir)?;
    std::fs::write(shadow_dir.join(format!("{}.yaml", meta.id)), &contents)?;

    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    store.ensure_rule_tracked(&meta.id, "shadow", &chrono::Utc::now().to_rfc3339())?;

    std::fs::write(dir.join("approved.json"), serde_json::json!({ "ruleId": meta.id, "approvedAt": chrono::Utc::now().to_rfc3339() }).to_string())?;
    println!("Approved '{cluster_id}' — rule '{}' is now in shadow mode (see `autoreview rules shadow-log {}`).", meta.id, meta.id);
    Ok(())
}

fn reject_candidate(repo_root: &std::path::Path, cluster_id: &str, reason: &str) -> anyhow::Result<()> {
    let dir = candidates_dir(repo_root).join(cluster_id);
    if !dir.exists() {
        anyhow::bail!("no candidate '{cluster_id}' found under {}", candidates_dir(repo_root).display());
    }
    std::fs::write(dir.join("rejected.json"), serde_json::json!({ "reason": reason, "rejectedAt": chrono::Utc::now().to_rfc3339() }).to_string())?;
    println!("Rejected '{cluster_id}': {reason}");
    Ok(())
}

#[derive(Debug, Default)]
struct RuleKindCounts {
    pattern: usize,
    taint: usize,
    threshold: usize,
}

#[derive(Deserialize)]
struct MinimalRuleMeta {
    #[serde(default = "default_rule_kind")]
    kind: String,
}

fn default_rule_kind() -> String {
    "pattern".to_string()
}

/// Recursively counts a resolved pack's rule files by declared `kind:` —
/// a small, standalone walk (not the shared `analyzers::ast_grep` machinery,
/// which is private to `autoreview-core`) since this listing command only
/// needs the count, not the parsed rule bodies themselves.
fn count_rule_kinds(pack_root: &std::path::Path) -> RuleKindCounts {
    let mut counts = RuleKindCounts::default();
    count_rule_kinds_inner(pack_root, &mut counts);
    counts
}

fn count_rule_kinds_inner(dir: &std::path::Path, counts: &mut RuleKindCounts) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            count_rule_kinds_inner(&path, counts);
            continue;
        }
        let is_yaml = path.extension().and_then(|e| e.to_str()).map(|e| e == "yml" || e == "yaml").unwrap_or(false);
        if !is_yaml || path.file_name().and_then(|n| n.to_str()) == Some("rulepack.yaml") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        let Ok(meta) = serde_yaml::from_str::<MinimalRuleMeta>(&contents) else { continue };
        match meta.kind.as_str() {
            "taint" => counts.taint += 1,
            "threshold" => counts.threshold += 1,
            _ => counts.pattern += 1,
        }
    }
}

/// Lists every pack registered in `.autoreview/rulepacks.yaml`: id, source
/// resolution status, and a rule-kind breakdown for packs that resolved
/// successfully. A pack that failed to resolve is listed with its error —
/// same information `diff`'s own `[warn]` line shows, surfaced here on
/// demand instead of only in review output.
pub fn run_rules_packs(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config_path = autoreview_core::rule_packs_config_path(repo_root);
    let configured = autoreview_core::load_rule_packs_config(&config_path)?;
    if configured.is_empty() {
        println!("No rule packs registered ({} not found or empty).", config_path.display());
        return Ok(());
    }

    let cache_root = autoreview_core::default_rule_packs_cache_root();
    println!("Registered rule packs:\n");
    for (id, result) in autoreview_core::resolve_rule_packs(repo_root, &cache_root, &configured) {
        match result {
            Ok(resolved) => {
                let counts = count_rule_kinds(&resolved.local_path);
                let trust = match resolved.trust {
                    autoreview_schema::RulePackTrust::Full => "full",
                    autoreview_schema::RulePackTrust::Shadow => "shadow",
                };
                println!("  {id} [{trust}] — {} pattern, {} taint, {} threshold rule(s) ({})", counts.pattern, counts.taint, counts.threshold, resolved.local_path.display());
            }
            Err(err) => println!("  {id} — failed to resolve: {err}"),
        }
    }
    Ok(())
}

/// Recognizes a `kind: git` source by shape — anything else is treated as
/// a `kind: local` filesystem path. Deliberately simple (no network probe,
/// no `git ls-remote` check): the same shorthand `git clone` itself
/// accepts, so a user who'd type it into `git clone <source>` can type the
/// same thing here.
fn looks_like_git_source(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://") || source.starts_with("git@") || source.ends_with(".git")
}

/// `autoreview rules packs add <source>` — registers a new external rule
/// pack in `.autoreview/rulepacks.yaml`. `source` is either a local
/// filesystem path (relative to `repo_root`) or a git URL/shorthand
/// (`looks_like_git_source`); the pack's own id comes from its
/// `rulepack.yaml`, not from an extra flag — resolving the source is what
/// proves it's a real, valid pack before anything gets written.
pub fn run_rules_packs_add(repo_root: &std::path::Path, source: &str) -> anyhow::Result<()> {
    let source_config = if looks_like_git_source(source) {
        autoreview_schema::RulePackSourceConfig::Git { url: source.to_string(), r#ref: None, subpath: None }
    } else {
        autoreview_schema::RulePackSourceConfig::Local { path: source.to_string() }
    };

    let cache_root = autoreview_core::default_rule_packs_cache_root();
    let (id, local_path) = autoreview_core::discover_pack_source(repo_root, &cache_root, &source_config)?;

    let config_path = autoreview_core::rule_packs_config_path(repo_root);
    let mut configured = autoreview_core::load_rule_packs_config(&config_path)?;
    if let Some(existing) = configured.iter().find(|p| p.id == id) {
        anyhow::bail!("a pack with id '{id}' is already registered (source: {:?}) — remove it from {} first if you meant to replace it", existing.source, config_path.display());
    }

    configured.push(autoreview_schema::RulePackConfig { id: id.clone(), source: source_config, trust: autoreview_schema::RulePackTrust::Full });
    autoreview_core::save_rule_packs_config(&config_path, &autoreview_schema::RulePacksFile { packs: configured })?;

    println!("Registered rule pack '{id}' (resolved to {}).", local_path.display());
    println!("Wrote {}. It runs at full trust by default — edit `trust: shadow` there to stage it first.", config_path.display());
    Ok(())
}

/// Manually reverses a shadow/promoted rule's most recent lifecycle step —
/// the human override sitting alongside the automatic `should_promote`/
/// `should_demote` gate `diff.rs` already runs on every review. A
/// `"promoted"` rule demotes back to `"shadow"` (same status flip and
/// on-disk file move `diff.rs` does automatically on a real demotion, just
/// triggered by hand instead of the firing-history gate). A `"shadow"`
/// rule rolls all the way back to `"rejected"` — undoing the original
/// `rules review --approve` — moving its file to `.autoreview/rules/
/// rejected/` (kept, not deleted, so it can be inspected or manually
/// restored) and stamping `invalid_at` on its history row.
pub fn run_rules_rollback(repo_root: &std::path::Path, rule_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;

    let Some(state) = store.rule_state(rule_id)? else {
        anyhow::bail!("rule '{rule_id}' is not tracked (never fired in shadow/promoted mode) — nothing to roll back");
    };

    match state.status.as_str() {
        "promoted" => {
            store.set_rule_status(rule_id, "shadow")?;
            move_shadow_rule_file(repo_root, rule_id, "promoted", "shadow");
            println!("Rolled back '{rule_id}': promoted -> shadow.");
        }
        "shadow" => {
            store.set_rule_status(rule_id, "rejected")?;
            store.invalidate_rule(rule_id, &chrono::Utc::now().to_rfc3339())?;
            move_shadow_rule_file(repo_root, rule_id, "shadow", "rejected");
            println!("Rolled back '{rule_id}': shadow -> rejected (rule file moved to .autoreview/rules/rejected/, not deleted).");
        }
        other => anyhow::bail!("rule '{rule_id}' has status '{other}' — rollback only applies to a 'shadow' or 'promoted' rule"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_test_support::init_repo;

    fn write_rule_file(repo_root: &std::path::Path, status: &str, rule_id: &str) {
        let dir = repo_root.join(".autoreview").join("rules").join(status);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{rule_id}.yaml")), format!("id: {rule_id}\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: m\nrule:\n  pattern: $A == $A\n")).unwrap();
    }

    #[test]
    fn rollback_of_an_untracked_rule_errors_clearly() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let err = run_rules_rollback(repo.path(), "never-seen-rule").unwrap_err();
        assert!(err.to_string().contains("not tracked"), "got: {err}");
    }

    #[test]
    fn rollback_demotes_a_promoted_rule_back_to_shadow() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        write_rule_file(repo.path(), "promoted", "go-example");
        let history_dir = history_dir_for(repo.path());
        let store = HistoryStore::open(&history_dir).unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        store.set_rule_status("go-example", "promoted").unwrap();
        drop(store);

        run_rules_rollback(repo.path(), "go-example").unwrap();

        let store = HistoryStore::open(&history_dir).unwrap();
        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.status, "shadow");
        assert!(repo.path().join(".autoreview/rules/shadow/go-example.yaml").exists());
        assert!(!repo.path().join(".autoreview/rules/promoted/go-example.yaml").exists());
    }

    #[test]
    fn rollback_rejects_a_shadow_rule_and_moves_its_file_without_deleting_it() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        write_rule_file(repo.path(), "shadow", "go-example");
        let history_dir = history_dir_for(repo.path());
        let store = HistoryStore::open(&history_dir).unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        drop(store);

        run_rules_rollback(repo.path(), "go-example").unwrap();

        let store = HistoryStore::open(&history_dir).unwrap();
        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.status, "rejected");
        assert!(state.invalid_at.is_some());
        assert!(repo.path().join(".autoreview/rules/rejected/go-example.yaml").exists());
        assert!(!repo.path().join(".autoreview/rules/shadow/go-example.yaml").exists());
    }

    #[test]
    fn rollback_of_an_already_rejected_rule_errors_with_a_clear_message() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let history_dir = history_dir_for(repo.path());
        let store = HistoryStore::open(&history_dir).unwrap();
        store.ensure_rule_tracked("go-example", "rejected", "2026-07-01T00:00:00Z").unwrap();
        drop(store);

        let err = run_rules_rollback(repo.path(), "go-example").unwrap_err();
        assert!(err.to_string().contains("rejected"), "got: {err}");
    }
}
