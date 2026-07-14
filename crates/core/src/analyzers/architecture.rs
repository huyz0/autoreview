//! Track 1 Tier 1 of the rule-pack expansion plan: declarative import/layer
//! rules from `.autoreview/architecture.yaml`, inspired by Python's
//! `import-linter` and JS's `dependency-cruiser` — both prove layer rules
//! don't strictly need a full dependency graph, just import-statement
//! inspection per file. Catches *direct* layer violations (file A imports
//! something in a forbidden layer); it cannot see transitive ones
//! (A -> B -> C where only A -> C is forbidden) — that's exactly what
//! Tier 2 (archgraph, a real cross-file dependency graph) exists for, and
//! isn't built yet. Every finding this module emits says so explicitly, so
//! a clean report never gets mistaken for an exhaustive one.
//!
//! Opt-in: no `architecture.yaml` means no layers are defined, so nothing
//! is ever flagged — there's no sane generic default for what a repo's
//! layers are.

use std::path::Path;

use globset::Glob;

use autoreview_schema::{AgentFinding, ArchitectureConfig, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// Reads and parses `.autoreview/architecture.yaml`. Returns `None` (not an
/// error) when the file doesn't exist — this feature is opt-in.
pub fn load_architecture_config(path: &Path) -> anyhow::Result<Option<ArchitectureConfig>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let file: autoreview_schema::ArchitectureFile = serde_yaml::from_str(&contents)?;
            Ok(Some(file.architecture))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Resolves which declared layer a path belongs to (file path or,
/// normalized, an import target) — first matching layer in declaration
/// order wins. Returns `None` if no layer's globs match.
fn layer_for_path<'a>(path: &str, layers: &'a [autoreview_schema::ArchitectureLayer]) -> Option<&'a str> {
    layers.iter().find_map(|layer| {
        layer.match_globs.iter().any(|pattern| Glob::new(pattern).map(|g| g.compile_matcher().is_match(path)).unwrap_or(false)).then(|| layer.name.as_str())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportLanguage {
    Go,
    JavaOrKotlin,
}

fn language_for_file(path: &str) -> Option<ImportLanguage> {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("go") => Some(ImportLanguage::Go),
        Some("java") | Some("kt") | Some("kts") => Some(ImportLanguage::JavaOrKotlin),
        _ => None,
    }
}

/// Extracts `(line_number, import_path)` pairs from source text — a plain
/// line scan, not a real parse, which is enough for import statements
/// specifically (no rule here needs to understand the rest of the file).
fn extract_imports(content: &str, language: ImportLanguage) -> Vec<(u32, String)> {
    let mut imports = Vec::new();
    let mut in_go_import_block = false;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let line = raw_line.trim();

        match language {
            ImportLanguage::Go => {
                if let Some(rest) = line.strip_prefix("import ") {
                    let rest = rest.trim();
                    if rest == "(" {
                        in_go_import_block = true;
                        continue;
                    }
                    if let Some(path) = extract_quoted(rest) {
                        imports.push((line_no, path));
                    }
                    continue;
                }
                if in_go_import_block {
                    if line == ")" {
                        in_go_import_block = false;
                        continue;
                    }
                    if let Some(path) = extract_quoted(line) {
                        imports.push((line_no, path));
                    }
                }
            }
            ImportLanguage::JavaOrKotlin => {
                let rest = line.strip_prefix("import ").map(|r| r.strip_prefix("static ").unwrap_or(r));
                if let Some(rest) = rest {
                    let path = rest.trim_end_matches(';').trim();
                    if !path.is_empty() {
                        imports.push((line_no, path.to_string()));
                    }
                }
            }
        }
    }

    imports
}

fn extract_quoted(s: &str) -> Option<String> {
    let s = s.trim();
    let start = s.find('"')?;
    let end = s[start + 1..].find('"')? + start + 1;
    Some(s[start + 1..end].to_string())
}

/// Normalizes an import path to the same slash-separated form file paths
/// use, so both can be matched against the same layer globs: Go imports are
/// already slash-separated; Java/Kotlin dotted package paths (`com.example.
/// repository.UserRepo`) become `com/example/repository/UserRepo`. This is
/// an approximation (package names don't always mirror directory layout in
/// unusual build setups) but holds for the overwhelming common case.
///
/// A trailing synthetic segment is always appended: layer globs are written
/// against file paths (`**/repository/**`, expecting something *after* the
/// directory name), but an import path names a package/class with nothing
/// trailing it (`myapp/internal/repository` has no file component) — without
/// this, a bare package-directory import would never match a glob that
/// requires a trailing segment, silently missing every violation. Verified
/// empirically: `**/repository/**` does not match the literal string
/// `myapp/internal/repository`.
fn normalize_import(import_path: &str, language: ImportLanguage) -> String {
    let slashed = match language {
        ImportLanguage::Go => import_path.to_string(),
        ImportLanguage::JavaOrKotlin => import_path.replace('.', "/"),
    };
    format!("{slashed}/_")
}

/// Checks each changed file against `architecture.yaml`'s layer rules:
/// resolves the file's own layer, and flags any import whose normalized
/// path resolves to a layer that layer is forbidden from depending on.
pub fn run_architecture_check(repo_root: &Path, changed_files: &[String], config: &ArchitectureConfig) -> Vec<AgentFinding> {
    if config.layers.is_empty() || config.rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for file in changed_files {
        let Some(language) = language_for_file(file) else { continue };
        let Some(file_layer) = layer_for_path(file, &config.layers) else { continue };
        let forbidden_targets: Vec<&str> = config.rules.iter().filter(|r| r.forbid.from == file_layer).flat_map(|r| r.forbid.to.iter().map(String::as_str)).collect();
        if forbidden_targets.is_empty() {
            continue;
        }

        let full_path = repo_root.join(file);
        let Ok(content) = std::fs::read_to_string(&full_path) else { continue };

        for (line_no, import_path) in extract_imports(&content, language) {
            let normalized = normalize_import(&import_path, language);
            let Some(import_layer) = layer_for_path(&normalized, &config.layers) else { continue };
            if !forbidden_targets.contains(&import_layer) {
                continue;
            }

            findings.push(AgentFinding {
                source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-architecture".to_string(), rule_id: Some("layer-violation".to_string()), aspect: None, backend: None },
                category: "architecture".to_string(),
                severity: Severity::High,
                confidence: 1.0,
                title: format!("Layer violation: {file_layer} -> {import_layer}"),
                message: format!(
                    "{file_layer} imports `{import_path}`, which resolves to the {import_layer} layer — {file_layer} is configured to never depend on {import_layer} in architecture.yaml. \
                    (Direct-import check only: this doesn't see transitive violations through an intermediate file — that needs a full dependency graph, not yet available.)"
                ),
                location: Location { path: file.clone(), range: LocationRange { start_line: line_no, ..Default::default() }, snippet: import_path.clone(), side: Side::New },
                related_locations: None,
                suggestion: None,
                tags: None,
                meta: None,
                suggested_patch: None,
            });
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{ArchitectureForbidRule, ArchitectureLayer, ArchitectureRuleEntry};

    fn make_config() -> ArchitectureConfig {
        ArchitectureConfig {
            layers: vec![
                ArchitectureLayer { name: "handler".to_string(), match_globs: vec!["**/handler/**".to_string()] },
                ArchitectureLayer { name: "repository".to_string(), match_globs: vec!["**/repository/**".to_string()] },
                ArchitectureLayer { name: "service".to_string(), match_globs: vec!["**/service/**".to_string()] },
            ],
            rules: vec![
                ArchitectureRuleEntry { forbid: ArchitectureForbidRule { from: "repository".to_string(), to: vec!["handler".to_string(), "service".to_string()] } },
                ArchitectureRuleEntry { forbid: ArchitectureForbidRule { from: "handler".to_string(), to: vec!["repository".to_string()] } },
            ],
        }
    }

    #[test]
    fn layer_for_path_matches_declared_globs() {
        let config = make_config();
        assert_eq!(layer_for_path("internal/handler/login.go", &config.layers), Some("handler"));
        assert_eq!(layer_for_path("internal/repository/user_repo.go", &config.layers), Some("repository"));
        assert_eq!(layer_for_path("internal/util/strings.go", &config.layers), None);
    }

    #[test]
    fn extract_imports_finds_single_line_go_imports() {
        let content = "package main\n\nimport \"myapp/internal/repository\"\n\nfunc main() {}\n";
        let imports = extract_imports(content, ImportLanguage::Go);
        assert_eq!(imports, vec![(3, "myapp/internal/repository".to_string())]);
    }

    #[test]
    fn extract_imports_finds_go_import_blocks() {
        let content = "package main\n\nimport (\n\t\"fmt\"\n\t\"myapp/internal/repository\"\n)\n\nfunc main() {}\n";
        let imports = extract_imports(content, ImportLanguage::Go);
        assert_eq!(imports, vec![(4, "fmt".to_string()), (5, "myapp/internal/repository".to_string())]);
    }

    #[test]
    fn extract_imports_finds_java_imports_including_static() {
        let content = "package com.example;\n\nimport com.example.repository.UserRepository;\nimport static java.util.Objects.requireNonNull;\n\npublic class S {}\n";
        let imports = extract_imports(content, ImportLanguage::JavaOrKotlin);
        assert_eq!(imports, vec![(3, "com.example.repository.UserRepository".to_string()), (4, "java.util.Objects.requireNonNull".to_string())]);
    }

    #[test]
    fn normalize_import_converts_dots_to_slashes_for_java_and_kotlin() {
        assert_eq!(normalize_import("com.example.repository.UserRepo", ImportLanguage::JavaOrKotlin), "com/example/repository/UserRepo/_");
        assert_eq!(normalize_import("myapp/internal/repository", ImportLanguage::Go), "myapp/internal/repository/_");
    }

    #[test]
    fn normalize_import_appends_a_trailing_segment_so_bare_directory_imports_match_file_style_globs() {
        // Regression test: a real bug found via the Go layer-violation test
        // failing — `**/repository/**` does not match the literal string
        // `myapp/internal/repository` (no trailing segment), only
        // `myapp/internal/repository/<anything>`.
        let glob = globset::Glob::new("**/repository/**").unwrap().compile_matcher();
        assert!(!glob.is_match("myapp/internal/repository"), "sanity check: bare directory path should NOT match on its own");
        assert!(glob.is_match(normalize_import("myapp/internal/repository", ImportLanguage::Go)));
    }

    #[test]
    fn detects_a_direct_layer_violation_in_go() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("internal/handler")).unwrap();
        std::fs::write(dir.path().join("internal/handler/login.go"), "package handler\n\nimport \"myapp/internal/repository\"\n\nfunc Login() {}\n").unwrap();

        let config = make_config();
        let findings = run_architecture_check(dir.path(), &["internal/handler/login.go".to_string()], &config);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "architecture");
        assert_eq!(findings[0].location.range.start_line, 3);
        assert!(findings[0].message.contains("handler"));
        assert!(findings[0].message.contains("repository"));
    }

    #[test]
    fn does_not_flag_an_allowed_dependency() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("internal/handler")).unwrap();
        std::fs::write(dir.path().join("internal/handler/login.go"), "package handler\n\nimport \"myapp/internal/service\"\n\nfunc Login() {}\n").unwrap();

        let config = make_config();
        let findings = run_architecture_check(dir.path(), &["internal/handler/login.go".to_string()], &config);
        assert!(findings.is_empty(), "handler -> service is not forbidden in this config");
    }

    #[test]
    fn detects_a_direct_layer_violation_in_java() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repository")).unwrap();
        std::fs::write(dir.path().join("repository/UserRepository.java"), "package com.example.repository;\n\nimport com.example.handler.LoginHandler;\n\npublic class UserRepository {}\n").unwrap();

        let config = make_config();
        let findings = run_architecture_check(dir.path(), &["repository/UserRepository.java".to_string()], &config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("repository"));
        assert!(findings[0].message.contains("handler"));
    }

    #[test]
    fn returns_empty_when_no_layers_are_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("internal/handler")).unwrap();
        std::fs::write(dir.path().join("internal/handler/login.go"), "package handler\n\nimport \"myapp/internal/repository\"\n").unwrap();

        let findings = run_architecture_check(dir.path(), &["internal/handler/login.go".to_string()], &ArchitectureConfig::default());
        assert!(findings.is_empty());
    }

    #[test]
    fn load_architecture_config_returns_none_when_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_architecture_config(&dir.path().join("architecture.yaml")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_architecture_config_parses_the_documented_yaml_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("architecture.yaml");
        std::fs::write(
            &path,
            r#"
architecture:
  layers:
    - name: handler
      match: ["**/handler/**", "**/controller/**"]
    - name: repository
      match: ["**/repository/**"]
  rules:
    - forbid: { from: repository, to: [handler] }
"#,
        )
        .unwrap();
        let config = load_architecture_config(&path).unwrap().unwrap();
        assert_eq!(config.layers.len(), 2);
        assert_eq!(config.layers[0].name, "handler");
        assert_eq!(config.layers[0].match_globs, vec!["**/handler/**", "**/controller/**"]);
        assert_eq!(config.rules[0].forbid.from, "repository");
        assert_eq!(config.rules[0].forbid.to, vec!["handler"]);
    }
}
