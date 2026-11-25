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
    category: String,
    contract: Option<ContractMeta>,
}

#[derive(Debug, Deserialize)]
struct ContractMeta { source: Option<String> }

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

fn discover_call_order_rules() -> Vec<RuleMeta> {
    autoreview_core::list_builtin_rules().into_iter()
        .filter(|entry| entry.kind == "call-sequence")
        .map(|entry| {
            let definition = autoreview_core::find_rule_definition(&entry.id, &[]).unwrap();
            serde_yaml::from_str(&definition.yaml).unwrap()
        })
        .collect()
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        "Rust" => "rs",
        "JavaScript" => "js",
        "TypeScript" => "ts",
        "Tsx" => "tsx",
        other => panic!("missing call-sequence fixture extension for {other:?}"),
    }
}

fn scan_filename_for_language(language: &str) -> String {
    format!("Main.{}", extension_for_language(language))
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_call_order_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    let rules = discover_call_order_rules();
    assert!(!rules.is_empty(), "expected built-in call-sequence rules");

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

        // Typed sources require a multi-file TypeScript project and a trusted
        // compiler; keyed_imports tests run these fixtures with that setup.
        if rule.contract.as_ref().and_then(|contract| contract.source.as_deref()) == Some("typed-promise-factory") {
            continue;
        }

        let positive_dir = write_single_file(&filename, &positive);
        let positive_findings = run_dataflow_check(positive_dir.path(), std::slice::from_ref(&filename), &[]);
        let positive_matches: Vec<_> = positive_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if positive_matches.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 match on positive fixture, got {} (all findings: {:?})",
                rule.id,
                positive_matches.len(),
                positive_findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
            ));
        }

        let negative_dir = write_single_file(&filename, &negative);
        let negative_findings = run_dataflow_check(negative_dir.path(), std::slice::from_ref(&filename), &[]);
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
fn ignored_results_follow_imported_helper_arguments_and_returns() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("helper")).unwrap();
    std::fs::write(root.path().join("go.mod"), "module example.com/project\n\ngo 1.22\n").unwrap();
    std::fs::write(root.path().join("helper/helper.go"), r#"package helper
import "os"
func Ignore(err error) {}
func Check(err error) { if err != nil { panic(err) } }
func Forward(err error) error { return err }
func Move(a, b string) error { return os.Rename(a, b) }
func Bridge(a, b string) error { return Move(a, b) }
"#).unwrap();
    std::fs::write(root.path().join("main.go"), r#"package main
import (
    "os"
    "example.com/project/helper"
)
func discarded(a, b string) {
    err := os.Rename(a, b)
    helper.Ignore(err)
}
func direct(a, b string) {
    helper.Ignore(os.Rename(a, b))
}

func checked(a, b string) {
    err := os.Rename(a, b)
    helper.Check(err)
}
func forwarded(a, b string) error {
    err := os.Rename(a, b)
    return helper.Forward(err)
}
func ignoredReturn(a, b string) {
    helper.Bridge(a, b)
}
"#).unwrap();
    let found = run_dataflow_check(root.path(), &["main.go".into()], &[]);
    let mut locations: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    locations.sort_unstable();
    assert_eq!(locations, [("helper/helper.go", 6), ("main.go", 7), ("main.go", 11)], "{found:#?}");
    let helper_finding = found.iter().find(|finding| finding.location.path == "helper/helper.go"
        && finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error")).unwrap();
    assert!(helper_finding.related_locations.iter().flatten()
        .any(|location| location.path == "main.go" && location.range.start_line == 23), "{helper_finding:#?}");
}

#[test]
fn ignored_results_follow_helpers_through_web_reexports() {
    for (extension, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let root = tempfile::tempdir().unwrap();
        let helper = format!("helper.{extension}");
        let barrel = format!("index.{extension}");
        let caller = format!("main.{extension}");
        std::fs::write(root.path().join(&helper), "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n").unwrap();
        std::fs::write(root.path().join(&barrel), "export { move as moveFile } from './helper';\n").unwrap();
        std::fs::write(root.path().join(&caller), "import { moveFile } from './index';\nfunction ignored(a, b) { moveFile(a, b); }\n").unwrap();
        let found = run_dataflow_check(root.path(), std::slice::from_ref(&caller), &[]);
        let finding = found.iter().find(|item| item.source.rule_id.as_deref() == Some(rule)).expect("ignored promise through barrel");
        assert_eq!(finding.location.path, helper);
        assert!(finding.related_locations.iter().flatten().any(|location| location.path == caller), "{found:#?}");
        std::fs::write(root.path().join(&caller), "import { moveFile } from './index';\nasync function handled(a, b) { await moveFile(a, b); }\n").unwrap();
        let clean = run_dataflow_check(root.path(), &[caller], &[]);
        assert!(!clean.iter().any(|item| item.source.rule_id.as_deref() == Some(rule)), "{clean:#?}");
    }
    let root = tempfile::tempdir().unwrap();
    let helper = "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n";
    std::fs::write(root.path().join("a.js"), helper).unwrap();
    std::fs::write(root.path().join("b.js"), helper).unwrap();
    std::fs::write(root.path().join("index.js"), "export * from './a'; export * from './b';\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { move } from './index';\nfunction ignored(a, b) { move(a, b); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise") && issue.path.as_deref() == Some("main.js")), "{:?}", result.coverage);
}

#[test]
fn ignored_results_follow_exported_arrow_helpers() {
    for (extension, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let root = tempfile::tempdir().unwrap();
        let caller = format!("main.{extension}");
        let helper = format!("helper.{extension}");
        std::fs::write(root.path().join(&helper), "import { rename } from 'node:fs/promises';\nexport const move = (a, b) => rename(a, b);\n").unwrap();
        std::fs::write(root.path().join(format!("index.{extension}")), "export * from './helper';\n").unwrap();
        std::fs::write(root.path().join(&caller), "import { move } from './index';\nfunction ignored(a, b) { move(a, b); }\n").unwrap();
        let found = run_dataflow_check(root.path(), std::slice::from_ref(&caller), &[]);
        let finding = found.iter().find(|item| item.source.rule_id.as_deref() == Some(rule)).expect("ignored promise from arrow helper");
        assert_eq!(finding.location.path, helper);
        assert!(finding.related_locations.iter().flatten().any(|location| location.path == caller), "{found:#?}");
        std::fs::write(root.path().join(&caller), "import { move } from './index';\nasync function handled(a, b) { await move(a, b); }\n").unwrap();
        let clean = run_dataflow_check(root.path(), &[caller], &[]);
        assert!(!clean.iter().any(|item| item.source.rule_id.as_deref() == Some(rule)), "{clean:#?}");
    }
}

#[test]
fn helper_return_demand_is_kept_separate_for_each_caller() {
    let source = r#"package storage
import "os"
func moveFile(a, b string) error {
    return os.Rename(a, b)
}
func ignored(a, b string) {
    moveFile(a, b)
}
func handled(a, b string) error {
    return moveFile(a, b)
}
func mixed(a, b string) error {
    moveFile(a, b)
    return moveFile(a, b)
}
"#;
    let root = write_single_file("storage.go", source);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    let matches: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| finding.location.range.start_line)
        .collect();
    assert_eq!(matches, [4], "{found:#?}");
    let mixed_only = source.replace("func ignored(a, b string) {\n    moveFile(a, b)\n}\n", "");
    let root = write_single_file("storage.go", &mixed_only);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    assert!(found.iter().any(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error")),
        "a used call must not hide a discarded call in the same function: {found:#?}");
}

fn assert_helper_locations(root: &Path, caller: &str, rule: &str, expected: &[(&str, u32)]) {
    let found = run_dataflow_check(root, &[caller.into()], &[]);
    let mut locations: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    locations.sort_unstable();
    assert_eq!(locations, expected, "{rule}: {found:#?}");
}

#[test]
fn imported_web_helpers_track_unused_promises_in_all_web_grammars() {
    for (extension, rule) in [
        ("js", "javascript-node-fs-discarded-promise"),
        ("ts", "typescript-node-fs-discarded-promise"),
        ("tsx", "typescript-node-fs-discarded-promise"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = format!("helper.{extension}");
        let caller = format!("worker.{extension}");
        std::fs::write(root.path().join(&helper), r#"import { rename } from "node:fs/promises";
export function discard(value) {}
export function forward(value) { return value; }
export function move(a, b) { return rename(a, b); }
"#).unwrap();
        std::fs::write(root.path().join(&caller), r#"import { rename } from "node:fs/promises";
import { discard, forward, move } from "./helper";
export function unused(a, b) {
  const result = rename(a, b);
  discard(result);
}
export function used(a, b) {
  const result = rename(a, b);
  return forward(result);
}
export function discardedReturn(a, b) {
  move(a, b);
}
"#).unwrap();
        assert_helper_locations(root.path(), &caller, rule, &[(helper.as_str(), 4), (caller.as_str(), 4)]);
    }
}

#[test]
fn imported_jvm_helpers_preserve_result_usage() {
    let cases = [
        (
            "Java", "java-discarded-objects-comparison", "app/Worker.java", "support/Checks.java",
            r#"package support;
import java.util.Objects;
public class Checks {
    public static void discard(boolean value) {}
    public static boolean forward(boolean value) { return value; }
    public static boolean compare(Object a, Object b) { return Objects.equals(a, b); }
}
"#,
            r#"package app;
import java.util.Objects;
import support.Checks;
class Worker {
    void unused(Object a, Object b) {
        boolean same = Objects.equals(a, b);
        Checks.discard(same);
    }
    boolean used(Object a, Object b) {
        boolean same = Objects.equals(a, b);
        return Checks.forward(same);
    }
    void dropped(Object a, Object b) { Checks.compare(a, b); }
}
"#,
            6, 6,
        ),
        (
            "Kotlin", "kotlin-discarded-run-catching-result", "app/Worker.kt", "support/Checks.kt",
            r#"package support
import kotlin.runCatching
fun discard(value: Result<Int>) {}
fun forward(value: Result<Int>): Result<Int> = value
fun parse(value: String): Result<Int> = runCatching { value.toInt() }
"#,
            r#"package app
import kotlin.runCatching
import support.discard
import support.forward
import support.parse
fun unused(text: String) {
    val parsed = runCatching { text.toInt() }
    discard(parsed)
}
fun used(text: String): Result<Int> {
    val parsed = runCatching { text.toInt() }
    return forward(parsed)
}
fun dropped(text: String) { parse(text) }
"#,
            7, 5,
        ),
    ];
    for (_language, rule, caller, helper, helper_source, caller_source, caller_line, helper_line) in cases {
        let root = tempfile::tempdir().unwrap();
        for (path, source) in [(caller, caller_source), (helper, helper_source)] {
            let file = root.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, source).unwrap();
        }
        assert_helper_locations(root.path(), caller, rule, &[(caller, caller_line), (helper, helper_line)]);
    }
}

#[test]
fn imported_rust_helpers_preserve_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/support.rs"), r#"use std::fs::remove_file;
pub(crate) fn discard(value: std::io::Result<()>) {}
pub(crate) fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }
pub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }
"#).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), r#"mod support;
use std::fs::remove_file;
fn unused(path: &str) {
    let result = remove_file(path);
    support::discard(result);
}
fn used(path: &str) -> std::io::Result<()> {
    let result = remove_file(path);
    support::forward(result)
}
fn dropped(path: &str) {
    support::remove(path);
}
"#).unwrap();
    assert_helper_locations(root.path(), "src/lib.rs", "rust-discarded-remove-file-result", &[("src/lib.rs", 4), ("src/support.rs", 4)]);
}

#[test]
fn recursive_helpers_propagate_argument_use_and_returned_results() {
    let source = r#"package storage
import "os"
func discardAtEnd(err error, depth int) {
    if depth > 0 { discardAtEnd(err, depth - 1) }
}
func checkAtEnd(err error, depth int) {
    if depth > 0 { checkAtEnd(err, depth - 1); return }
    if err != nil { panic(err) }
}
func moveAtEnd(a, b string, depth int) error {
    if depth > 0 { return moveAtEnd(a, b, depth - 1) }
    return os.Rename(a, b)
}
func unused(a, b string) {
    err := os.Rename(a, b)
    discardAtEnd(err, 3)
}
func used(a, b string) {
    err := os.Rename(a, b)
    checkAtEnd(err, 3)
}
func discardedReturn(a, b string) {
    moveAtEnd(a, b, 3)
}
func handledReturn(a, b string) error {
    return moveAtEnd(a, b, 3)
}
"#;
    let root = write_single_file("storage.go", source);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    let mut lines: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| finding.location.range.start_line).collect();
    lines.sort_unstable();
    assert_eq!(lines, [12, 15], "{found:#?}");
}

const ASSIGNED_RESULT_CASES: [(&str, &str, &str); 4] = [
    ("move.go", "go-os-rename-discarded-error", r#"package storage
import "os"
func overwritten(from, to string) error {
    var err, alias = os.Rename(from, to), error(nil) // unused result
    err, alias = nil, err
    alias = err
    return alias
}
func swapped(from, to string) error {
    var err, alias = os.Rename(from, to), error(nil)
    err, alias = nil, err
    return alias
}
func checked(from, to string) error {
    if err := os.Rename(from, to); err != nil { return err }
    return nil
}
func named(from, to string) (err error) {
    err = os.Rename(from, to)
    return
}
func deferred(from, to string) {
    err := os.Rename(from, to)
    defer inspect(err)
}
func blank(from, to string) {
    err := os.Rename(from, to) // unused result
    _ = err
}
"#),
    ("Main.kt", "kotlin-discarded-run-catching-result", r#"import kotlin.runCatching
fun parse(s: String, active: Boolean): Int {
    val result = runCatching { s.toInt() }
    if (active) return result.getOrThrow()
    return 0
}
fun show(s: String) {
    val parsed = runCatching { s.toInt() }
    println("Read: $parsed")
}
fun overwrite(s: String): Int {
    var parsed = runCatching { s.toInt() } // unused result
    parsed = runCatching { 0 }
    return parsed.getOrThrow()
}
"#),
    ("lib.rs", "rust-discarded-remove-file-result", r#"use std::fs::remove_file;
fn overwrite(path: &str, spare: &str, refresh: bool) -> std::io::Result<()> {
    let mut removed = remove_file(path); // unused result
    if refresh {
        removed = remove_file(spare);
    } else {
        removed = Ok(());
    }
    removed
}
fn log(path: &str) {
    let removed = remove_file(path);
    println!("{removed:?}");
}
fn propagate(path: &str) -> std::io::Result<()> {
    let _removed = remove_file(path)?;
    Ok(())
}
fn shadow(path: &str) -> std::io::Result<()> {
    let removed = remove_file(path); // unused result
    let removed = Ok(());
    removed
}
fn forward(path: &str) -> std::io::Result<()> {
    let outcome = remove_file(path);
    let outcome = outcome;
    outcome
}
"#),
    ("Page.tsx", "typescript-node-fs-discarded-promise", r#"import { rename } from 'node:fs/promises';
export async function Page(from: string, to: string) {
    const result = rename(from, to); // unused result
    return <main><result /></main>;
    await result;
}
export async function Render(from: string, to: string) {
    const pending = rename(from, to);
    try { return <main />; } finally { await pending; }
}
export function Schedule(from: string, to: string) {
    let pending = rename(from, to);
    schedule(() => pending.catch(report));
}
const preview = rename => {
    const pending = rename('fake', 'never');
};
export const Tile = (from: string, to: string) => {
    const pending = rename(from, to); // unused result
    return <aside />;
};
"#),
];

#[test]
fn assigned_results_are_used_only_by_reachable_reads_of_the_original_value() {
    for (path, rule, source) in ASSIGNED_RESULT_CASES {
        let repo = write_single_file(path, source);
        let findings = run_dataflow_check(repo.path(), &[path.into()], &[]);
        let mut matches: Vec<_> = findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
            .map(|finding| finding.location.range.start_line).collect();
        matches.sort_unstable();
        let expected: Vec<_> = source.lines().enumerate().filter(|(_, text)| text.contains("// unused result"))
            .map(|(index, _)| index as u32 + 1).collect();
        assert_eq!(matches, expected, "{path}: {findings:#?}");
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
    let dir = write_single_file(&filename, &positive);
    let findings = run_dataflow_check(dir.path(), std::slice::from_ref(&filename), &[]);

    let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some(rule_id)).expect("the positive fixture must fire this rule");
    let meta = finding.meta.as_ref().unwrap_or_else(|| panic!("{rule_id} declares a metadata block, so its finding must carry meta"));
    let cwe = meta.get("cwe").unwrap_or_else(|| panic!("{rule_id} declares cwe metadata; got meta keys {:?}", meta.keys().collect::<Vec<_>>()));
    assert_eq!(cwe, &serde_json::json!(["CWE-611"]), "the rule's declared CWE must reach the finding unchanged");
}
