//! `autoreview explain <finding-id>` — "why did this fire," a
//! deliberately deterministic answer: looks the finding up in this repo's
//! recorded run reports, then resolves its source back to the exact rule
//! definition (builtin, a registered external pack, or a repo-local
//! shadow/promoted rule) it actually matched, or explains that it came
//! from an LLM specialist's own judgment rather than a fixed rule. No LLM
//! call here — the whole point is to show ground truth a human can verify
//! themselves, not another model's guess about its own output.

use std::path::Path;

use autoreview_schema::{Finding, FindingSourceKind};

use super::history::{find_finding_in_run_reports, history_dir_for};

fn print_finding_summary(finding: &Finding) {
    println!("[{:?}] {} ({})", finding.severity, finding.title, finding.id);
    println!("  category:    {}", finding.category);
    println!("  confidence:  {:.2}", finding.confidence);
    println!("  location:    {}:{}", finding.location.path, finding.location.range.start_line);
    println!("  message:     {}", finding.message);
    if let Some(related) = &finding.related_locations {
        println!("  related:");
        for loc in related {
            println!("    {}:{}", loc.path, loc.range.start_line);
        }
    }
}

/// Looks for `rule_id` first among the builtin/registered-pack rules
/// (`find_rule_definition`), then among this repo's own shadow/promoted
/// rules (`.autoreview/rules/{shadow,promoted}/*.yaml`) — the two places a
/// deterministic (`Analyzer`/`LearnedRule`-sourced) finding's rule
/// definition can actually live. Several analyzers set `rule_id` to a
/// check name that was never a YAML rule at all (an external linter's own
/// check code, or an autoreview analyzer whose logic lives directly in
/// Rust) — `tool` distinguishes that expected case from an actual gap
/// (a builtin/pack rule id that no longer resolves), so the two get
/// different, honest explanations instead of one generic "not found."
fn print_rule_definition(repo_root: &Path, tool: &str, rule_id: &str) {
    let configured_packs = autoreview_core::load_rule_packs_config(&autoreview_core::rule_packs_config_path(repo_root)).unwrap_or_default();
    let rule_packs_cache_root = autoreview_core::default_rule_packs_cache_root();
    let registered_packs: Vec<_> = autoreview_core::resolve_rule_packs(repo_root, &rule_packs_cache_root, &configured_packs).into_iter().filter_map(|(_, r)| r.ok()).collect();

    if let Some(def) = autoreview_core::find_rule_definition(rule_id, &registered_packs) {
        println!("\nRule definition ({}, kind: {}{}):", def.source_label, def.kind, if def.semantic { ", semantic: true" } else { "" });
        println!("---");
        print!("{}", def.yaml);
        if !def.yaml.ends_with('\n') {
            println!();
        }
        return;
    }

    for shadow_file in autoreview_core::discover_shadow_rule_files(repo_root) {
        let Ok(text) = std::fs::read_to_string(&shadow_file.path) else { continue };
        #[derive(serde::Deserialize)]
        struct IdOnly {
            id: String,
        }
        let Ok(parsed) = serde_yaml::from_str::<IdOnly>(&text) else { continue };
        if parsed.id != rule_id {
            continue;
        }
        println!("\nRule definition ({} rule, {}):", shadow_file.status, shadow_file.path.display());
        println!("---");
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        return;
    }

    if tool == "golangci-lint" || tool == "clippy" {
        println!("\n'{rule_id}' is {tool}'s own check, not one of autoreview's declarative rules — there's no YAML rule to show. See {tool}'s own documentation for what '{rule_id}' means.");
    } else if tool.starts_with("autoreview-") {
        println!("\nThis check's logic lives directly in autoreview's {tool} analyzer code (crates/core/src/analyzers/), not a declarative YAML rule file — there's no rule text to show beyond the message above.");
    } else {
        println!("\nNo rule definition for '{rule_id}' was found among builtin rules, registered rule packs, or this repo's .autoreview/rules/{{shadow,promoted}}/ — it may have been renamed or removed since this finding was reported.");
    }
}

pub fn run_explain(repo_root: &Path, finding_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let finding = match find_finding_in_run_reports(&history_dir, finding_id)? {
        Some(f) => f,
        None => {
            println!("No finding with id '{finding_id}' was found in this repo's recorded run reports at {}.", history_dir.display());
            println!("`explain` only works on a finding id printed by a previous `autoreview diff` run on this machine.");
            std::process::exit(1);
        }
    };

    print_finding_summary(&finding);

    match (finding.source.kind, &finding.source.rule_id) {
        (FindingSourceKind::Agent, _) => {
            println!(
                "\nThis finding came from an LLM specialist (aspect: {}, backend: {}), not a fixed rule — it's the model's own judgment call on this diff, not a match against declared pattern/taint/threshold logic. There's no rule text to show beyond the message above; use `autoreview feedback {finding_id} --fp` or `--tp` to record whether it held up.",
                finding.source.aspect.as_deref().unwrap_or("unknown"),
                finding.source.backend.as_deref().unwrap_or("unknown"),
            );
        }
        (_, Some(rule_id)) => {
            println!("\nSource: {} rule '{rule_id}' (tool: {})", if finding.source.kind == FindingSourceKind::LearnedRule { "learned" } else { "deterministic" }, finding.source.tool);
            print_rule_definition(repo_root, &finding.source.tool, rule_id);
        }
        (_, None) => {
            println!("\nSource: {} (tool: {}) — this analyzer has no per-finding rule id to look up; its logic lives directly in the analyzer's own code, not a declarative rule file.", finding.source.tool, finding.source.tool);
        }
    }

    if let Some(meta) = &finding.meta {
        if let Some(pack_id) = meta.get("rulePackId").and_then(|v| v.as_str()) {
            println!("\n(sourced from registered rule pack '{pack_id}')");
        }
    }

    Ok(())
}
