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

/// Analyzers whose findings are never backed by a declarative YAML rule
/// file — their check logic is plain Rust, so a missing `find_rule_
/// definition`/shadow-file match for one of these is expected, not a gap.
/// Deliberately NOT a blanket `tool.starts_with("autoreview-")` check:
/// `autoreview-dataflow` (taint rules), `autoreview-complexity` (threshold
/// rules), and `autoreview-shadow-rule` (shadow/promoted rules) all share
/// that prefix but ARE genuinely YAML-backed — for those, a failed lookup
/// really does mean the rule was renamed/removed, and saying otherwise
/// would be a false explanation from the one command whose whole job is
/// truthful ones.
const CODE_ONLY_ANALYZER_TOOLS: &[&str] = &["autoreview-duplication", "autoreview-practices", "autoreview-architecture", "autoreview-archgraph", "autoreview-symindex", "autoreview-churn"];

fn print_definition_text(header_detail: &str, text: &str) {
    println!("\nRule definition ({header_detail}):");
    println!("---");
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
}

/// Looks for `rule_id` first among builtin rules (no I/O beyond reading the
/// embedded rule tree already compiled into the binary), then — only if
/// that misses — resolves registered rule packs (which may re-fetch a
/// git-source pack over the network) and searches those, then finally
/// this repo's own shadow/promoted rules
/// (`.autoreview/rules/{shadow,promoted}/*.yaml`). Deferring pack
/// resolution until after the free builtin check means explaining a
/// builtin-rule finding (the common case) never pays for a git pack
/// fetch it doesn't need.
fn print_rule_definition(repo_root: &Path, tool: &str, rule_id: &str) {
    if let Some(def) = autoreview_core::find_rule_definition(rule_id, &[]) {
        print_definition_text(&format!("{}, kind: {}{}", def.source_label, def.kind, if def.semantic { ", semantic: true" } else { "" }), &def.yaml);
        return;
    }

    let configured_packs = autoreview_core::load_rule_packs_config(&autoreview_core::rule_packs_config_path(repo_root)).unwrap_or_default();
    let rule_packs_cache_root = autoreview_core::default_rule_packs_cache_root();
    let registered_packs: Vec<_> = autoreview_core::resolve_rule_packs(repo_root, &rule_packs_cache_root, &configured_packs).into_iter().filter_map(|(_, r)| r.ok()).collect();

    if let Some(def) = autoreview_core::find_rule_definition(rule_id, &registered_packs) {
        print_definition_text(&format!("{}, kind: {}{}", def.source_label, def.kind, if def.semantic { ", semantic: true" } else { "" }), &def.yaml);
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
        print_definition_text(&format!("{} rule, {}", shadow_file.status, shadow_file.path.display()), &text);
        return;
    }

    println!("\n{}", no_definition_found_message(tool, rule_id));
}

/// The message printed when neither a builtin/pack rule nor a shadow/
/// promoted rule file matches `rule_id` — a pure function of `tool`/
/// `rule_id` so the tool-classification logic (which of the two possible
/// explanations applies) is unit-testable without the file-system/network
/// I/O the rest of `print_rule_definition` does.
fn no_definition_found_message(tool: &str, rule_id: &str) -> String {
    if tool == "golangci-lint" || tool == "clippy" {
        format!("'{rule_id}' is {tool}'s own check, not one of autoreview's declarative rules — there's no YAML rule to show. See {tool}'s own documentation for what '{rule_id}' means.")
    } else if CODE_ONLY_ANALYZER_TOOLS.contains(&tool) {
        format!("This check's logic lives directly in autoreview's {tool} analyzer code (crates/core/src/analyzers/), not a declarative YAML rule file — there's no rule text to show beyond the message above.")
    } else {
        format!("No rule definition for '{rule_id}' was found among builtin rules, registered rule packs, or this repo's .autoreview/rules/{{shadow,promoted}}/ — it may have been renamed or removed since this finding was reported.")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_yaml_backed_tool_whose_rule_no_longer_resolves_gets_the_renamed_or_removed_message() {
        // Regression test: `autoreview-dataflow` (taint rules),
        // `autoreview-complexity` (threshold rules), and
        // `autoreview-shadow-rule` (shadow/promoted rules) are all
        // genuinely YAML-backed despite sharing the "autoreview-" prefix
        // with the Rust-only analyzers — a blanket `starts_with("autoreview-")`
        // check previously misclassified them as code-only, wrongly
        // claiming their (real, just-missing) YAML rule didn't exist.
        for tool in ["autoreview-dataflow", "autoreview-complexity", "autoreview-shadow-rule"] {
            let message = no_definition_found_message(tool, "some-removed-rule-id");
            assert!(message.contains("may have been renamed or removed"), "tool '{tool}' got the wrong message: {message}");
            assert!(!message.contains("lives directly in"), "tool '{tool}' is YAML-backed and must not get the code-only message: {message}");
        }
    }

    #[test]
    fn a_genuinely_code_only_analyzer_gets_the_code_only_message() {
        for tool in CODE_ONLY_ANALYZER_TOOLS {
            let message = no_definition_found_message(tool, "some-rule-id");
            assert!(message.contains("lives directly in"), "tool '{tool}' got the wrong message: {message}");
        }
    }

    #[test]
    fn an_external_linter_gets_pointed_at_its_own_docs() {
        for tool in ["golangci-lint", "clippy"] {
            let message = no_definition_found_message(tool, "some-check");
            assert!(message.contains("own check"), "tool '{tool}' got the wrong message: {message}");
        }
    }
}
