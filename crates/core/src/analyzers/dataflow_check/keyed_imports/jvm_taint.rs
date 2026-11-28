use std::collections::BTreeSet;
use std::path::Path;

use autoreview_dataflow::call_order::{check_jvm_taint_helpers, KeyedFunction, KeyedUnit};
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::{AgentFinding, AnalysisCoverageIssue};

use super::super::{dsl::helper_taint_finding, functions::lower_keyed_graphs, taint_rules::TaintRuleDef};
use super::{Resolver, SummaryCaches};

pub(in crate::analyzers::dataflow_check) fn run_taint(
    root: &Path, paths: &[String], rules: &[TaintRuleDef], caches: &SummaryCaches<'_>,
) -> (Vec<AgentFinding>, Vec<AnalysisCoverageIssue>) {
    let rules: Vec<_> = rules.iter().filter_map(|rule| {
        let language = Language::from_rule_name(&rule.language)?;
        matches!(language, Language::Java | Language::Kotlin).then_some((rule, language))
    }).collect();
    if rules.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Ok(root) = root.canonicalize() else { return (Vec::new(), Vec::new()) };
    let resolver = Resolver { root, cache: caches.parse, packages: caches };
    let entries: BTreeSet<_> = paths.iter().filter_map(|path| {
        let language = language_for_path(Path::new(path))?;
        rules.iter().any(|(_, declared)| language.matches_rule_language(*declared))
            .then(|| resolver.relative(&resolver.root.join(path))).flatten()
    }).collect();
    if entries.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (sources, mut coverage) = resolver.load(&entries);
    for issue in &mut coverage {
        issue.reason = format!("jvm_taint_{}", issue.reason);
        issue.detail = issue.detail.replace("call-sequence", "JVM helper taint");
    }
    let lowered: Vec<_> = sources.values().map(|file| lower_keyed_graphs(file.language, &file.parsed, false)).collect();
    let functions: Vec<_> = lowered.iter().map(|(_, file)| {
        file.iter().map(|(node, graph)| KeyedFunction { node: *node, graph }).collect::<Vec<_>>()
    }).collect();
    let mut findings = Vec::new();
    for (rule, declared) in rules {
        let units: Vec<_> = sources.iter().zip(&functions).map(|((path, file), functions)| KeyedUnit {
            path,
            source: file.parsed.0.as_bytes(),
            functions,
            imports: &file.imports,
            exports: &file.exports,
            entry: entries.contains(path) && file.language.matches_rule_language(declared),
        }).collect();
        if units.iter().any(|unit| unit.entry) {
            findings.extend(check_jvm_taint_helpers(&rule.spec, &units).into_iter().map(|hit| helper_taint_finding(rule, hit)));
        }
    }
    (findings, coverage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::dataflow_check::run_dataflow_check;
    use crate::rule_packs::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;

    fn review(language: &str, files: &[(&str, &str)], entry: &str) -> Vec<AgentFinding> {
        let repo = tempfile::tempdir().unwrap();
        for (file, source) in files {
            let path = repo.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let pack = tempfile::tempdir().unwrap();
        std::fs::write(pack.path().join("rulepack.yaml"), "id: helper-test\nversion: '1.0.0'\n").unwrap();
        std::fs::write(pack.path().join("helper.yml"), format!(
            "id: helper-test-taint\nkind: taint\nlanguage: {language}\ncategory: security\nseverity: error\nmessage: '{{tainted_arg}} reaches {{sink_call}}'\nsources:\n  - call: readInput\nsinks:\n  - call: sensitiveWrite\n    argPositions: [1]\nsanitizers:\n  - call: validatedValue\n"
        )).unwrap();
        let packs = [ResolvedRulePack { id: "helper-test".into(), local_path: pack.path().to_path_buf(), trust: RulePackTrust::Full }];
        run_dataflow_check(repo.path(), &[entry.into()], &packs).into_iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some("helper-test-taint")).collect()
    }

    #[test]
    fn java_tracks_arguments_through_imported_helpers_and_respects_validation() {
        let findings = review("Java", &[
            ("app/Entry.java", r#"package app;
import helpers.Relay;
class Entry {
    void handle() {
        String input = readInput();
        Relay.forward("label", input);
        Relay.validated(input);
        Relay.forward(input, "literal");
        Relay.overloaded(input);
    }
}"#),
            ("helpers/Relay.java", r#"package helpers;
public class Relay {
    public static void forward(String tag, String input) { Terminal.store(input); }
    public static void validated(String input) { Terminal.storeValidated(input); }
    public static void overloaded(String input) { Terminal.store(input); }
    public static void overloaded(Object input) {}
}"#),
            ("helpers/Terminal.java", r#"package helpers;
class Terminal {
    static void store(String raw) { sensitiveWrite("audit", raw); }
    static void storeValidated(String raw) {
        String clean = validatedValue(raw);
        sensitiveWrite("audit", clean);
    }
}"#),
        ], "app/Entry.java");
        assert_single_path(&findings, "app/Entry.java", 6, "helpers/Terminal.java", 3);
    }

    #[test]
    fn kotlin_binds_reordered_named_arguments_at_each_helper_hop() {
        let findings = review("Kotlin", &[
            ("app/Entry.kt", r#"package app
import helpers.forward as relay
fun handle() {
    val input = readInput()
    relay(payload = input, label = "audit")
    relay(payload = "literal", label = input)
    relay("audit", payload = input)
}"#),
            ("helpers/Relay.kt", r#"package helpers
fun forward(label: String, payload: String) {
    store(raw = payload, topic = label)
}"#),
            ("helpers/Terminal.kt", r#"package helpers
fun store(topic: String, raw: String) {
    sensitiveWrite(topic, raw)
}"#),
        ], "app/Entry.kt");
        assert_eq!(findings.len(), 2, "{findings:#?}");
        for (finding, line) in findings.iter().zip([5, 7]) {
            assert_single_path(std::slice::from_ref(finding), "app/Entry.kt", line, "helpers/Terminal.kt", 3);
        }
    }

    #[test]
    fn kotlin_named_helper_arguments_stay_clean_after_validation() {
        let findings = review("Kotlin", &[
            ("app/Entry.kt", r#"package app
import helpers.forward
fun handle() {
    val input = readInput()
    forward(payload = input, label = "audit")
}"#),
            ("helpers/Relay.kt", r#"package helpers
fun forward(label: String, payload: String) {
    val clean = validatedValue(payload)
    store(raw = clean, topic = label)
}"#),
            ("helpers/Terminal.kt", r#"package helpers
fun store(topic: String, raw: String) {
    sensitiveWrite(topic, raw)
}"#),
        ], "app/Entry.kt");
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn kotlin_named_helper_calls_decline_competing_default_overloads() {
        let findings = review("Kotlin", &[
            ("app/Entry.kt", r#"package app
import helpers.forward
fun handle() {
    val input = readInput()
    forward(payload = input)
}"#),
            ("helpers/Relay.kt", r#"package helpers
fun forward(payload: Any) {
    sensitiveWrite("audit", payload)
}"#),
            ("helpers/OtherRelay.kt", r#"package helpers
fun forward(payload: String, label: String = "audit") {}"#),
        ], "app/Entry.kt");
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn kotlin_tracks_import_aliases_and_respects_validation_and_argument_order() {
        let findings = review("Kotlin", &[
            ("app/Entry.kt", r#"package app
import helpers.forward as relay
import helpers.forwardValidated
import helpers.store as masked
fun handle() {
    val input = readInput()
    relay("label", input)
    forwardValidated(input)
    relay(input, "literal")
    relay(input = "literal", label = input)
    fun masked(input: String) {}
    masked(input)
}"#),
            ("helpers/Relay.kt", r#"package helpers
fun forward(label: String, input: String) {
    store(input)
}
fun forwardValidated(input: String) {
    storeValidated(input)
}"#),
            ("helpers/Terminal.kt", r#"package helpers
fun store(raw: String) {
    sensitiveWrite("audit", raw)
}
fun storeValidated(raw: String) {
    var clean = raw
    clean = validatedValue(clean)
    sensitiveWrite("audit", clean)
}"#),
        ], "app/Entry.kt");
        assert_single_path(&findings, "app/Entry.kt", 7, "helpers/Terminal.kt", 3);
    }

    fn assert_single_path(findings: &[AgentFinding], caller: &str, line: u32, sink: &str, sink_line: u32) {
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].location.path, caller);
        assert_eq!(findings[0].location.range.start_line, line);
        assert!(findings[0].title.contains("`input`"));
        let related = findings[0].related_locations.as_ref().unwrap();
        assert_eq!(related[0].path, sink);
        assert_eq!(related[0].range.start_line, sink_line);
    }
}
