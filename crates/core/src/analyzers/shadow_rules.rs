//! Runs approved-but-not-yet-promoted (shadow) and already-promoted rules
//! from `.autoreview/rules/{shadow,promoted}/*.yaml` against the diff's
//! changed files, via the real `ast-grep` binary — same invocation shape as
//! `analyzers::ast_grep`'s builtin pack, just pointed at a repo-local rule
//! directory instead of the embedded one.
//!
//! A rule file lands in `shadow/` when a human approves a
//! drafted-and-benched candidate (`autoreview rules review --approve`,
//! which copies its `rule.yaml` here and registers it in the history
//! store), and moves on to `promoted/` once the firing-history gate in
//! `diff.rs` promotes it. Every firing gets recorded into the history store
//! regardless of status; the caller (`diff.rs`) decides whether to suppress
//! (shadow) or surface (promoted) the resulting finding.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use autoreview_langsupport::Language;
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Side};

use super::ast_grep::{is_relevant_source_file, map_severity, title_from_rule_id};
use super::rule_pack::{run_ast_grep_scan, write_rule_file};

fn default_category() -> String {
    "correctness".to_string()
}

#[derive(Debug, Deserialize)]
struct RuleIdMeta {
    id: String,
    #[serde(default = "default_category")]
    category: String,
    /// ast-grep's own `language:` field — always present in practice (ast-
    /// grep itself requires it to run a pattern rule at all), but parsed
    /// as optional here since Rust has never enforced it before this
    /// gate existed: a shadow rule predating this change with no
    /// (unexpectedly) parseable `language:` value fails open, i.e. still
    /// gets copied, rather than silently going missing.
    #[serde(default)]
    language: Option<String>,
}

/// Maps ast-grep's own `language:` string value to the `Language`(s) it can
/// match against — same convention as `ast_grep.rs`'s `RULE_DIR_LANGUAGES`:
/// `"TypeScript"` covers both `.ts` and `.tsx` (ast-grep dispatches `.tsx`
/// files under the `TypeScript` value based on extension, not a separate
/// declared value in practice), `"Tsx"` is accepted too for a rule author
/// who writes it explicitly.
fn languages_from_str(value: &str) -> Option<&'static [Language]> {
    match value {
        "Go" => Some(&[Language::Go]),
        "Java" => Some(&[Language::Java]),
        "Kotlin" => Some(&[Language::Kotlin]),
        "TypeScript" => Some(&[Language::TypeScript, Language::Tsx]),
        "Tsx" => Some(&[Language::Tsx]),
        "JavaScript" => Some(&[Language::JavaScript]),
        _ => None,
    }
}

/// One discovered shadow/promoted rule file and the lifecycle status
/// implied by which directory it was found in.
pub struct ShadowRuleFile {
    pub path: std::path::PathBuf,
    pub status: &'static str,
}

fn collect_rule_files(dir: &Path, status: &'static str, out: &mut Vec<ShadowRuleFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()).is_some_and(|ext| ext == "yml" || ext == "yaml") {
            out.push(ShadowRuleFile { path, status });
        }
    }
}

/// Discovers every rule file under `.autoreview/rules/shadow/` and
/// `.autoreview/rules/promoted/`. Returns an empty list (not an error) if
/// neither directory exists — shadow mode is entirely opt-in.
pub fn discover_shadow_rule_files(repo_root: &Path) -> Vec<ShadowRuleFile> {
    let mut files = Vec::new();
    collect_rule_files(&repo_root.join(".autoreview").join("rules").join("shadow"), "shadow", &mut files);
    collect_rule_files(&repo_root.join(".autoreview").join("rules").join("promoted"), "promoted", &mut files);
    files
}

/// One firing of a shadow/promoted rule against the diff, with the finding
/// itself plus which rule (by id) and lifecycle status produced it — the
/// caller needs both to decide whether to suppress it and how to record it.
pub struct ShadowFinding {
    pub finding: AgentFinding,
    pub rule_id: String,
    pub status: &'static str,
}

/// Runs every discovered shadow/promoted rule file against the changed
/// files. Returns an empty list (not an error) if no rule files are
/// discovered, no changed file matches a supported language, or `ast-grep`
/// isn't on PATH — matches `run_ast_grep`'s own graceful-degradation
/// contract, since shadow mode should never be able to break Stage 1.
pub fn run_shadow_rules(repo_root: &Path, changed_files: &[String]) -> anyhow::Result<Vec<ShadowFinding>> {
    let rule_files = discover_shadow_rule_files(repo_root);
    if rule_files.is_empty() {
        return Ok(vec![]);
    }

    let relevant: Vec<&str> = changed_files.iter().map(String::as_str).filter(|p| is_relevant_source_file(p) && repo_root.join(p).exists()).collect();
    if relevant.is_empty() {
        return Ok(vec![]);
    }
    let languages_present = autoreview_langsupport::languages_present(relevant.iter().copied());

    let temp_dir = tempfile::tempdir()?;
    let rules_dir = temp_dir.path().join("rules");
    std::fs::create_dir_all(&rules_dir)?;

    let mut meta_by_rule_id: HashMap<String, (&'static str, String)> = HashMap::new();
    for (idx, rule_file) in rule_files.iter().enumerate() {
        let Ok(contents) = std::fs::read_to_string(&rule_file.path) else { continue };
        let Ok(meta) = serde_yaml::from_str::<RuleIdMeta>(&contents) else { continue };
        // Rule-group apply condition, fail-open: only skip when the
        // declared language is both present AND resolves to a known
        // Language with none of it in this diff — an unparseable/absent
        // `language:` value means "don't gate," not "exclude."
        if let Some(langs) = meta.language.as_deref().and_then(languages_from_str) {
            if !langs.iter().any(|l| languages_present.contains(l)) {
                continue;
            }
        }
        meta_by_rule_id.insert(meta.id, (rule_file.status, meta.category));
        write_rule_file(&rules_dir.join(format!("rule-{idx}.yml")), &contents)?;
    }
    if meta_by_rule_id.is_empty() {
        return Ok(vec![]);
    }

    let matches = match run_ast_grep_scan(temp_dir.path(), repo_root, &relevant)? {
        Some(matches) => matches,
        None => return Ok(vec![]),
    };

    Ok(matches.iter().filter_map(|m| match_to_shadow_finding(m, &meta_by_rule_id)).collect())
}

fn match_to_shadow_finding(m: &serde_json::Value, meta_by_rule_id: &HashMap<String, (&'static str, String)>) -> Option<ShadowFinding> {
    let rule_id = m.get("ruleId")?.as_str()?.to_string();
    let (status, category) = meta_by_rule_id.get(&rule_id)?.clone();
    let file = m.get("file")?.as_str()?.to_string();
    let message = m.get("message").and_then(|v| v.as_str()).unwrap_or("(no message provided by rule)").to_string();
    let severity = map_severity(m.get("severity").and_then(|v| v.as_str()).unwrap_or("warning"));
    let snippet = m.get("lines").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let range = m.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32 + 1;
    let start_col = range.get("start")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32 + 1;
    let end_col = range.get("end")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);

    Some(ShadowFinding {
        finding: AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-shadow-rule".to_string(), rule_id: Some(rule_id.clone()), aspect: None, backend: None },
            category,
            severity,
            confidence: 1.0,
            title: title_from_rule_id(&rule_id),
            message,
            location: Location { path: file, range: LocationRange { start_line, start_col, end_line: Some(end_line), end_col }, snippet, side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        },
        rule_id,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn ast_grep_available() -> bool {
        Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const SELF_COMPARISON_RULE: &str = "id: go-self-comparison-shadow-test\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: self comparison\nrule:\n  pattern: $A == $A\n";
    const JAVA_ONLY_RULE: &str = "id: java-only-shadow-test\nlanguage: Java\ncategory: correctness\nseverity: warning\nmessage: java thing\nrule:\n  pattern: $A == $A\n";

    #[test]
    fn languages_from_str_maps_known_ast_grep_language_values() {
        assert_eq!(languages_from_str("Go"), Some(&[Language::Go][..]));
        assert_eq!(languages_from_str("TypeScript"), Some(&[Language::TypeScript, Language::Tsx][..]));
        assert_eq!(languages_from_str("Unknown"), None);
    }

    #[test]
    fn rule_id_meta_language_field_is_optional_for_fail_open_behavior() {
        let meta: RuleIdMeta = serde_yaml::from_str("id: x\ncategory: correctness\n").unwrap();
        assert_eq!(meta.language, None);
    }

    #[test]
    fn a_shadow_rule_for_a_different_language_does_not_fire_on_a_go_only_diff() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".autoreview/rules/shadow/rule.yaml"), JAVA_ONLY_RULE);
        write(&dir.path().join("main.go"), "package main\n\nfunc f(x int) bool {\n\treturn x == x\n}\n");

        let findings = run_shadow_rules(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.is_empty(), "a Java-only shadow rule should not be considered for a Go-only diff, got: {:#?}", findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>());
    }

    #[test]
    fn returns_empty_when_no_shadow_or_promoted_directories_exist() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("main.go"), "package main\n\nfunc f(x int) bool {\n\treturn x == x\n}\n");
        let findings = run_shadow_rules(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn fires_a_shadow_rule_and_reports_its_status() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".autoreview/rules/shadow/rule.yaml"), SELF_COMPARISON_RULE);
        write(&dir.path().join("main.go"), "package main\n\nfunc f(x int) bool {\n\treturn x == x\n}\n");

        let findings = run_shadow_rules(dir.path(), &["main.go".to_string()]).unwrap();
        assert_eq!(findings.len(), 1, "got: {:#?}", findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>());
        assert_eq!(findings[0].status, "shadow");
        assert_eq!(findings[0].rule_id, "go-self-comparison-shadow-test");
    }

    #[test]
    fn fires_a_promoted_rule_and_reports_its_status() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".autoreview/rules/promoted/rule.yaml"), SELF_COMPARISON_RULE);
        write(&dir.path().join("main.go"), "package main\n\nfunc f(x int) bool {\n\treturn x == x\n}\n");

        let findings = run_shadow_rules(dir.path(), &["main.go".to_string()]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, "promoted");
    }

    #[test]
    fn does_not_fire_when_the_pattern_does_not_match() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".autoreview/rules/shadow/rule.yaml"), SELF_COMPARISON_RULE);
        write(&dir.path().join("main.go"), "package main\n\nfunc f(x, y int) bool {\n\treturn x == y\n}\n");

        let findings = run_shadow_rules(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.is_empty());
    }
}
