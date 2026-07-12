use std::path::Path;

use autoreview_core::discover_manifests;

/// Lists every skill visible to this repo — embedded builtins plus any
/// repo-local overrides under `.autoreview/skills/` (which shadow a builtin
/// of the same id). Unlike the other M1 command stubs, this one is fully
/// real: `discover_manifests`/`compile_skill` already exist for Stage 3, so
/// surfacing them as a standalone command costs nothing extra.
pub fn run_skills_list(repo_root: &Path) -> anyhow::Result<()> {
    let manifests = discover_manifests(repo_root)?;
    println!("Skills available in this repo:\n");
    for manifest in &manifests {
        let triggers = if manifest.triggers.always {
            "always".to_string()
        } else {
            let mut parts = Vec::new();
            if !manifest.triggers.globs.is_empty() {
                parts.push(format!("globs: {}", manifest.triggers.globs.join(", ")));
            }
            if !manifest.triggers.signals.is_empty() {
                parts.push(format!("signals: {}", manifest.triggers.signals.join(", ")));
            }
            if parts.is_empty() {
                "(none)".to_string()
            } else {
                parts.join("; ")
            }
        };
        println!("  {} (v{})", manifest.id, manifest.version);
        println!("    title:      {}", manifest.title);
        println!("    categories: {}", manifest.categories.join(", "));
        println!("    cost class: {:?}", manifest.cost_class);
        println!("    triggers:   {triggers}");
        println!();
    }
    Ok(())
}

pub fn run_skills_stub(action: &str) {
    println!("`autoreview skills {action}` is not implemented yet — planned for M3 (feedback-driven skill evolution + replay eval), per the project plan.");
    println!("Available today: `autoreview skills list`.");
}
