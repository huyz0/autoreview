//! Data-driven conformance test for every builtin ast-grep rule: each rule
//! gets a positive fixture (must fire, exactly once, with the right rule id)
//! and a negative fixture (must not fire at all). This is the same
//! self-test discipline the plan's rule-factory bench stage calls for
//! ("100% on its own positive/negative test files") applied to the builtin
//! rules we ship today — adding a new rule means adding rule YAML +
//! fixture files, not touching this test file.
//!
//! Rules are discovered in the same compiled rule directory embedded in
//! the shipped binary. Each rule's `id`/`language` selects its fixtures
//! from `tests/fixtures/rules/<rule-id>/`. Use a positive and negative
//! source pair or a `cases.yml` file when both cases share a template
//! or TypeScript exercises the same sources as JavaScript.
//!
//! Requires the real `ast-grep` binary; skips (not fails) when it's absent,
//! so this suite is safe to run anywhere but exercises real integration
//! wherever the tool is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_core::run_ast_grep;
use serde::Deserialize;

fn default_kind() -> String {
    "pattern".to_string()
}

#[derive(Debug, Deserialize)]
struct RuleMeta {
    id: String,
    language: String,
    #[serde(default = "default_kind")]
    kind: String,
}

fn ast_grep_available() -> bool {
    Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn rules_dir() -> PathBuf {
    Path::new(env!("OUT_DIR")).join("compiled-rules")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SharedFixtures {
    JavaScript { javascript: String },
    Template { template: String, positive: String, negative: String },
}

fn read_fixture(rule: &RuleMeta, case: &str, extension: &str) -> anyhow::Result<String> {
    let dir = fixtures_dir().join(&rule.id);
    let path = dir.join(format!("{case}.{extension}"));
    if path.is_file() {
        return Ok(std::fs::read_to_string(path)?);
    }
    let shared = dir.join("cases.yml");
    anyhow::ensure!(shared.is_file(), "missing fixture {}", path.display());
    let fixture = serde_yaml::from_str(&std::fs::read_to_string(shared)?)?;
    match fixture {
        SharedFixtures::JavaScript { javascript } => {
            let source = fixtures_dir().join(javascript).join(format!("{case}.js"));
            Ok(std::fs::read_to_string(source)?)
        }
        SharedFixtures::Template { template, positive, negative } => {
            const PLACEHOLDER: &str = "@CASE@";
            anyhow::ensure!(template.matches(PLACEHOLDER).count() == 1, "{} needs one {PLACEHOLDER}", rule.id);
            let replacement = if case == "positive" { positive } else { negative };
            Ok(template.replace(PLACEHOLDER, &replacement))
        }
    }
}

/// Only `kind: pattern` (or `kind`-absent) rules — this test drives
/// `run_ast_grep`, which can't run a `kind: taint`/`kind: threshold` rule
/// at all (see `extract_pattern_rules`'s docs in `ast_grep.rs` for why
/// those files are invisible to the ast-grep subprocess in the first
/// place). Non-pattern rules get their own conformance test —
/// `taint_rule_conformance.rs` for `kind: taint`.
fn discover_rules() -> Vec<RuleMeta> {
    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    rules.retain(|r| r.kind == "pattern");
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    rules
}

fn walk(dir: &Path, out: &mut Vec<RuleMeta>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let meta: RuleMeta = serde_yaml::from_str(&contents).unwrap_or_else(|e| panic!("failed to parse {} as rule metadata: {e}", path.display()));
        out.push(meta);
    }
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        "TypeScript" => "ts",
        "Tsx" => "tsx",
        "JavaScript" => "js",
        "Rust" => "rs",
        other => panic!("rule_pack_conformance doesn't know the fixture extension for language {other:?} — add it to extension_for_language"),
    }
}

fn scan_filename_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "main.go",
        "Java" => "Sample.java",
        "Kotlin" => "Sample.kt",
        "TypeScript" => "sample.ts",
        "Tsx" => "Sample.tsx",
        "JavaScript" => "sample.js",
        "Rust" => "sample.rs",
        other => panic!("rule_pack_conformance doesn't know the scan filename for language {other:?} — add it to scan_filename_for_language"),
    }
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_builtin_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    if !ast_grep_available() {
        eprintln!("skipping rule_pack_conformance: ast-grep not on PATH");
        return;
    }

    let rules = discover_rules();
    assert!(!rules.is_empty(), "expected to discover at least one rule under {}", rules_dir().display());

    let mut failures = Vec::new();

    for rule in &rules {
        let ext = extension_for_language(&rule.language);
        let filename = scan_filename_for_language(&rule.language);
        let positive = read_fixture(rule, "positive", ext).unwrap_or_else(|error| panic!("{}: {error}", rule.id));
        let negative = read_fixture(rule, "negative", ext).unwrap_or_else(|error| panic!("{}: {error}", rule.id));

        if rule.language == "Rust" {
            for (case, source) in [("positive", &positive), ("negative", &negative)] {
                let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Rust).unwrap();
                if parser.parse(source, None).unwrap().root_node().has_error() {
                    failures.push(format!("{}: invalid Rust {case} fixture", rule.id));
                }
            }
        }

        let positive_dir = write_single_file(filename, &positive);
        let positive_findings = run_ast_grep(positive_dir.path(), &[filename.to_string()], &[]).unwrap();
        let positive_matches: Vec<_> = positive_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if positive_matches.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 match on positive fixture, got {} (all findings: {:?})",
                rule.id,
                positive_matches.len(),
                positive_findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
            ));
        }

        let negative_dir = write_single_file(filename, &negative);
        let negative_findings = run_ast_grep(negative_dir.path(), &[filename.to_string()], &[]).unwrap();
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", rule.id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

const KOTLIN_COROUTINE_RULES: &[&str] = &["kotlin-runblocking-in-suspend-function", "kotlin-thread-sleep-in-suspend", "kotlin-suspend-swallowed-by-runcatching", "kotlin-suspend-call-in-finally-without-noncancellable"];
const KOTLIN_COROUTINE_CHECKS: &str = "runBlocking { work() }; Thread.sleep(1); runCatching { work() }; try { work() } finally { cleanup() }";

fn kotlin_test_repo(source: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("example.kt"), source).unwrap();
    repo
}

#[test]
fn kotlin_suspend_checks_use_the_nearest_function_modifiers() {
    if !ast_grep_available() {
        return;
    }
    let checks = KOTLIN_COROUTINE_CHECKS;
    let source = format!("suspend fun outer() {{\n fun callback() {{ {checks} }}\n val anonymous = fun() {{ {checks} }}\n {checks}\n listOf(1).map {{ {checks} }}\n}}\nfun ordinary() {{\n suspend fun nested() {{ {checks} }}\n {checks}\n}}\n");
    let repo = kotlin_test_repo(&source);
    let findings = run_ast_grep(repo.path(), &["example.kt".into()], &[]).unwrap();
    for &rule in KOTLIN_COROUTINE_RULES {
        let lines: Vec<_> = findings.iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
            .map(|finding| finding.location.range.start_line)
            .collect();
        assert_eq!(lines, [4, 5, 8], "{rule}");
    }
}

#[test]
fn kotlin_callback_findings_report_unresolved_execution() {
    if !ast_grep_available() {
        return;
    }
    let checks = KOTLIN_COROUTINE_CHECKS;
    let source = format!("suspend fun run() {{\n {checks}\n callbacks.register {{ {checks} }}\n listOf(1).map {{ {checks} }}\n}}\n");
    let repo = kotlin_test_repo(&source);
    let result = autoreview_core::run_ast_grep_with_coverage(repo.path(), &["example.kt".into()], &[]).unwrap();
    for &rule in KOTLIN_COROUTINE_RULES {
        let findings: Vec<_> = result.findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule)).collect();
        assert_eq!(findings.len(), 3, "{rule}");
        assert_eq!(findings[0].confidence, 1.0, "{rule}");
        assert!(findings[1..].iter().all(|finding| finding.confidence < 1.0 && finding.message.contains("unresolved")), "{rule}");
        let lines: Vec<_> = result.coverage.iter().filter(|issue| issue.rule.as_deref() == Some(rule) && issue.reason == "callback_execution_unresolved").map(|issue| issue.line.unwrap()).collect();
        assert_eq!(lines, [3, 4], "{rule}");
    }
}

#[test]
fn kotlin_direct_lambda_invocation_preserves_outer_uncertainty() {
    if !ast_grep_available() { return; }
    let checks = KOTLIN_COROUTINE_CHECKS;
    let source = format!("suspend fun run() {{\n ({{ {checks} }})()\n ((({{ {checks} }})))()\n register {{ ({{ {checks} }})() }}\n val action = {{ {checks} }}\n action()\n ({{ {checks} }}).invoke()\n}}\n");
    let repo = kotlin_test_repo(&source);
    let result = autoreview_core::run_ast_grep_with_coverage(repo.path(), &["example.kt".into()], &[]).unwrap();
    for &rule in KOTLIN_COROUTINE_RULES {
        let findings: Vec<_> = result.findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule)).collect();
        let lines: Vec<_> = findings.iter().map(|finding| finding.location.range.start_line).collect();
        assert_eq!(lines, [2, 3, 4, 5, 7], "{rule}");
        assert!(findings[..2].iter().all(|finding| finding.confidence == 1.0), "{rule}");
        assert!(findings[2..4].iter().all(|finding| finding.confidence < 1.0), "{rule}");
        assert_eq!(findings[4].confidence, 1.0, "{rule}");
        let incomplete: Vec<_> = result.coverage.iter().filter(|issue| issue.rule.as_deref() == Some(rule)).map(|issue| issue.line.unwrap()).collect();
        assert_eq!(incomplete, [4, 5], "{rule}");
    }
}

#[test]
fn kotlin_local_callback_aliases_require_contiguous_immutable_execution() {
    if !ast_grep_available() { return; }
    let checks = KOTLIN_COROUTINE_CHECKS;
    let source = format!("suspend fun run(flag: Boolean) {{\n val first = {{ {checks} }}\n val copy = first\n copy.invoke()\n val delayed = {{ {checks} }}\n work()\n delayed.invoke()\n var mutable = {{ {checks} }}\n mutable = {{ }}\n mutable.invoke()\n register {{ val nested = {{ {checks} }}; nested.invoke() }}\n val bare = {{ {checks} }}\n bare()\n val branch = {{ {checks} }}\n if (flag) branch.invoke()\n val escaped = {{ {checks} }}\n register(escaped)\n}}\n");
    let repo = kotlin_test_repo(&source);
    let result = autoreview_core::run_ast_grep_with_coverage(repo.path(), &["example.kt".into()], &[]).unwrap();
    for &rule in KOTLIN_COROUTINE_RULES {
        let findings: Vec<_> = result.findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule)).collect();
        assert_eq!(findings.iter().map(|finding| finding.location.range.start_line).collect::<Vec<_>>(), [2, 5, 8, 11, 12, 14, 16], "{rule}");
        assert_eq!(findings[0].confidence, 1.0, "{rule}");
        assert!(findings[1..].iter().all(|finding| finding.confidence < 1.0), "{rule}");
        let incomplete: Vec<_> = result.coverage.iter().filter(|issue| issue.rule.as_deref() == Some(rule)).map(|issue| issue.line.unwrap()).collect();
        assert_eq!(incomplete, [5, 8, 11, 12, 14, 16], "{rule}");
    }
}

#[test]
fn kotlin_unused_local_callbacks_do_not_inherit_suspend_execution() {
    if !ast_grep_available() { return; }
    let checks = KOTLIN_COROUTINE_CHECKS;
    let source = format!("suspend fun run() {{\n val unused = {{ {checks} }}\n val called = {{ {checks} }}\n `called`.invoke()\n val passed = {{ {checks} }}\n register(passed)\n val nested = {{ register {{ {checks} }} }}\n val shadow = {{ {checks} }}\n val other = {{ val shadow = 1 }}\n val template = {{ {checks} }}\n println(\"${{template()}}\")\n}}\n");
    let repo = kotlin_test_repo(&source);
    let result = autoreview_core::run_ast_grep_with_coverage(repo.path(), &["example.kt".into()], &[]).unwrap();
    for &rule in KOTLIN_COROUTINE_RULES {
        let findings: Vec<_> = result.findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule)).collect();
        assert_eq!(findings.iter().map(|finding| finding.location.range.start_line).collect::<Vec<_>>(), [3, 5, 8, 10], "{rule}");
        assert!(findings.iter().all(|finding| finding.confidence < 1.0), "{rule}");
        let incomplete: Vec<_> = result.coverage.iter().filter(|issue| issue.rule.as_deref() == Some(rule)).map(|issue| issue.line.unwrap()).collect();
        assert_eq!(incomplete, [3, 5, 8, 10], "{rule}");
    }
}

#[test]
fn kotlin_callback_alias_budget_remains_incomplete() {
    if !ast_grep_available() { return; }
    for count in [31, 32] {
        let mut source = String::from("suspend fun run() {\n val a0 = { runBlocking { work() } }\n");
        for index in 1..=count { source.push_str(&format!(" val a{index} = a{}\n", index - 1)); }
        source.push_str(&format!(" a{count}.invoke()\n}}\n"));
        let repo = kotlin_test_repo(&source);
        let result = autoreview_core::run_ast_grep_with_coverage(repo.path(), &["example.kt".into()], &[]).unwrap();
        let incomplete = result.coverage.iter().any(|issue| issue.rule.as_deref() == Some("kotlin-runblocking-in-suspend-function") && issue.reason == "callback_execution_unresolved");
        assert_eq!(incomplete, count == 32, "{:?}", result.coverage);
    }
}

#[test]
fn typescript_pattern_catalog_has_tsx_parity() {
    if !ast_grep_available() {
        return;
    }
    let repo = tempfile::tempdir().unwrap();
    let mut samples = Vec::new();
    for rule in discover_rules().into_iter().filter(|rule| rule.language == "TypeScript") {
        let dir = fixtures_dir().join(&rule.id);
        for (fixture, expected) in [("positive", 1), ("negative", 0)] {
            let with_jsx = dir.join(format!("{fixture}.tsx"));
            let extension = if with_jsx.exists() { "tsx" } else { "ts" };
            let source = read_fixture(&rule, fixture, extension).unwrap();
            let path = format!("{}-{fixture}.tsx", rule.id);
            std::fs::write(repo.path().join(&path), source).unwrap();
            samples.push((path, rule.id.clone(), expected));
        }
    }
    let paths: Vec<_> = samples.iter().map(|(path, _, _)| path.clone()).collect();
    let findings = run_ast_grep(repo.path(), &paths, &[]).unwrap();
    for (path, id, expected) in samples {
        let count = findings.iter().filter(|finding| finding.location.path == path && finding.source.rule_id.as_deref() == Some(id.as_str())).count();
        assert_eq!(count, expected, "{id} on {path}");
    }
}

#[test]
fn every_rule_declares_a_category() {
    for rule in discover_rules() {
        // Re-parse fully this time (discover_rules only pulls id/language) to
        // assert the category field specifically — a rule with no category
        // silently falls back to "correctness" in production, which is
        // surprising enough to want a hard test failure instead.
        let path = find_rule_file(&rule.id).unwrap_or_else(|| panic!("could not relocate rule file for {}", rule.id));
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        assert!(value.get("category").and_then(|c| c.as_str()).is_some(), "{}: rule file has no `category` field ({})", rule.id, path.display());
    }
}

fn find_rule_file(rule_id: &str) -> Option<PathBuf> {
    fn walk_find(dir: &Path, rule_id: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk_find(&path, rule_id) {
                    return Some(found);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("yml") {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    if let Ok(meta) = serde_yaml::from_str::<RuleMeta>(&contents) {
                        if meta.id == rule_id {
                            return Some(path);
                        }
                    }
                }
            }
        }
        None
    }
    walk_find(&rules_dir(), rule_id)
}

/// Every `category: security` rule must carry a CWE mapping, whatever its
/// `kind`. CWE IDs are how a security finding gets triaged, mapped to
/// compliance requirements, and understood by downstream SARIF consumers,
/// so a security rule without one is materially less useful than one with
/// it. 37 rules were missing this when the guard was added; the point of
/// the guard is that the next one can't ship silently.
#[test]
fn every_security_rule_declares_a_cwe_mapping() {
    #[derive(Debug, serde::Deserialize)]
    struct SecurityRuleMeta {
        id: String,
        #[serde(default)]
        category: String,
        #[serde(default)]
        metadata: Option<MetaBlock>,
    }
    #[derive(Debug, serde::Deserialize)]
    struct MetaBlock {
        #[serde(default)]
        cwe: Vec<String>,
    }

    fn walk(dir: &Path, out: &mut Vec<(PathBuf, SecurityRuleMeta)>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let is_yaml = path.extension().and_then(|e| e.to_str()).map(|e| e == "yml" || e == "yaml").unwrap_or(false);
            if !is_yaml || path.file_name().and_then(|n| n.to_str()) == Some("rulepack.yaml") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            if let Ok(meta) = serde_yaml::from_str::<SecurityRuleMeta>(&contents) {
                out.push((path, meta));
            }
        }
    }

    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    let security: Vec<_> = rules.iter().filter(|(_, r)| r.category == "security").collect();
    assert!(!security.is_empty(), "expected the builtin pack to contain security rules");

    let missing: Vec<String> = security
        .iter()
        .filter(|(_, r)| r.metadata.as_ref().map(|m| m.cwe.is_empty()).unwrap_or(true))
        .map(|(p, r)| format!("{} ({})", r.id, p.display()))
        .collect();
    assert!(missing.is_empty(), "{} security rule(s) declare no cwe metadata:\n{}", missing.len(), missing.join("\n"));
}
