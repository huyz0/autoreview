use std::path::Path;
use std::process::Command;

use globset::Glob;

use autoreview_schema::ContextProviderConfig;

use crate::triage::signals::DiffFacts;

#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    pub label: String,
    pub content: String,
}

/// Per-item and total caps on how much context gets pushed into a
/// specialist's prompt up front — this is a token-cost control, not a
/// correctness one, so it's deliberately simple (char count as a proxy).
const MAX_ITEM_CHARS: usize = 4_000;
const MAX_TOTAL_CHARS: usize = 12_000;

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}\n... [truncated]", s.chars().take(max_chars).collect::<String>())
    }
}

/// Zero-setup defaults: auto-discovered without any `.autoreview/config.yaml`
/// at all, per the plan ("docs (zero setup): auto-discover CLAUDE.md,
/// CONTRIBUTING.md, docs/adr/**, style guides + .autoreview/context/**").
fn default_providers() -> Vec<ContextProviderConfig> {
    vec![
        ContextProviderConfig::Docs {
            paths: vec!["CLAUDE.md".to_string(), "CONTRIBUTING.md".to_string(), "docs/adr/**".to_string(), ".autoreview/context/**".to_string()],
        },
        ContextProviderConfig::GitHistory,
    ]
}

fn collect_docs(repo_root: &Path, glob_patterns: &[String]) -> Vec<ContextItem> {
    let mut items = Vec::new();
    let Ok(canonical_repo_root) = repo_root.canonicalize() else { return items };
    for pattern in glob_patterns {
        // Plain filenames (no glob metacharacters) are checked directly so a
        // bare "CLAUDE.md" doesn't need glob-walking the whole tree.
        if !pattern.contains(['*', '?', '[']) {
            let path = repo_root.join(pattern);
            // `pattern` comes from repo-local config (`.autoreview/config.yaml`),
            // which — in a code-review tool that processes untrusted PRs — can
            // itself be attacker-controlled diff content. Without this check a
            // `../../etc/passwd`-style path would read files outside the repo
            // straight into an LLM prompt.
            let Ok(canonical) = path.canonicalize() else { continue };
            if !canonical.starts_with(&canonical_repo_root) {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(&canonical) {
                items.push(ContextItem { label: pattern.clone(), content: truncate(&contents, MAX_ITEM_CHARS) });
            }
            continue;
        }

        let Ok(glob) = Glob::new(pattern) else { continue };
        let matcher = glob.compile_matcher();
        for entry in walk_files(repo_root) {
            let Ok(relative) = entry.strip_prefix(repo_root) else { continue };
            let relative_str = relative.to_string_lossy().replace('\\', "/");
            if matcher.is_match(&relative_str) {
                if let Ok(contents) = std::fs::read_to_string(&entry) {
                    items.push(ContextItem { label: relative_str, content: truncate(&contents, MAX_ITEM_CHARS) });
                }
            }
        }
    }
    items
}

/// A small, dependency-free recursive file walk — this module only needs to
/// glob-match a handful of doc-shaped directories, not general-purpose
/// gitignore-aware traversal (that's the analyzers' concern, via the tools
/// they shell out to).
fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
    const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "vendor"];
    let mut results = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return results };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
            }
            results.extend(walk_files(&path));
        } else {
            results.push(path);
        }
    }
    results
}

fn collect_git_history(repo_root: &Path, facts: &DiffFacts) -> Vec<ContextItem> {
    if facts.files.is_empty() {
        return vec![];
    }
    let mut args = vec!["log".to_string(), "--oneline".to_string(), "-n".to_string(), "8".to_string(), "--".to_string()];
    args.extend(facts.files.iter().map(|f| f.path.clone()));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = Command::new("git").args(&arg_refs).current_dir(repo_root).output();
    match output {
        Ok(output) if output.status.success() => {
            let log = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if log.is_empty() {
                vec![]
            } else {
                vec![ContextItem {
                    label: "recent commit history touching these files".to_string(),
                    content: truncate(&log, MAX_ITEM_CHARS),
                }]
            }
        }
        _ => vec![],
    }
}

/// Runs the configured (or, absent any config, the zero-setup default)
/// context providers and returns items capped to a total budget — the
/// M1 rollout only implements `docs` and `git-history`; `command`/`mcp`
/// providers are recognized in config (M2/M3) but silently skipped here
/// rather than erroring, since specifying one today is a forward-compatible
/// no-op, not a mistake.
pub fn collect_context(repo_root: &Path, facts: &DiffFacts, configured: &[ContextProviderConfig]) -> Vec<ContextItem> {
    let providers: Vec<ContextProviderConfig> = if configured.is_empty() { default_providers() } else { configured.to_vec() };

    let mut items = Vec::new();
    for provider in &providers {
        match provider {
            ContextProviderConfig::Docs { paths } => items.extend(collect_docs(repo_root, paths)),
            ContextProviderConfig::GitHistory => items.extend(collect_git_history(repo_root, facts)),
            ContextProviderConfig::Command { .. } | ContextProviderConfig::Mcp { .. } => {
                // Not implemented until M2 (command) / M3 (mcp) per the plan's rollout.
            }
        }
    }

    let mut total = 0usize;
    let mut budgeted = Vec::new();
    for item in items {
        let len = item.content.chars().count();
        if total + len > MAX_TOTAL_CHARS {
            break;
        }
        total += len;
        budgeted.push(item);
    }
    budgeted
}

/// Renders context items as a prompt-ready block, or an explicit "(none)"
/// marker — matching how Stage-1 findings are presented to specialists, so
/// the compiled prompt never has a silently-empty section.
pub fn render_context_block(items: &[ContextItem]) -> String {
    if items.is_empty() {
        return "(no additional project context found)".to_string();
    }
    items.iter().map(|item| format!("### {}\n{}", item.label, item.content)).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::signals::FileChange;

    fn make_facts(files: Vec<FileChange>) -> DiffFacts {
        DiffFacts {
            repo_root: "/repo".into(),
            base_ref: "main~1".into(),
            head_ref: "main".into(),
            files,
            languages: Default::default(),
            sensitive_path_hit: false,
            sensitive_paths: vec![],
            dependency_change: false,
            ci_or_infra_change: false,
            tests_touched: false,
            source_touched_without_tests: false,
            added_branch_keywords: 0,
        }
    }

    #[test]
    fn discovers_claude_md_by_exact_filename() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Project conventions: use constructor injection.").unwrap();
        let facts = make_facts(vec![]);
        let items = collect_context(dir.path(), &facts, &[]);
        assert!(items.iter().any(|i| i.label == "CLAUDE.md" && i.content.contains("constructor injection")));
    }

    #[test]
    fn discovers_docs_adr_via_glob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(dir.path().join("docs/adr/0001-use-postgres.md"), "We chose Postgres because...").unwrap();
        let facts = make_facts(vec![]);
        let items = collect_context(dir.path(), &facts, &[]);
        assert!(items.iter().any(|i| i.content.contains("Postgres")));
    }

    #[test]
    fn skips_directories_that_should_never_be_walked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/some-pkg/docs/adr")).unwrap();
        std::fs::write(dir.path().join("node_modules/some-pkg/docs/adr/x.md"), "should not be found").unwrap();
        let facts = make_facts(vec![]);
        let items = collect_context(dir.path(), &facts, &[ContextProviderConfig::Docs { paths: vec!["**/adr/**".to_string()] }]);
        assert!(items.is_empty(), "should not have descended into node_modules");
    }

    #[test]
    fn returns_no_items_when_nothing_matches_and_no_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        let facts = make_facts(vec![]);
        let items = collect_context(dir.path(), &facts, &[]);
        assert!(items.is_empty());
        assert_eq!(render_context_block(&items), "(no additional project context found)");
    }

    #[test]
    fn command_and_mcp_providers_are_recognized_but_not_yet_implemented() {
        let dir = tempfile::tempdir().unwrap();
        let facts = make_facts(vec![]);
        let providers = vec![
            ContextProviderConfig::Command { run: "./scripts/similar-code.sh".to_string(), input: "changed-files".to_string() },
            ContextProviderConfig::Mcp { server: "jira".to_string(), use_for: vec![] },
        ];
        let items = collect_context(dir.path(), &facts, &providers);
        assert!(items.is_empty(), "command/mcp providers should be a no-op in M1, not an error");
    }

    #[test]
    fn caps_total_context_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "a".repeat(MAX_TOTAL_CHARS + 5_000)).unwrap();
        let facts = make_facts(vec![]);
        let items = collect_context(dir.path(), &facts, &[]);
        let total: usize = items.iter().map(|i| i.content.chars().count()).sum();
        assert!(total <= MAX_TOTAL_CHARS);
    }
}
