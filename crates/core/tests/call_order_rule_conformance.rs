//! Conformance test for every `kind: call-sequence` rule — same fixture
//! convention and structure as `taint_rule_conformance.rs`, duplicated
//! (not shared) for the same reason that file is its own from
//! `rule_pack_conformance.rs`: each `kind` is a distinct execution
//! backend with its own discovery filter, and the three files are small
//! enough that a shared harness would be more indirection than the
//! duplication costs.

use std::path::{Path, PathBuf};

use autoreview_core::run_dataflow_check;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RuleMeta {
    id: String,
    language: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    category: String,
}

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rules-builtin")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

fn discover_call_order_rules() -> Vec<RuleMeta> {
    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    rules.retain(|r| r.kind == "call-sequence");
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
        let Ok(meta) = serde_yaml::from_str::<RuleMeta>(&contents) else { continue };
        out.push(meta);
    }
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        other => panic!("call_order_rule_conformance doesn't know the fixture extension for language {other:?} — add it to extension_for_language"),
    }
}

fn scan_filename_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "main.go",
        "Java" => "Main.java",
        "Kotlin" => "Main.kt",
        other => panic!("call_order_rule_conformance doesn't know the scan filename for language {other:?} — add it to scan_filename_for_language"),
    }
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_call_order_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    let rules = discover_call_order_rules();
    assert!(!rules.is_empty(), "expected to discover at least one kind: call-sequence rule under {}", rules_dir().display());

    let mut failures = Vec::new();

    for rule in &rules {
        let ext = extension_for_language(&rule.language);
        let filename = scan_filename_for_language(&rule.language);
        let fixture_dir = fixtures_dir().join(&rule.id);
        let positive_path = fixture_dir.join(format!("positive.{ext}"));
        let negative_path = fixture_dir.join(format!("negative.{ext}"));

        let (Ok(positive), Ok(negative)) = (std::fs::read_to_string(&positive_path), std::fs::read_to_string(&negative_path)) else {
            failures.push(format!("{}: missing fixture files (expected {} and {})", rule.id, positive_path.display(), negative_path.display()));
            continue;
        };

        let positive_dir = write_single_file(filename, &positive);
        let positive_findings = run_dataflow_check(positive_dir.path(), &[filename.to_string()], &[]);
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
        let negative_findings = run_dataflow_check(negative_dir.path(), &[filename.to_string()], &[]);
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", rule.id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "call-sequence rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

#[test]
fn completion_requires_the_same_returned_handle_across_files() {
    let factory = r#"package archive

import (
    "compress/gzip"
    "io"
)

func Create(out io.Writer) *gzip.Writer {
    return gzip.NewWriter(out)
}

func Finish(writer *gzip.Writer) {
    writer.Close()
}
"#;
    let caller = r#"package jobs

import (
    "compress/gzip"
    "example.com/project/archive"
    "io"
)

func Write(out io.Writer, other *gzip.Writer) {
    original := archive.Create(out)
    alias := original
    defer archive.Finish(other)
    alias.Write([]byte("payload"))
}
"#;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("archive");
    std::fs::create_dir(&archive).unwrap();
    std::fs::write(root.path().join("go.mod"), "module example.com/project\n\ngo 1.22\n").unwrap();
    std::fs::write(archive.join("writer.go"), factory).unwrap();
    let path = root.path().join("jobs.go");
    std::fs::write(&path, caller).unwrap();
    let scan = || run_dataflow_check(root.path(), &["jobs.go".into()], &[]).into_iter()
        .filter(|hit| hit.source.rule_id.as_deref() == Some("go-gzip-writer-unclosed"))
        .collect::<Vec<_>>();
    let findings = scan();
    assert_eq!(findings.len(), 1, "{findings:?}");
    let related = findings[0].related_locations.as_ref().unwrap();
    assert_eq!(related[0].path, "archive/writer.go");
    assert!(related[0].snippet.contains("gzip.NewWriter"));

    std::fs::write(path, caller.replace("Finish(other)", "Finish(alias)")).unwrap();
    assert!(scan().is_empty());
}

#[test]
fn every_call_order_rule_declares_a_category() {
    for rule in discover_call_order_rules() {
        assert!(!rule.category.is_empty(), "{}: kind: call-sequence rule has no category field", rule.id);
    }
}

#[test]
fn applies_pack_call_sequences_to_go_and_web_languages() {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;

    let dir = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    for language in ["Go", "JavaScript", "TypeScript"] {
        std::fs::write(
            pack.path().join(format!("{language}.yml")),
            format!(
                "id: example-{language}\nkind: call-sequence\nlanguage: {language}\ncategory: correctness\nseverity: warning\nmessage: Verify before sending\nafter: [{{call: begin}}]\nunless: [{{call: verify}}]\nbefore: [{{call: send}}]\ncheckBeforeReturn: true\n"
            ),
        ).unwrap();
    }
    let go = "package main\nfunc bad() {\nbegin()\nsend()\n}\nfunc explicit() {\nbegin()\nreturn\n}\nfunc deferred() {\nbegin()\ndefer verify()\nreturn\n}\nfunc earlySend() {\nbegin()\ndefer verify()\nsend()\nreturn\n}\n";
    let web = "function bad() {\nbegin();\nsend();\n}\nfunction clean() {\nbegin();\nverify();\nsend();\n}\n";
    let tsx = "function bad() {\nbegin();\nsend();\nreturn <main />;\n}\n";
    let cases = [
        ("worker.go", "Go", go, &[4, 8, 18][..]),
        ("worker.mjs", "JavaScript", web, &[3][..]),
        ("worker.cjs", "JavaScript", web, &[3][..]),
        ("worker.mts", "TypeScript", web, &[3][..]),
        ("worker.cts", "TypeScript", web, &[3][..]),
        ("view.tsx", "TypeScript", tsx, &[3, 4][..]),
    ];
    let files: Vec<_> = cases.iter().map(|(path, _, source, _)| {
        std::fs::write(dir.path().join(path), source).unwrap();
        (*path).to_owned()
    }).collect();
    let findings = run_dataflow_check(dir.path(), &files, &[ResolvedRulePack {
        id: "call-sequence-parity".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    for (path, language, _, expected) in cases {
        let id = format!("example-{language}");
        let lines: Vec<_> = findings.iter().filter(|finding| finding.location.path == path && finding.source.rule_id.as_deref() == Some(&id))
            .map(|finding| finding.location.range.start_line).collect();
        assert_eq!(lines, expected, "{path}: {findings:#?}");
    }
}

/// Regression test for a real silent-drop bug: `RuleMetadataBlock`
/// documents itself as flowing "verbatim into `AgentFinding.meta`", and
/// that held for pattern rules but not for call-sequence ones — the
/// loader parsed the block and then discarded it, so the CWE mappings
/// these rules declare never reached a report. Asserts against a real
/// rule's real fixture rather than a synthetic one, so it also fails if
/// somebody strips the metadata out of the rule file itself.
#[test]
fn applies_rust_call_sequences_to_branch_and_early_return_paths() {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;

    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("rust.yml"), r#"
id: rust-example-finish
kind: call-sequence
language: Rust
category: correctness
severity: warning
message: Finish before leaving
after: [{call: begin}]
unless: [{call: finish}]
checkBeforeReturn: true
"#).unwrap();
    std::fs::write(repo.path().join("lib.rs"), r#"impl Worker {
    async fn partial(&self, ready: bool) -> Result<(), Error> {
        self.begin();
        if ready {
            self.finish();
        } else {
            check().await?;
        }
        Ok(())
    }
    fn closed(&self) {
        self.begin();
        self.finish();
    }
    fn unreachable(&self) {
        return;
        self.begin();
    }
}"#).unwrap();
    let found = run_dataflow_check(repo.path(), &["lib.rs".into()], &[ResolvedRulePack {
        id: "call-sequence-rust".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    let lines: Vec<_> = found.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("rust-example-finish"))
        .map(|finding| finding.location.range.start_line).collect();
    assert_eq!(lines, [7, 10], "{found:#?}");
}

#[test]
fn a_call_order_rules_declared_cwe_reaches_the_finding() {
    let rule_id = "java-xxe-unconfigured-parser";
    let rule = discover_call_order_rules().into_iter().find(|r| r.id == rule_id).expect("java-xxe-unconfigured-parser is a builtin call-sequence rule");
    let filename = scan_filename_for_language(&rule.language);

    let positive = std::fs::read_to_string(fixtures_dir().join(rule_id).join(format!("positive.{}", extension_for_language(&rule.language)))).expect("positive fixture exists");
    let dir = write_single_file(filename, &positive);
    let findings = run_dataflow_check(dir.path(), &[filename.to_string()], &[]);

    let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some(rule_id)).expect("the positive fixture must fire this rule");
    let meta = finding.meta.as_ref().unwrap_or_else(|| panic!("{rule_id} declares a metadata block, so its finding must carry meta"));
    let cwe = meta.get("cwe").unwrap_or_else(|| panic!("{rule_id} declares cwe metadata; got meta keys {:?}", meta.keys().collect::<Vec<_>>()));
    assert_eq!(cwe, &serde_json::json!(["CWE-611"]), "the rule's declared CWE must reach the finding unchanged");
}
