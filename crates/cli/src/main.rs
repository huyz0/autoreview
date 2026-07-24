mod commands;
mod render;

use clap::{Parser, Subcommand};

use autoreview_schema::{AgentBackendKind, Tier};
use commands::apply::run_apply;
use commands::auth::{run_auth_login, run_auth_logout, run_auth_status};
use commands::diff::{run_diff_or_watch, DiffCommandOptions};
use commands::doctor::run_doctor;
use commands::explain::run_explain;
use commands::feedback::{run_feedback, run_missed_report};
use commands::history::{run_history_costs, run_history_sync};
use commands::rules::{
    run_rules_bench, run_rules_mine, run_rules_mine_bitbucket_comments, run_rules_mine_bugfix_commits, run_rules_mine_code, run_rules_mine_comments, run_rules_mine_linter_config, run_rules_mine_llm_patterns,
    run_rules_mine_suppressions, run_rules_packs, run_rules_packs_add, run_rules_packs_refresh, run_rules_packs_validate, run_rules_review, run_rules_rollback, run_rules_shadow_log,
};
use commands::skills::{run_skills_list, run_skills_mine, run_skills_review, run_skills_rollback};
use commands::skills_bench::run_skills_bench;
use commands::spec::{run_spec_draft, SpecDraftOptions};

#[derive(Parser)]
#[command(name = "autoreview", version, about = "Portable, hierarchical, deterministic-first code review CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check that required tools (git, claude, ast-grep, analyzers) are available
    Doctor,
    /// Review the diff between two refs (default: origin/main...HEAD)
    Diff {
        #[arg(long, default_value = "origin/main")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long)]
        tier: Option<String>,
        #[arg(long)]
        aspects: Option<String>,
        #[arg(long = "max-usd")]
        max_usd: Option<f64>,
        /// Suppress findings whose fingerprint was already reported in the most recent prior run on this repo
        #[arg(long)]
        incremental: bool,
        /// Which agent backend drives specialists: claude-code (default), pi, or local-llm
        #[arg(long)]
        backend: Option<String>,
        /// Re-run automatically whenever base...head changes (e.g. a new
        /// commit lands on head) instead of running once and exiting
        #[arg(long)]
        watch: bool,
        /// Seconds between change checks in --watch mode
        #[arg(long, default_value_t = 3)]
        watch_interval: u64,
    },
    /// Apply a finding's suggested patch, gated by a `git apply --check` sanity check
    Apply { finding_id: String },
    /// Show why a finding fired: its recorded source, and (for a
    /// deterministic rule) the exact rule definition it matched
    Explain { finding_id: String },
    /// Record a verdict on a finding, or report a missed finding. --fp/--tp
    /// are the quick/generic verdicts; --doesnt-apply/--accepted-risk/
    /// --fix-in-followup record a more specific reason a finding wasn't
    /// acted on (modeled on Aviator Verify's waiver taxonomy) — all three
    /// still count as a confirmed true positive for rule-precision
    /// purposes, but --doesnt-apply does NOT count as evidence the rule
    /// itself is wrong the way --fp does.
    Feedback {
        id: String,
        #[arg(long)]
        fp: bool,
        #[arg(long)]
        tp: bool,
        /// The rule is valid in general but doesn't apply to this specific case
        #[arg(long)]
        doesnt_apply: bool,
        /// The finding is real; the author accepts the risk and won't fix it
        #[arg(long)]
        accepted_risk: bool,
        /// The finding is real and will be fixed in a separate follow-up PR
        #[arg(long)]
        fix_in_followup: bool,
        #[arg(long)]
        missed: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Manage the learned-rule factory: mine candidates from findings/PR
    /// comments/code, review/approve into shadow mode, bench against
    /// history, and the automatic shadow -> promoted lifecycle diff.rs runs
    /// on every review.
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Manage review skills (list, mine, bench, review, rollback)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Manage local run/event history
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },
    /// Manage .autoreview/spec.md, the optional acceptance-criteria spec
    /// `autoreview diff` checks the change against (Initiative 3)
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
    /// Manage GitHub/Bitbucket credentials for network-backed mining
    /// sources (rules mine --from-bitbucket-comments, and GitHub sources
    /// beyond the existing gh-shelling-out --from-comments)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Log into a provider (currently: bitbucket) and store the
    /// credential. --email/--token make this non-interactive (for CI);
    /// omitted, they're prompted for (the token's input is hidden)
    Login {
        provider: String,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Show whether a GitHub/Bitbucket credential is currently stored —
    /// read-only, no network call
    Status,
    /// Remove a locally stored credential — does NOT revoke it
    /// server-side, see run_auth_logout's own docs for why
    Logout { provider: String },
}

#[derive(Subcommand)]
enum SpecAction {
    /// Draft .autoreview/spec.md from the base...head diff via the
    /// configured agent backend — a starting point to review and edit, not
    /// a final answer.
    Draft {
        #[arg(long, default_value = "origin/main")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// Overwrite an existing .autoreview/spec.md
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum HistoryAction {
    /// Pull the team's synced event log (storage.sync.mode: git) onto this machine
    Sync,
    /// Show total/per-stage/per-day spend across every local run
    Costs {
        /// Only include runs on or after this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
    },
}

#[derive(Subcommand)]
enum RulesAction {
    /// Mine recurring agent findings into rule candidates. With
    /// --from-comments, mines recurring human PR review comments via `gh`
    /// instead (opt-in — see mineFromComments in .autoreview/config.yaml).
    /// With --from-code, mines call-pair usage conventions directly from
    /// the repo's own Go source (discovery prototype, prints findings —
    /// see commands::rules::run_rules_mine_code). With
    /// --from-bugfix-commits, mines the repo's own local git history for
    /// bug-fix-shaped commits — no auth/network needed at all. With
    /// --from-suppressions, mines linter-suppression comments (// nolint,
    /// @SuppressWarnings, // eslint-disable, # noqa) already present in
    /// the repo's own source — also no auth/network needed. With
    /// --from-bitbucket-comments, mines recurring human PR review
    /// comments from Bitbucket Cloud — needs `autoreview auth login
    /// bitbucket` first (opt-in — see mineFromBitbucketComments in
    /// .autoreview/config.yaml). With --from-linter-config, prints a
    /// report comparing .golangci.yml/.eslintrc/checkstyle/detekt config
    /// against autoreview's own rule catalog (report only, not fed into
    /// the draft pipeline). With --from-llm-patterns, samples representative
    /// source files and asks the configured agent backend to propose
    /// call-pair conventions, mechanically re-verifying every proposal
    /// against the whole repo before it's drafted — opt-in (see
    /// mineFromLlmPatterns in .autoreview/config.yaml) since it sends whole
    /// file contents to the backend, unlike every other source.
    Mine {
        #[arg(long)]
        from_comments: bool,
        #[arg(long)]
        from_code: bool,
        #[arg(long)]
        from_bugfix_commits: bool,
        #[arg(long)]
        from_suppressions: bool,
        #[arg(long)]
        from_bitbucket_comments: bool,
        #[arg(long)]
        from_linter_config: bool,
        #[arg(long)]
        from_llm_patterns: bool,
    },
    /// Bench a candidate rule against its self-test + historical precision
    Bench { cluster_id: String },
    /// Human review queue for candidate rules — lists candidates by default;
    /// --approve moves a candidate to shadow mode, --reject <clusterId> --reason <text> records why
    Review {
        #[arg(long)]
        approve: Option<String>,
        #[arg(long)]
        reject: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Inspect recent firings of a shadow-mode rule
    ShadowLog { rule_id: String },
    /// Roll back a promoted/shadow rule to its prior state
    Rollback { rule_id: String },
    /// List registered external rule packs (.autoreview/rulepacks.yaml) and
    /// how many rules of each kind each one resolved to. With a subcommand,
    /// manages the registration instead of listing it.
    Packs {
        #[command(subcommand)]
        action: Option<PacksAction>,
    },
}

#[derive(Subcommand)]
enum PacksAction {
    /// Register a new external rule pack. `source` is a local filesystem
    /// path (relative to the repo root) or a git URL — the pack's id comes
    /// from its own rulepack.yaml, not a flag, since resolving the source
    /// is what proves it's a real pack before anything gets written to
    /// .autoreview/rulepacks.yaml.
    Add { source: String },
    /// Checks a rule pack directory is well-formed (readable rulepack.yaml,
    /// every rule file valid for its own kind, no duplicate ids) — doesn't
    /// touch .autoreview/rulepacks.yaml, so it works on a pack that isn't
    /// registered anywhere yet.
    Validate { path: String },
    /// Forces a fresh fetch for every registered git-source pack and
    /// reports whether its commit changed. Every `autoreview diff` already
    /// re-fetches a git pack's ref fresh on every run, so this doesn't
    /// change what a review sees — it's a manual, explicit health/freshness
    /// check (CI pre-warming, "did anything change") independent of
    /// running a full review.
    Refresh,
}

#[derive(Subcommand)]
enum SkillsAction {
    /// List all skills visible to this repo (builtins + repo-local overrides)
    List,
    /// Mine feedback into skill-edit proposals
    Mine,
    /// Replay-bench a skill-edit proposal against the history store
    Bench { aspect: String, proposal_id: String },
    /// Human review queue for skill-edit proposals — lists proposals by
    /// default; --approve <aspect>:<proposalId> applies the proposal's
    /// drafted line to the skill's repo-local instructions.md override,
    /// --reject <aspect>:<proposalId> --reason <text> records why
    Review {
        #[arg(long)]
        approve: Option<String>,
        #[arg(long)]
        reject: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Roll back a skill to a prior version
    Rollback { aspect: String, version: String },
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "quick" => Some(Tier::Quick),
        "standard" => Some(Tier::Standard),
        "deep" => Some(Tier::Deep),
        _ => {
            eprintln!("warning: unrecognized --tier '{s}', ignoring override");
            None
        }
    }
}

fn parse_backend(s: &str) -> Option<AgentBackendKind> {
    match s {
        "claude-code" => Some(AgentBackendKind::ClaudeCode),
        "pi" => Some(AgentBackendKind::Pi),
        "local-llm" => Some(AgentBackendKind::LocalLlm),
        _ => {
            eprintln!("warning: unrecognized --backend '{s}', ignoring override");
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    match cli.command {
        Commands::Doctor => {
            run_doctor(&repo_root);
        }
        Commands::Diff { base, head, tier, aspects, max_usd, incremental, backend, watch, watch_interval } => {
            run_diff_or_watch(
                DiffCommandOptions {
                    repo_root,
                    base_ref: base,
                    head_ref: head,
                    tier: tier.and_then(|t| parse_tier(&t)),
                    aspects: aspects.map(|a| a.split(',').map(|s| s.to_string()).collect()),
                    max_usd,
                    incremental,
                    backend: backend.and_then(|b| parse_backend(&b)),
                },
                watch,
                watch_interval,
            )?;
        }
        Commands::Apply { finding_id } => run_apply(&repo_root, &finding_id)?,
        Commands::Explain { finding_id } => run_explain(&repo_root, &finding_id)?,
        Commands::Feedback { id, fp, tp, doesnt_apply, accepted_risk, fix_in_followup, missed, note } => {
            let flags = [fp, tp, doesnt_apply, accepted_risk, fix_in_followup, missed.is_some()];
            if flags.iter().filter(|f| **f).count() > 1 {
                eprintln!("error: specify only one of --fp, --tp, --doesnt-apply, --accepted-risk, --fix-in-followup, or --missed <description>");
                std::process::exit(1);
            }
            if let Some(description) = missed {
                run_missed_report(&repo_root, &id, &description)?;
            } else if fp {
                run_feedback(&repo_root, &id, autoreview_schema::FeedbackVerdict::FalsePositive, note.as_deref())?;
            } else if tp {
                run_feedback(&repo_root, &id, autoreview_schema::FeedbackVerdict::TruePositive, note.as_deref())?;
            } else if doesnt_apply {
                run_feedback(&repo_root, &id, autoreview_schema::FeedbackVerdict::DoesntApply, note.as_deref())?;
            } else if accepted_risk {
                run_feedback(&repo_root, &id, autoreview_schema::FeedbackVerdict::AcceptedRisk, note.as_deref())?;
            } else if fix_in_followup {
                run_feedback(&repo_root, &id, autoreview_schema::FeedbackVerdict::FixInFollowup, note.as_deref())?;
            } else {
                eprintln!("error: specify one of --fp, --tp, --doesnt-apply, --accepted-risk, --fix-in-followup, or --missed <description>");
                std::process::exit(1);
            }
        }
        Commands::Rules { action } => match action {
            RulesAction::Mine { from_comments, from_code, from_bugfix_commits, from_suppressions, from_bitbucket_comments, from_linter_config, from_llm_patterns } => {
                if [from_comments, from_code, from_bugfix_commits, from_suppressions, from_bitbucket_comments, from_linter_config, from_llm_patterns].iter().filter(|f| **f).count() > 1 {
                    eprintln!(
                        "error: --from-comments, --from-code, --from-bugfix-commits, --from-suppressions, --from-bitbucket-comments, --from-linter-config, and --from-llm-patterns are mutually exclusive"
                    );
                    std::process::exit(1);
                } else if from_comments {
                    run_rules_mine_comments(&repo_root)?
                } else if from_code {
                    run_rules_mine_code(&repo_root)?
                } else if from_bugfix_commits {
                    run_rules_mine_bugfix_commits(&repo_root)?
                } else if from_suppressions {
                    run_rules_mine_suppressions(&repo_root)?
                } else if from_bitbucket_comments {
                    run_rules_mine_bitbucket_comments(&repo_root)?
                } else if from_linter_config {
                    run_rules_mine_linter_config(&repo_root)?
                } else if from_llm_patterns {
                    run_rules_mine_llm_patterns(&repo_root)?
                } else {
                    run_rules_mine(&repo_root)?
                }
            }
            RulesAction::Bench { cluster_id } => run_rules_bench(&repo_root, &cluster_id)?,
            RulesAction::Review { approve, reject, reason } => run_rules_review(&repo_root, approve, reject, reason)?,
            RulesAction::ShadowLog { rule_id } => run_rules_shadow_log(&repo_root, &rule_id)?,
            RulesAction::Rollback { rule_id } => run_rules_rollback(&repo_root, &rule_id)?,
            RulesAction::Packs { action: None } => run_rules_packs(&repo_root)?,
            RulesAction::Packs { action: Some(PacksAction::Add { source }) } => run_rules_packs_add(&repo_root, &source)?,
            RulesAction::Packs { action: Some(PacksAction::Validate { path }) } => run_rules_packs_validate(std::path::Path::new(&path))?,
            RulesAction::Packs { action: Some(PacksAction::Refresh) } => run_rules_packs_refresh(&repo_root)?,
        },
        Commands::Skills { action } => match action {
            SkillsAction::List => run_skills_list(&repo_root)?,
            SkillsAction::Mine => run_skills_mine(&repo_root)?,
            SkillsAction::Bench { aspect, proposal_id } => run_skills_bench(&repo_root, &aspect, &proposal_id)?,
            SkillsAction::Review { approve, reject, reason } => run_skills_review(&repo_root, approve, reject, reason)?,
            SkillsAction::Rollback { aspect, version } => run_skills_rollback(&repo_root, &aspect, &version)?,
        },
        Commands::History { action } => match action {
            HistoryAction::Sync => run_history_sync(&repo_root)?,
            HistoryAction::Costs { since } => run_history_costs(&repo_root, since.as_deref())?,
        },
        Commands::Spec { action } => match action {
            SpecAction::Draft { base, head, force } => run_spec_draft(SpecDraftOptions { repo_root, base_ref: base, head_ref: head, force })?,
        },
        Commands::Auth { action } => match action {
            AuthAction::Login { provider, email, token } => run_auth_login(&repo_root, &provider, email, token)?,
            AuthAction::Status => run_auth_status()?,
            AuthAction::Logout { provider } => run_auth_logout(&provider)?,
        },
    }

    Ok(())
}
