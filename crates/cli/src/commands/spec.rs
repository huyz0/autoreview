//! `autoreview spec draft` — has the configured agent backend write a first
//! draft of `.autoreview/spec.md` from the base...head diff, so a human
//! starts from a shaped scaffold (title/intent/acceptance criteria) instead
//! of a blank page. The draft is written to disk as-is for the human to
//! edit; nothing here validates it against `parse_spec` or runs
//! `run_spec_verify` against it — that happens on the next `autoreview
//! diff` once the human is satisfied with the file.

use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_core::{draft_spec, load_config};

use super::backend::{backend_available, backend_label, build_backend};

pub struct SpecDraftOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    pub force: bool,
}

fn full_diff_text(repo_root: &Path, base_ref: &str, head_ref: &str) -> anyhow::Result<String> {
    let output = Command::new("git").args(["diff", &format!("{base_ref}...{head_ref}")]).current_dir(repo_root).output()?;
    if !output.status.success() {
        anyhow::bail!("git diff {base_ref}...{head_ref} failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        anyhow::bail!("git diff {base_ref}...{head_ref} is empty — nothing to draft a spec from");
    }
    Ok(text)
}

pub fn run_spec_draft(options: SpecDraftOptions) -> anyhow::Result<()> {
    let spec_path = options.repo_root.join(".autoreview").join("spec.md");
    if spec_path.exists() && !options.force {
        anyhow::bail!("{} already exists — pass --force to overwrite it", spec_path.display());
    }

    let config = load_config(&options.repo_root.join(".autoreview").join("config.yaml"))?;
    let backend_kind = config.agents.backend;
    if !backend_available(backend_kind, &config) {
        anyhow::bail!("agent backend '{}' is not available (autoreview doctor can confirm what's installed)", backend_label(backend_kind));
    }

    let diff_text = full_diff_text(&options.repo_root, &options.base_ref, &options.head_ref)?;
    let backend = build_backend(backend_kind, &config);
    let model = config.budgets.models.standard.clone();

    println!("Drafting spec from {}...{} via {} ({model})...", options.base_ref, options.head_ref, backend_label(backend_kind));
    let drafted = draft_spec(backend.as_ref(), &model, &diff_text, &options.repo_root)?;

    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&spec_path, format!("{}\n", drafted.markdown.trim_end()))?;
    println!("Wrote {} — review and edit before your next `autoreview diff` run.", spec_path.display());
    Ok(())
}
