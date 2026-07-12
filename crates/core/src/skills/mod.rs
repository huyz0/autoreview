use std::path::Path;

use include_dir::{include_dir, Dir};

use autoreview_schema::{SkillManifest, Tier};

/// Builtin skills are embedded into the binary at compile time — the whole
/// point of shipping a single Rust executable is that it doesn't need a
/// side-by-side data directory installed to run.
static BUILTIN_SKILLS: Dir = include_dir!("$CARGO_MANIFEST_DIR/skills-builtin");

pub const OUTPUT_CONTRACT: &str = r#"
## Output contract

When you are done reviewing, end your final message with exactly one fenced
code block like this, and nothing after it:

```json
{
  "findings": [
    {
      "source": { "kind": "agent", "tool": "claude-code", "aspect": "<your aspect id>" },
      "category": "<category>",
      "severity": "blocker|high|medium|low|info",
      "confidence": 0.0,
      "title": "<one line, imperative>",
      "message": "<what, why it matters, evidence>",
      "location": {
        "path": "<repo-relative path>",
        "range": { "startLine": 1, "endLine": 1 },
        "snippet": "<the flagged lines, verbatim>",
        "side": "new"
      },
      "suggestion": {
        "description": "<what to change>",
        "patch": "<unified diff, optional>",
        "safety": "safe-autofix|needs-review"
      }
    }
  ]
}
```

If you found nothing, emit `{"findings": []}`. Do not include findings
outside this JSON block — prose explanation belongs before the block, not
inside it. If your output fails to parse, you will be asked to re-emit only
the corrected JSON in a follow-up turn.
"#;

pub struct CompiledSkill {
    pub manifest: SkillManifest,
    pub system_prompt: String,
}

fn depth_filename(tier: Tier) -> Option<&'static str> {
    match tier {
        Tier::Quick => Some("depth/quick.md"),
        Tier::Deep => Some("depth/deep.md"),
        Tier::Standard => None,
    }
}

/// Where a skill's files are read from: embedded in the binary, or a
/// repo-local override under `.autoreview/skills/<id>/` (which shadows a
/// builtin of the same id).
enum SkillFiles {
    // include_dir's File::path() is relative to the embed root, not to a
    // given subdirectory, so lookups must go through BUILTIN_SKILLS with the
    // full "<id>/<relative>" path rather than a sub-Dir handle.
    Embedded(String),
    Disk(std::path::PathBuf),
}

impl SkillFiles {
    fn read(&self, relative: &str) -> Option<String> {
        match self {
            SkillFiles::Embedded(id) => BUILTIN_SKILLS.get_file(format!("{id}/{relative}")).and_then(|f| f.contents_utf8()).map(|s| s.to_string()),
            SkillFiles::Disk(base) => std::fs::read_to_string(base.join(relative)).ok(),
        }
    }
}

fn find_skill_files(repo_root: &Path, id: &str) -> Option<SkillFiles> {
    let disk_dir = repo_root.join(".autoreview").join("skills").join(id);
    if disk_dir.join("skill.yaml").exists() {
        return Some(SkillFiles::Disk(disk_dir));
    }
    BUILTIN_SKILLS.get_dir(id).map(|_| SkillFiles::Embedded(id.to_string()))
}

/// All skill ids visible to this repo: embedded builtins plus any repo-local
/// additions under `.autoreview/skills/`, deduplicated (repo-local wins).
pub fn discover_skill_ids(repo_root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = BUILTIN_SKILLS
        .dirs()
        .filter_map(|d| d.path().file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();

    let local_skills_dir = repo_root.join(".autoreview").join("skills");
    if let Ok(entries) = std::fs::read_dir(&local_skills_dir) {
        for entry in entries.flatten() {
            if entry.path().join("skill.yaml").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    if !ids.contains(&name.to_string()) {
                        ids.push(name.to_string());
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}

pub fn load_manifest(repo_root: &Path, id: &str) -> anyhow::Result<SkillManifest> {
    let files = find_skill_files(repo_root, id).ok_or_else(|| anyhow::anyhow!("skill '{id}' not found (checked .autoreview/skills/ and builtins)"))?;
    let yaml = files.read("skill.yaml").ok_or_else(|| anyhow::anyhow!("skill '{id}' has no skill.yaml"))?;
    Ok(serde_yaml::from_str(&yaml)?)
}

pub fn discover_manifests(repo_root: &Path) -> anyhow::Result<Vec<SkillManifest>> {
    discover_skill_ids(repo_root).iter().map(|id| load_manifest(repo_root, id)).collect()
}

pub fn compile_skill(repo_root: &Path, id: &str, tier: Tier) -> anyhow::Result<CompiledSkill> {
    let files = find_skill_files(repo_root, id).ok_or_else(|| anyhow::anyhow!("skill '{id}' not found (checked .autoreview/skills/ and builtins)"))?;
    let yaml = files.read("skill.yaml").ok_or_else(|| anyhow::anyhow!("skill '{id}' has no skill.yaml"))?;
    let manifest: SkillManifest = serde_yaml::from_str(&yaml)?;

    let instructions = files.read("instructions.md").ok_or_else(|| anyhow::anyhow!("skill '{id}' has no instructions.md"))?;

    let mut system_prompt = instructions;
    if let Some(depth_file) = depth_filename(tier) {
        if let Some(overlay) = files.read(depth_file) {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&overlay);
        }
    }
    system_prompt.push_str("\n\n");
    system_prompt.push_str(OUTPUT_CONTRACT);

    Ok(CompiledSkill { manifest, system_prompt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_all_three_builtin_skills() {
        let ids = discover_skill_ids(Path::new("/nonexistent-repo-root"));
        assert_eq!(ids, vec!["correctness".to_string(), "design".to_string(), "security".to_string()]);
    }

    #[test]
    fn loads_manifest_for_each_builtin_skill() {
        for id in ["correctness", "security", "design"] {
            let manifest = load_manifest(Path::new("/nonexistent-repo-root"), id).unwrap();
            assert_eq!(manifest.id, id);
        }
    }

    #[test]
    fn compiles_skill_with_quick_depth_overlay() {
        let compiled = compile_skill(Path::new("/nonexistent-repo-root"), "correctness", Tier::Quick).unwrap();
        assert!(compiled.system_prompt.contains("Quick-tier depth"));
        assert!(compiled.system_prompt.contains("Output contract"));
        assert!(!compiled.system_prompt.contains("Deep-tier depth"));
    }

    #[test]
    fn compiles_skill_with_deep_depth_overlay() {
        let compiled = compile_skill(Path::new("/nonexistent-repo-root"), "security", Tier::Deep).unwrap();
        assert!(compiled.system_prompt.contains("Deep-tier depth"));
        assert!(!compiled.system_prompt.contains("Quick-tier depth"));
    }

    #[test]
    fn standard_tier_has_no_depth_overlay() {
        let compiled = compile_skill(Path::new("/nonexistent-repo-root"), "design", Tier::Standard).unwrap();
        assert!(!compiled.system_prompt.contains("Quick-tier depth"));
        assert!(!compiled.system_prompt.contains("Deep-tier depth"));
    }

    #[test]
    fn unknown_skill_id_errors_clearly() {
        let err = load_manifest(Path::new("/nonexistent-repo-root"), "does-not-exist").unwrap_err();
        assert!(err.to_string().contains("does-not-exist"));
    }
}
