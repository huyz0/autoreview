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
    Diff(DiffArgs),
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
    Feedback(FeedbackArgs),
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

#[derive(clap::Args)]
struct DiffArgs {
    #[arg(long, default_value = "origin/main")]
    base: String,
    #[arg(long, default_value = "HEAD")]
    head: String,
    #[arg(long)]
    tier: Option<String>,
    #[arg(long)]
    aspects: Option<String>,
    /// Limit findings to the given subcategory labels
    #[arg(long, value_delimiter = ',')]
    subcategories: Option<Vec<String>>,
    #[arg(long = "max-usd")]
    max_usd: Option<f64>,
    /// Suppress findings whose fingerprint was already reported in the most recent prior run on this repo
    #[arg(long)]
    incremental: bool,
    /// Run deterministic analyzers only, without calling model backends
    #[arg(long)]
    static_only: bool,
    /// Limit deterministic findings to changed lines or related locations on changed lines
    #[arg(long)]
    changed_lines: bool,
    /// Use the local RustSec database to audit changed Cargo.lock files
    #[arg(long)]
    audit_rust_deps: bool,
    /// Which agent backend drives specialists: claude-code (default), pi, local-llm, or openai-compatible
    #[arg(long)]
    backend: Option<String>,
    /// Re-run automatically whenever base...head changes (e.g. a new
    /// commit lands on head) instead of running once and exiting
    #[arg(long)]
    watch: bool,
    /// Seconds between change checks in --watch mode
    #[arg(long, default_value_t = 3)]
    watch_interval: u64,
}

#[derive(clap::Args)]
struct FeedbackArgs {
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
    /// List built-in rules and their category and subcategory labels
    List {
        #[arg(long, value_delimiter = ',')]
        subcategories: Vec<String>,
        #[arg(long)]
        json: bool,
    },
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
    /// Draft and verify an unregistered DSL rule from a retained finding
    Draft {
        finding_id: String,
        /// Verify that the same source file is clean at this commit
        #[arg(long)]
        fixed: Option<String>,
        /// Validate a local YAML proposal instead of asking the configured model
        #[arg(long)]
        proposal: Option<std::path::PathBuf>,
    },
    /// Bench a candidate rule against its fixtures and sample repository files
    Bench { cluster_id: String },
    /// Measure built-in rules on labelled Git snapshots
    Replay {
        #[arg(long, default_value = ".autoreview/replay.yaml")]
        suite: std::path::PathBuf,
    },
    /// Check changed lines against precision-qualified rules from the base commit
    Gate {
        #[arg(long, default_value = "origin/main")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
    },
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
        "openai-compatible" => Some(AgentBackendKind::OpenAiCompatible),
        _ => {
            eprintln!("warning: unrecognized --backend '{s}', ignoring override");
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let repo = commands::revision::repository_root(&cwd).unwrap_or(cwd);
    match cli.command {
        Commands::Doctor => run_doctor(&repo),
        Commands::Diff(args) => run_review(repo, args)?,
        Commands::Apply { finding_id } => run_apply(&repo, &finding_id)?,
        Commands::Explain { finding_id } => run_explain(&repo, &finding_id)?,
        Commands::Feedback(args) => record_feedback(&repo, args)?,
        Commands::Rules { action } => run_rules(&repo, action)?,
        Commands::Skills { action } => run_skills(&repo, action)?,
        Commands::History { action } => run_history(&repo, action)?,
        Commands::Spec { action } => run_spec(&repo, action)?,
        Commands::Auth { action } => run_auth(&repo, action)?,
    }
    Ok(())
}

fn run_review(repo: std::path::PathBuf, args: DiffArgs) -> anyhow::Result<()> {
    let tier = args.tier.as_deref().map(|value| parse_tier(value).ok_or_else(|| anyhow::anyhow!("invalid tier '{value}'"))).transpose()?;
    let backend = args.backend.as_deref().map(|value| parse_backend(value).ok_or_else(|| anyhow::anyhow!("invalid backend '{value}'"))).transpose()?;
    anyhow::ensure!(args.max_usd.is_none_or(|limit| limit.is_finite() && limit >= 0.0), "--max-usd must be a finite, nonnegative amount");
    run_diff_or_watch(DiffCommandOptions {
        repo_root: repo, base_ref: args.base, head_ref: args.head, tier,
        aspects: args.aspects.map(|value| value.split(',').map(|item| item.trim().to_string()).collect()),
        subcategories: args.subcategories,
        max_usd: args.max_usd, incremental: args.incremental, static_only: args.static_only,
        changed_lines: args.changed_lines, audit_rust_deps: args.audit_rust_deps, backend,
    }, args.watch, args.watch_interval)
}

fn record_feedback(repo: &std::path::Path, args: FeedbackArgs) -> anyhow::Result<()> {
    use autoreview_schema::FeedbackVerdict::*;
    let flags = [
        (args.fp, FalsePositive), (args.tp, TruePositive), (args.doesnt_apply, DoesntApply),
        (args.accepted_risk, AcceptedRisk), (args.fix_in_followup, FixInFollowup),
    ];
    let mut selected = flags.into_iter().filter_map(|(active, verdict)| active.then_some(verdict));
    let verdict = selected.next();
    if selected.next().is_some() || verdict.is_some() && args.missed.is_some() {
        eprintln!("error: specify only one of --fp, --tp, --doesnt-apply, --accepted-risk, --fix-in-followup, or --missed <description>");
        std::process::exit(1);
    }
    if let Some(description) = args.missed {
        return run_missed_report(repo, &args.id, &description);
    }
    let Some(verdict) = verdict else {
        eprintln!("error: specify one of --fp, --tp, --doesnt-apply, --accepted-risk, --fix-in-followup, or --missed <description>");
        std::process::exit(1);
    };
    run_feedback(repo, &args.id, verdict, args.note.as_deref())
}

type MineSource = fn(&std::path::Path) -> anyhow::Result<()>;

fn run_mining(repo: &std::path::Path, sources: &[(bool, MineSource)]) -> anyhow::Result<()> {
    let mut selected = sources.iter().filter_map(|&(enabled, run)| enabled.then_some(run));
    let run = selected.next().unwrap_or(run_rules_mine);
    if selected.next().is_some() {
        eprintln!("error: --from-comments, --from-code, --from-bugfix-commits, --from-suppressions, --from-bitbucket-comments, --from-linter-config, and --from-llm-patterns are mutually exclusive");
        std::process::exit(1);
    }
    run(repo)
}

fn run_rules(repo: &std::path::Path, action: RulesAction) -> anyhow::Result<()> {
    match action {
        RulesAction::List { subcategories, json } => commands::rules::list_builtin(&subcategories, json)?,
        RulesAction::Mine { from_comments, from_code, from_bugfix_commits, from_suppressions, from_bitbucket_comments, from_linter_config, from_llm_patterns } =>
            run_mining(repo, &[(from_comments, run_rules_mine_comments), (from_code, run_rules_mine_code),
                (from_bugfix_commits, run_rules_mine_bugfix_commits), (from_suppressions, run_rules_mine_suppressions),
                (from_bitbucket_comments, run_rules_mine_bitbucket_comments), (from_linter_config, run_rules_mine_linter_config),
                (from_llm_patterns, run_rules_mine_llm_patterns)])?,
        RulesAction::Draft { finding_id, fixed, proposal } => commands::rule_draft::run(repo, &finding_id, fixed.as_deref(), proposal.as_deref())?,
        RulesAction::Bench { cluster_id } => run_rules_bench(repo, &cluster_id)?,
        RulesAction::Replay { suite } => commands::replay::run_replay(repo, &suite)?,
        RulesAction::Gate { base, head } => commands::replay::run_gate(repo, &base, &head)?,
        RulesAction::Review { approve, reject, reason } => run_rules_review(repo, approve, reject, reason)?,
        RulesAction::ShadowLog { rule_id } => run_rules_shadow_log(repo, &rule_id)?,
        RulesAction::Rollback { rule_id } => run_rules_rollback(repo, &rule_id)?,
        RulesAction::Packs { action } => run_packs(repo, action)?,
    }
    Ok(())
}

fn run_packs(repo: &std::path::Path, action: Option<PacksAction>) -> anyhow::Result<()> {
    match action {
        None => run_rules_packs(repo),
        Some(PacksAction::Add { source }) => run_rules_packs_add(repo, &source),
        Some(PacksAction::Validate { path }) => run_rules_packs_validate(std::path::Path::new(&path)),
        Some(PacksAction::Refresh) => run_rules_packs_refresh(repo),
    }
}

fn run_skills(repo: &std::path::Path, action: SkillsAction) -> anyhow::Result<()> {
    match action {
        SkillsAction::List => run_skills_list(repo)?,
        SkillsAction::Mine => run_skills_mine(repo)?,
        SkillsAction::Bench { aspect, proposal_id } => run_skills_bench(repo, &aspect, &proposal_id)?,
        SkillsAction::Review { approve, reject, reason } => run_skills_review(repo, approve, reject, reason)?,
        SkillsAction::Rollback { aspect, version } => run_skills_rollback(repo, &aspect, &version)?,
    }
    Ok(())
}

fn run_history(repo: &std::path::Path, action: HistoryAction) -> anyhow::Result<()> {
    match action {
        HistoryAction::Sync => run_history_sync(repo)?,
        HistoryAction::Costs { since } => run_history_costs(repo, since.as_deref())?,
    }
    Ok(())
}

fn run_spec(repo: &std::path::Path, action: SpecAction) -> anyhow::Result<()> {
    match action {
        SpecAction::Draft { base, head, force } => run_spec_draft(SpecDraftOptions { repo_root: repo.to_path_buf(), base_ref: base, head_ref: head, force })?,
    }
    Ok(())
}

fn run_auth(repo: &std::path::Path, action: AuthAction) -> anyhow::Result<()> {
    match action {
        AuthAction::Login { provider, email, token } => run_auth_login(repo, &provider, email, token)?,
        AuthAction::Status => run_auth_status()?,
        AuthAction::Logout { provider } => run_auth_logout(&provider)?,
    }
    Ok(())
}
