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

mod packs;
mod review;

pub use packs::{run_rules_packs, run_rules_packs_add, run_rules_packs_refresh, run_rules_packs_validate};
pub use review::{run_rules_bench, run_rules_review, run_rules_rollback, run_rules_shadow_log};
pub(super) use packs::validate_rule_file;

use autoreview_core::{
    draft_candidate, find_similar_existing_rule, load_existing_rule_summaries, mine_candidates, mine_from_bitbucket_pr_comments, mine_from_llm_patterns, mine_from_pr_comments, resolve_bitbucket_repo_slug, write_seed_file,
    CandidateSeed, CredentialStore, DraftOutcome, HistoryStore, BITBUCKET_SERVICE,
};
use autoreview_schema::AgentBackendKind;

use super::backend::{backend_available, build_backend};
use super::history::history_dir_for;

pub(super) fn cheap_model_for(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::OpenAiCompatible => &config.agents.open_ai_compatible.model,
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => &config.budgets.models.cheap,
    }
}

/// Resolves every registered rule pack (`.autoreview/rulepacks.yaml`),
/// same warn-and-skip-on-failure convention `diff.rs` uses for a full
/// review run — a pack that fails to resolve shouldn't block a mining
/// run any more than it blocks a review. Shared by every mine source that
/// needs the existing-rule set (the dedup gate) or, later, the taint/
/// call-sequence rule loaders themselves.
fn resolved_packs_for_mining(repo_root: &std::path::Path) -> Vec<autoreview_core::ResolvedRulePack> {
    let configured = autoreview_core::load_rule_packs_config(&autoreview_core::rule_packs_config_path(repo_root)).unwrap_or_default();
    let cache_root = autoreview_core::default_rule_packs_cache_root();
    autoreview_core::resolve_rule_packs(repo_root, &cache_root, &configured)
        .into_iter()
        .filter_map(|(id, result)| match result {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                println!("  [warn] failed to resolve rule pack '{id}': {err}");
                None
            }
        })
        .collect()
}

/// Drops any mined candidate that closely matches a rule already shipped
/// (builtin or a registered pack) — closes the gap where nothing
/// previously stopped a mining source from proposing a rule that
/// duplicates one that already exists, leaving a human at `rules review`
/// as the only backstop. Prints what got skipped and why rather than
/// silently shrinking the seed list, matching this pipeline's existing
/// "always say what happened" tone.
fn filter_out_existing_rule_duplicates(repo_root: &std::path::Path, seeds: Vec<CandidateSeed>) -> Vec<CandidateSeed> {
    let existing = load_existing_rule_summaries(&resolved_packs_for_mining(repo_root));
    seeds
        .into_iter()
        .filter(|seed| match find_similar_existing_rule(seed, &existing) {
            Some(m) => {
                println!("  {} skipped — matches existing rule '{}' (similarity {:.2})", seed.cluster_id, m.rule_id, m.similarity);
                false
            }
            None => true,
        })
        .collect()
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
        let directory = seed_path.parent().expect("the seed is written inside a candidate directory");
        let rule_path = directory.join("rule.yaml");
        if rule_path.symlink_metadata().is_ok() {
            println!("    [draft] kept existing rule at {}", rule_path.display());
            continue;
        }

        let Some(backend) = &backend else {
            println!("    [draft] skipped — no agent backend available (needed to attempt a rule draft)");
            continue;
        };
        let (outcome, _usage) = draft_candidate(backend.as_ref(), seed, &model, 2, repo_root);
        match outcome {
            DraftOutcome::Drafted { rule_yaml, agreement_count } => {
                let mut draft = tempfile::NamedTempFile::new_in(directory)?;
                std::io::Write::write_all(&mut draft, rule_yaml.as_bytes())?;
                draft.persist_noclobber(&rule_path)?;
                println!("    [draft] {agreement_count}/5 attempts agreed -> {}", rule_path.display());
            }
            DraftOutcome::Inexpressible { rationale } => {
                println!("    [draft] inexpressible as a syntactic rule: {rationale}");
            }
        }
    }
    println!("\n(Next: add tests/positive and tests/negative fixtures under a drafted candidate's directory, run `autoreview rules bench <clusterId>`, then `autoreview rules review --approve <clusterId>` to move it into shadow mode.)");
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
    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every recurring cluster matched a rule that's already shipped — nothing new to draft.");
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
    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every recurring cluster matched a rule that's already shipped — nothing new to draft.");
        return Ok(());
    }

    draft_and_write_seeds(repo_root, &config, &seeds)
}

/// `autoreview rules mine --from-bugfix-commits` — mines the repo's own
/// local git history for bug-fix-shaped commits (see
/// `rule_factory::mine_from_bugfix_commits`'s own module docs). No
/// `.enabled` config gate, unlike `--from-comments`: this never leaves
/// the local `git log`/`git show`, so there's no external network/auth
/// surface to opt into.
pub fn run_rules_mine_bugfix_commits(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let max_commits = config.mine_from_bugfix_commits.max_commits;

    println!("Scanning the {max_commits} most recent commit(s) for bug-fix-shaped changes...");
    let findings = autoreview_core::mine_from_bugfix_commits(repo_root, max_commits)?;
    if findings.is_empty() {
        println!("No bug-fix-shaped commits found in that window — nothing to mine.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar fixes spanning >= 2 distinct commits).");
        return Ok(());
    }
    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every recurring cluster matched a rule that's already shipped — nothing new to draft.");
        return Ok(());
    }

    draft_and_write_seeds(repo_root, &config, &seeds)
}

/// `autoreview rules mine --from-suppressions` — mines linter-suppression
/// comments already present in the repo's own source (see
/// `rule_factory::mine_from_suppressions`'s own module docs). No
/// `.enabled` config gate — a pure filesystem walk over the repo's own
/// already-checked-out source, no external tool, no network.
pub fn run_rules_mine_suppressions(repo_root: &std::path::Path) -> anyhow::Result<()> {
    println!("Scanning the repo's own source for linter-suppression comments...");
    let findings = autoreview_core::mine_from_suppressions(repo_root);
    if findings.is_empty() {
        println!("No linter-suppression comments found — nothing to mine.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar suppressions spanning >= 2 distinct files).");
        return Ok(());
    }
    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every recurring cluster matched a rule that's already shipped — nothing new to draft.");
        return Ok(());
    }

    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    draft_and_write_seeds(repo_root, &config, &seeds)
}

/// `autoreview rules mine --from-bitbucket-comments` — mines recurring
/// human PR review comments from a Bitbucket Cloud repository (see
/// `rule_factory::mine_from_bitbucket_comments`'s own module docs). Opt-in
/// (`mineFromBitbucketComments.enabled: true`) since it hits the real
/// Bitbucket API, and needs a stored credential first — errors clearly
/// pointing at `auth login bitbucket` rather than silently doing nothing
/// when one isn't configured.
pub fn run_rules_mine_bitbucket_comments(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    if !config.mine_from_bitbucket_comments.enabled {
        println!("mineFromBitbucketComments.enabled is false in .autoreview/config.yaml — nothing to do. Set it to true to mine recurring PR review comments via Bitbucket's API.");
        return Ok(());
    }

    let store = CredentialStore::open_default();
    let Some(email) = store.recall_account(BITBUCKET_SERVICE) else {
        anyhow::bail!("no Bitbucket credential stored — run `autoreview auth login bitbucket` first");
    };
    let Some(token) = store.load(BITBUCKET_SERVICE, &email)? else {
        anyhow::bail!("no Bitbucket credential stored — run `autoreview auth login bitbucket` first");
    };

    let (workspace, repo_slug) = match &config.mine_from_bitbucket_comments.workspace {
        Some(workspace) => {
            let Some((_, repo_slug)) = resolve_bitbucket_repo_slug(repo_root) else {
                anyhow::bail!("mineFromBitbucketComments.workspace is set, but the repo slug couldn't be inferred from origin's remote URL — this needs a real bitbucket.org remote configured");
            };
            (workspace.clone(), repo_slug)
        }
        None => match resolve_bitbucket_repo_slug(repo_root) {
            Some(resolved) => resolved,
            None => anyhow::bail!("couldn't determine the Bitbucket workspace/repo — set mineFromBitbucketComments.workspace in .autoreview/config.yaml, or configure a bitbucket.org origin remote"),
        },
    };

    println!("Fetching the {} most recently merged PRs' review comments from {workspace}/{repo_slug}...", config.mine_from_bitbucket_comments.lookback_prs);
    let findings = mine_from_bitbucket_pr_comments(&email, &token, &workspace, &repo_slug, config.mine_from_bitbucket_comments.lookback_prs, &config.mine_from_bitbucket_comments.curl_binary)?;
    if findings.is_empty() {
        println!("No substantive review comments found on recent merged PRs — nothing to mine.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar comments spanning >= 2 distinct PRs).");
        return Ok(());
    }
    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every recurring cluster matched a rule that's already shipped — nothing new to draft.");
        return Ok(());
    }

    draft_and_write_seeds(repo_root, &config, &seeds)
}

/// `autoreview rules mine --from-linter-config` — compares whatever
/// linter config the repo already has (`.golangci.yml`/`.eslintrc.*`/
/// checkstyle/detekt) against autoreview's own rule catalog (see
/// `rule_factory::config_gap_report`'s own module docs). Report only,
/// same "prototype — inspect only" posture `--from-code` already has —
/// no seed files, no draft attempt.
pub fn run_rules_mine_linter_config(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let existing = load_existing_rule_summaries(&resolved_packs_for_mining(repo_root));
    let reports = autoreview_core::compare_linter_configs_to_rule_catalog(repo_root, &existing);
    if reports.is_empty() {
        println!("No .golangci.yml/.eslintrc.*/checkstyle/detekt config found under this repo — nothing to compare.");
        return Ok(());
    }

    for report in &reports {
        println!("{} ({}):", report.tool, report.config_path.display());
        if report.disabled_matches.is_empty() {
            println!("  No disabled checks matched an existing autoreview rule.");
        } else {
            println!("  Disabled in {}, but autoreview already covers it (a standing false-positive risk for this team):", report.tool);
            for m in &report.disabled_matches {
                println!("    {} ~ {} (similarity {:.2})", m.linter_check, m.autoreview_rule_id, m.similarity);
            }
        }
        if !report.uncovered_checks.is_empty() {
            println!("  Enabled in {}, with no obvious autoreview equivalent (a possible coverage gap):", report.tool);
            for name in &report.uncovered_checks {
                println!("    {name}");
            }
        }
        println!();
    }
    Ok(())
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

/// `autoreview rules mine --from-llm-patterns` — the ninth mining source
/// (see `rule_factory::mine_from_llm_patterns`'s own module docs): unlike
/// every other source, its very first step (proposing conventions) itself
/// needs a real agent backend, not just its optional drafting step — so
/// this checks `backend_available` up front and errors clearly rather than
/// silently doing nothing. Opt-in (`mineFromLlmPatterns.enabled: true`)
/// for a different reason than the network-backed sources: this is the
/// only mining source that sends whole sampled file *contents* to the
/// configured backend, a materially larger privacy surface than the short
/// titles/messages every other source works with — the warning below says
/// so at the point it actually happens, not just in a doc comment.
pub fn run_rules_mine_llm_patterns(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    if !config.mine_from_llm_patterns.enabled {
        println!(
            "mineFromLlmPatterns.enabled is false in .autoreview/config.yaml — nothing to do. Set it to true to let an agent propose call-pair conventions from sampled source files (this sends whole file contents to the configured agent backend — see the config's doc comment before enabling)."
        );
        return Ok(());
    }

    let backend_kind = config.agents.backend;
    if !backend_available(backend_kind, &config) {
        anyhow::bail!("no agent backend available — mineFromLlmPatterns needs one for its very first step (proposing conventions), unlike the other mining sources whose only backend use is the optional drafting step");
    }
    let backend = build_backend(backend_kind, &config);
    let model = cheap_model_for(backend_kind, &config).to_string();

    println!(
        "Sampling up to {} representative file(s) and asking the agent backend to propose call-pair conventions (sends whole file contents to the backend)...",
        config.mine_from_llm_patterns.max_sample_files
    );
    let (seeds, proposed_count) = mine_from_llm_patterns(
        backend.as_ref(),
        repo_root,
        &model,
        2,
        config.mine_from_llm_patterns.max_sample_files,
        config.mine_from_llm_patterns.min_occurrences,
        config.mine_from_llm_patterns.min_consistency,
    );
    println!("The agent proposed {proposed_count} convention(s); {} survived mechanical re-verification against the whole repo.", seeds.len());
    if seeds.is_empty() {
        println!("Nothing survived verification — nothing to mine.");
        return Ok(());
    }

    let seeds = filter_out_existing_rule_duplicates(repo_root, seeds);
    if seeds.is_empty() {
        println!("Every verified convention matched a rule that's already shipped — nothing new to draft.");
        return Ok(());
    }

    draft_and_write_seeds(repo_root, &config, &seeds)
}
