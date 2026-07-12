mod commands;
mod render;

use clap::{Parser, Subcommand};

use autoreview_schema::Tier;
use commands::apply::run_apply;
use commands::diff::{run_diff, DiffCommandOptions};
use commands::doctor::run_doctor;
use commands::feedback::run_feedback;
use commands::skills::{run_skills_list, run_skills_stub};
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
    },
    /// Apply a finding's suggested patch, gated by a `git apply --check` sanity check
    Apply { finding_id: String },
    /// Record true/false-positive feedback on a finding, or report a missed finding
    Feedback {
        id: String,
        #[arg(long)]
        fp: bool,
        #[arg(long)]
        tp: bool,
        #[arg(long)]
        missed: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Manage the learned-rule factory (not yet implemented — planned M3)
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Manage review skills (list is implemented; mining/eval planned M3)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
}

#[derive(Subcommand)]
enum RulesAction {
    /// Mine recurring agent findings into rule candidates
    Mine,
    /// Bench a candidate rule against its self-test + historical precision
    Bench { cluster_id: String },
    /// Human review queue for candidate rules
    Review,
    /// Inspect recent firings of a shadow-mode rule
    ShadowLog { rule_id: String },
    /// Roll back a promoted/shadow rule to its prior state
    Rollback { rule_id: String },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// List all skills visible to this repo (builtins + repo-local overrides)
    List,
    /// Mine feedback into skill-edit proposals
    Mine,
    /// Replay-bench a skill-edit proposal against the history store
    Bench { aspect: String, proposal_id: String },
    /// Human review queue for skill-edit proposals
    Review,
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    match cli.command {
        Commands::Doctor => {
            run_doctor(&repo_root);
        }
        Commands::Diff { base, head, tier, aspects, max_usd, incremental } => {
            run_diff(DiffCommandOptions {
                repo_root,
                base_ref: base,
                head_ref: head,
                tier: tier.and_then(|t| parse_tier(&t)),
                aspects: aspects.map(|a| a.split(',').map(|s| s.to_string()).collect()),
                max_usd,
                incremental,
            })?;
        }
        Commands::Apply { finding_id } => run_apply(&repo_root, &finding_id)?,
        Commands::Feedback { id, fp, tp, missed, note } => {
            if let Some(description) = missed {
                run_feedback(&repo_root, &id, "missed", Some(&description))?;
            } else if fp {
                run_feedback(&repo_root, &id, "fp", note.as_deref())?;
            } else if tp {
                run_feedback(&repo_root, &id, "tp", note.as_deref())?;
            } else {
                eprintln!("error: specify one of --fp, --tp, or --missed <description>");
                std::process::exit(1);
            }
        }
        Commands::Rules { action } => {
            let label = match action {
                RulesAction::Mine => "mine".to_string(),
                RulesAction::Bench { cluster_id } => format!("bench {cluster_id}"),
                RulesAction::Review => "review".to_string(),
                RulesAction::ShadowLog { rule_id } => format!("shadow-log {rule_id}"),
                RulesAction::Rollback { rule_id } => format!("rollback {rule_id}"),
            };
            run_rules_stub(&label);
        }
        Commands::Skills { action } => match action {
            SkillsAction::List => run_skills_list(&repo_root)?,
            SkillsAction::Mine => run_skills_stub("mine"),
            SkillsAction::Bench { aspect, proposal_id } => run_skills_stub(&format!("bench {aspect} {proposal_id}")),
            SkillsAction::Review => run_skills_stub("review"),
            SkillsAction::Rollback { aspect, version } => run_skills_stub(&format!("rollback {aspect} {version}")),
        },
    }

    Ok(())
}
