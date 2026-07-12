//! Data-driven conformance test for every builtin ast-grep rule: each rule
//! gets a positive fixture (must fire, exactly once, with the right rule id)
//! and a negative fixture (must not fire at all). This is the same
//! self-test discipline the plan's rule-factory bench stage calls for
//! ("100% on its own positive/negative test files") applied to the builtin
//! rules we ship today — adding a new rule means adding a row to this table,
//! not writing a new test function.
//!
//! Requires the real `ast-grep` binary; skips (not fails) when it's absent,
//! so this suite is safe to run anywhere but exercises real integration
//! wherever the tool is installed.

use std::process::Command;

use autoreview_core::run_ast_grep;

struct RuleCase {
    rule_id: &'static str,
    filename: &'static str,
    positive: &'static str,
    negative: &'static str,
}

const CASES: &[RuleCase] = &[
    RuleCase {
        rule_id: "go-no-self-comparison",
        filename: "main.go",
        positive: "package main\n\nfunc main() {\n\tx := 1\n\tif x == x {\n\t\tprintln(\"bug\")\n\t}\n}\n",
        negative: "package main\n\nfunc main() {\n\tx := 1\n\ty := 2\n\tif x == y {\n\t\tprintln(\"fine\")\n\t}\n}\n",
    },
    RuleCase {
        rule_id: "go-empty-error-check",
        filename: "main.go",
        positive: "package main\n\nfunc doIt() error { return nil }\n\nfunc main() {\n\tif err := doIt(); err != nil {\n\t}\n}\n",
        negative: "package main\n\nimport \"fmt\"\n\nfunc doIt() error { return nil }\n\nfunc main() {\n\tif err := doIt(); err != nil {\n\t\tfmt.Println(err)\n\t}\n}\n",
    },
    RuleCase {
        rule_id: "java-self-comparison",
        filename: "Sample.java",
        positive: "public class Sample {\n    boolean check(int x) {\n        return x == x;\n    }\n}\n",
        negative: "public class Sample {\n    boolean check(int x, int y) {\n        return x == y;\n    }\n}\n",
    },
    RuleCase {
        rule_id: "java-empty-catch-block",
        filename: "Sample.java",
        positive: "public class Sample {\n    void run() {\n        try {\n            doThing();\n        } catch (Exception e) {\n        }\n    }\n    void doThing() {}\n}\n",
        negative: "public class Sample {\n    void run() {\n        try {\n            doThing();\n        } catch (Exception e) {\n            e.printStackTrace();\n        }\n    }\n    void doThing() {}\n}\n",
    },
    RuleCase {
        rule_id: "kotlin-avoid-not-null-assertion",
        filename: "Sample.kt",
        positive: "fun main() {\n    val s: String? = null\n    println(s!!.length)\n}\n",
        negative: "fun main() {\n    val s: String? = null\n    println(s?.length)\n}\n",
    },
    RuleCase {
        rule_id: "kotlin-empty-catch-block",
        filename: "Sample.kt",
        positive: "fun run() {\n    try {\n        doThing()\n    } catch (e: Exception) {\n    }\n}\nfun doThing() {}\n",
        negative: "fun run() {\n    try {\n        doThing()\n    } catch (e: Exception) {\n        println(e)\n    }\n}\nfun doThing() {}\n",
    },
];

fn ast_grep_available() -> bool {
    Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
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

    let mut failures = Vec::new();

    for case in CASES {
        let positive_dir = write_single_file(case.filename, case.positive);
        let positive_findings = run_ast_grep(positive_dir.path(), &[case.filename.to_string()]).unwrap();
        let positive_matches: Vec<_> = positive_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(case.rule_id)).collect();
        if positive_matches.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 match on positive fixture, got {} (all findings: {:?})",
                case.rule_id,
                positive_matches.len(),
                positive_findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
            ));
        }

        let negative_dir = write_single_file(case.filename, case.negative);
        let negative_findings = run_ast_grep(negative_dir.path(), &[case.filename.to_string()]).unwrap();
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(case.rule_id)).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", case.rule_id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "rule conformance failures:\n{}", failures.join("\n"));
}
