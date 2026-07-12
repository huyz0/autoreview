use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone)]
pub struct DiffFacts {
    pub repo_root: String,
    pub base_ref: String,
    pub head_ref: String,
    pub files: Vec<FileChange>,
    pub languages: HashMap<String, u32>,
    pub sensitive_path_hit: bool,
    pub sensitive_paths: Vec<String>,
    pub dependency_change: bool,
    pub ci_or_infra_change: bool,
    pub tests_touched: bool,
    pub source_touched_without_tests: bool,
    pub added_branch_keywords: u32,
}

const DEPENDENCY_MANIFESTS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.mod",
    "go.sum",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Cargo.toml",
    "Cargo.lock",
    "requirements.txt",
    "pyproject.toml",
];

const CI_INFRA_GLOBS: &[&str] = &[".github/workflows/**", "**/Dockerfile", "**/*.tf", ".gitlab-ci.yml"];

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.py")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
        || path.ends_with("Test.java")
        || path.ends_with("Test.kt")
}

fn extension_language(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?;
    Some(match ext {
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "go" => "go",
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        _ => return None,
    })
}

fn build_globset(patterns: &[&str]) -> globset::GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
}

fn build_globset_owned(patterns: &[String]) -> globset::GlobSet {
    let refs: Vec<&str> = patterns.iter().map(|s| s.as_str()).collect();
    build_globset(&refs)
}

fn git_diff_numstat(repo_root: &str, base_ref: &str, head_ref: &str) -> anyhow::Result<Vec<FileChange>> {
    let output = Command::new("git")
        .args(["diff", "--numstat", &format!("{base_ref}...{head_ref}")])
        .current_dir(repo_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("git diff --numstat failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(add_str), Some(del_str), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let additions = if add_str == "-" { 0 } else { add_str.parse().unwrap_or(0) };
        let deletions = if del_str == "-" { 0 } else { del_str.parse().unwrap_or(0) };
        changes.push(FileChange { path: path.to_string(), additions, deletions });
    }
    Ok(changes)
}

fn git_added_lines(repo_root: &str, base_ref: &str, head_ref: &str) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["diff", "--unified=0", &format!("{base_ref}...{head_ref}")])
        .current_dir(repo_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("git diff --unified=0 failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let added: Vec<&str> = stdout.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).collect();
    Ok(added.join("\n"))
}

fn count_branch_keywords(added_lines: &str) -> u32 {
    let keywords = ["if", "else if", "for", "while", "case", "catch"];
    let mut count = 0u32;
    for line in added_lines.lines() {
        for kw in keywords {
            count += line.matches(kw).count() as u32;
        }
        count += line.matches("&&").count() as u32;
        count += line.matches("||").count() as u32;
    }
    count
}

pub fn collect_diff_facts(
    repo_root: &str,
    base_ref: &str,
    head_ref: &str,
    sensitive_path_globs: Option<&[String]>,
) -> anyhow::Result<DiffFacts> {
    let default_sensitive = autoreview_schema::default_sensitive_paths();
    let sensitive_globs_owned;
    let sensitive_globs: &[String] = match sensitive_path_globs {
        Some(g) => g,
        None => {
            sensitive_globs_owned = default_sensitive;
            &sensitive_globs_owned
        }
    };

    let files = git_diff_numstat(repo_root, base_ref, head_ref)?;
    let added_lines = git_added_lines(repo_root, base_ref, head_ref)?;

    let mut languages: HashMap<String, u32> = HashMap::new();
    for file in &files {
        if let Some(lang) = extension_language(&file.path) {
            *languages.entry(lang.to_string()).or_insert(0) += 1;
        }
    }

    let sensitive_set = build_globset_owned(sensitive_globs);
    let sensitive_paths: Vec<String> = files.iter().map(|f| f.path.clone()).filter(|p| sensitive_set.is_match(p)).collect();

    let dep_manifest_names: std::collections::HashSet<&str> = DEPENDENCY_MANIFESTS.iter().copied().collect();
    let dependency_change = files.iter().any(|f| {
        Path::new(&f.path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|name| dep_manifest_names.contains(name))
            .unwrap_or(false)
    });

    let ci_infra_set = build_globset(CI_INFRA_GLOBS);
    let ci_or_infra_change = files.iter().any(|f| ci_infra_set.is_match(&f.path));

    let tests_touched = files.iter().any(|f| is_test_path(&f.path));
    let source_touched = files.iter().any(|f| !is_test_path(&f.path));

    Ok(DiffFacts {
        repo_root: repo_root.to_string(),
        base_ref: base_ref.to_string(),
        head_ref: head_ref.to_string(),
        sensitive_path_hit: !sensitive_paths.is_empty(),
        sensitive_paths,
        dependency_change,
        ci_or_infra_change,
        tests_touched,
        source_touched_without_tests: source_touched && !tests_touched,
        added_branch_keywords: count_branch_keywords(&added_lines),
        files,
        languages,
    })
}
