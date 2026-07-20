mod commands;
mod render;

use clap::{Parser, Subcommand};

use autoreview_schema::{AgentBackendKind, Tier};
use commands::apply::run_apply;
use commands::diff::{run_diff, DiffCommandOptions};
use commands::doctor::run_doctor;
use commands::feedback::{run_feedback, run_missed_report};
use commands::history::run_history_sync;
use commands::rules::{run_rules_bench, run_rules_mine, run_rules_mine_comments, run_rules_packs, run_rules_review, run_rules_shadow_log};
use commands::skills::{run_skills_list, run_skills_mine, run_skills_review, run_skills_stub};
use commands::skills_bench::run_skills_bench;
use commands::stubs::run_rules_stub;

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
    },
    /// Apply a finding's suggested patch, gated by a `git apply --check` sanity check
    Apply { finding_id: String },
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
    /// Manage the learned-rule factory (mine is implemented; draft/bench/shadow/promote planned M3)
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Manage review skills (list and mine are implemented; bench/review/rollback planned M3)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Manage local run/event history
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },
}

#[derive(Subcommand)]
enum HistoryAction {
    /// Pull the team's synced event log (storage.sync.mode: git) onto this machine
    Sync,
}

#[derive(Subcommand)]
enum RulesAction {
    /// Mine recurring agent findings into rule candidates. With
    /// --from-comments, mines recurring human PR review comments via `gh`
    /// instead (opt-in — see mineFromComments in .autoreview/config.yaml)
    Mine {
        #[arg(long)]
        from_comments: bool,
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
    /// how many rules of each kind each one resolved to
    Packs,
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
        Commands::Diff { base, head, tier, aspects, max_usd, incremental, backend } => {
            run_diff(DiffCommandOptions {
                repo_root,
                base_ref: base,
                head_ref: head,
                tier: tier.and_then(|t| parse_tier(&t)),
                aspects: aspects.map(|a| a.split(',').map(|s| s.to_string()).collect()),
                max_usd,
                incremental,
                backend: backend.and_then(|b| parse_backend(&b)),
            })?;
        }
        Commands::Apply { finding_id } => run_apply(&repo_root, &finding_id)?,
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
            RulesAction::Mine { from_comments } => {
                if from_comments {
                    run_rules_mine_comments(&repo_root)?
                } else {
                    run_rules_mine(&repo_root)?
                }
            }
            RulesAction::Bench { cluster_id } => run_rules_bench(&repo_root, &cluster_id)?,
            RulesAction::Review { approve, reject, reason } => run_rules_review(&repo_root, approve, reject, reason)?,
            RulesAction::ShadowLog { rule_id } => run_rules_shadow_log(&repo_root, &rule_id)?,
            RulesAction::Rollback { rule_id } => run_rules_stub(&format!("rollback {rule_id}")),
            RulesAction::Packs => run_rules_packs(&repo_root)?,
        },
        Commands::Skills { action } => match action {
            SkillsAction::List => run_skills_list(&repo_root)?,
            SkillsAction::Mine => run_skills_mine(&repo_root)?,
            SkillsAction::Bench { aspect, proposal_id } => run_skills_bench(&repo_root, &aspect, &proposal_id)?,
            SkillsAction::Review { approve, reject, reason } => run_skills_review(&repo_root, approve, reject, reason)?,
            SkillsAction::Rollback { aspect, version } => run_skills_stub(&format!("rollback {aspect} {version}")),
        },
        Commands::History { action } => match action {
            HistoryAction::Sync => run_history_sync(&repo_root)?,
        },
    }

    Ok(())
}
