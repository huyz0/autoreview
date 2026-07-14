//! Runs approved-but-not-yet-promoted (shadow) and already-promoted rules
//! from `.autoreview/rules/{shadow,promoted}/*.yaml` against the diff's
//! changed files, via the real `ast-grep` binary — same invocation shape as
//! `analyzers::ast_grep`'s builtin pack, just pointed at a repo-local rule
//! directory instead of the embedded one.
//!
//! A rule file lands in `shadow/` or `promoted/` by a human copying a
//! drafted-and-benched candidate's `rule.yaml` there (`rules review`, the
//! human-approval gate, is still a stub — this is the interim path). Every
//! firing gets recorded into the history store regardless of status; the
//! caller (`diff.rs`) decides whether to suppress (shadow) or surface
//! (promoted) the resulting finding.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Side};

use super::ast_grep::{is_relevant_source_file, map_severity, title_from_rule_id};

fn default_category() -> String {
    "correctness".to_string()
}

#[derive(Debug, Deserialize)]
struct RuleIdMeta {
    id: String,
    #[serde(default = "default_category")]
    category: String,
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

    let temp_dir = tempfile::tempdir()?;
    let rules_dir = temp_dir.path().join("rules");
    std::fs::create_dir_all(&rules_dir)?;

    let mut meta_by_rule_id: HashMap<String, (&'static str, String)> = HashMap::new();
    for (idx, rule_file) in rule_files.iter().enumerate() {
        let Ok(contents) = std::fs::read_to_string(&rule_file.path) else { continue };
        let Ok(meta) = serde_yaml::from_str::<RuleIdMeta>(&contents) else { continue };
        meta_by_rule_id.insert(meta.id, (rule_file.status, meta.category));
        std::fs::write(rules_dir.join(format!("rule-{idx}.yml")), &contents)?;
    }
    if meta_by_rule_id.is_empty() {
        return Ok(vec![]);
    }

    let sgconfig_path = temp_dir.path().join("sgconfig.yml");
    std::fs::write(&sgconfig_path, "ruleDirs:\n  - rules\n")?;

    let output = match Command::new("ast-grep").arg("scan").arg("--config").arg(&sgconfig_path).arg("--json").args(&relevant).current_dir(repo_root).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err.into()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let matches: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
        Ok(matches) => matches,
        Err(err) => anyhow::bail!("ast-grep produced unparsable output: {err}. stderr: {}", String::from_utf8_lossy(&output.stderr)),
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

    fn ast_grep_available() -> bool {
        Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const SELF_COMPARISON_RULE: &str = "id: go-self-comparison-shadow-test\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: self comparison\nrule:\n  pattern: $A == $A\n";

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
