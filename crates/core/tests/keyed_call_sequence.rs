use autoreview_core::{run_dataflow_check, ResolvedRulePack};
use autoreview_schema::{AgentFinding, RulePackTrust};

fn findings(
    language: &str,
    path: &str,
    source: &str,
    identity: &str,
    check_returns: bool,
) -> Vec<AgentFinding> {
    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join(path), source).unwrap();
    std::fs::write(
        pack.path().join("pending.yml"),
        format!(
            "id: pending-entry\nkind: call-sequence\nlanguage: {language}\ncategory: correctness\nseverity: warning\n\
             message: 'Clean up {{collection}} and {{key}} before {{trigger_call}}'\n\
             after: [{{call: register}}]\nunless: [{{call: remove}}]\nbefore: [{{call: finish}}]\n\
             checkBeforeReturn: {check_returns}\nidentity: {identity}\n"
        ),
    ).unwrap();
    run_dataflow_check(
        repo.path(),
        &[path.into()],
        &[ResolvedRulePack {
            id: "entries".into(),
            local_path: pack.path().into(),
            trust: RulePackTrust::Full,
        }],
    )
    .into_iter()
    .filter(|finding| finding.source.rule_id.as_deref() == Some("pending-entry"))
    .collect()
}

const RECEIVER: &str = "{receiver: true, keyArgument: 0}";
const ARGUMENT: &str = "{collectionArgument: 0, keyArgument: 2}";

#[test]
fn cleanup_preserves_other_collections_keys_and_paths() {
    let source = r#"function pending(active, spare, partial) {
    active.register("first");
    active.register("second");
    spare.register("first");
    if (partial) {
        active.remove('first');
    } else {
        active.remove("second");
    }
    spare.remove("first");
    return;
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 2, "{hits:#?}");
    for (hit, (key, registration)) in hits.iter().zip([("first", 2), ("second", 3)]) {
        assert_eq!(hit.location.range.start_line, 11);
        assert_eq!(
            hit.message,
            format!("Clean up active and {key:?} before return")
        );
        assert_eq!(
            hit.related_locations.as_ref().unwrap()[0].range.start_line,
            registration
        );
    }
}

#[test]
fn a_wrong_collection_and_key_do_not_satisfy_cleanup() {
    let source = r#"function pending(active: Map<string, unknown>, spare: Map<string, unknown>) {
    register(active, makeValue(), "first");
    remove(spare, makeValue(), "first");
    remove(active, makeValue(), "second");
}"#;
    let hits = findings("TypeScript", "worker.tsx", source, ARGUMENT, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 5);
    assert_eq!(
        hits[0].message,
        "Clean up active and \"first\" before fallthrough"
    );
}

#[test]
fn cleanup_on_every_branch_clears_the_entry() {
    let source = r#"function complete(active, key, ready) {
    active.register(key);
    if (ready) {
        active.remove(key);
        return;
    } else {
        active.remove(key);
    }
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
}

#[test]
fn before_checks_only_the_trigger_collection_and_key() {
    let source = r#"function pending(active, spare) {
    active.register("first");
    spare.finish("first");
    active.finish("second");
    active.finish("first");
    active.remove("first");
    active.finish("first");
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, false);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 5);
    assert_eq!(
        hits[0].message,
        "Clean up active and \"first\" before active.finish"
    );
}

#[test]
fn uncertain_identity_or_cleanup_leaves_no_finding() {
    for body in [
        "registry.register(key); key = replacement; registry.remove(key);",
        "registry.register(key); { const key = newKey(); registry.remove(key); }",
        "registry.register(key); { const { key } = other; registry.remove(key); }",
        "registry.register(key); const alias = registry; alias.remove(key);",
        "registry.register(key); registry.remove(newKey());",
        "registry.register(key); return registry.remove(key);",
        "registry.register(key); if (registry.remove(key)) return;",
        "registry.register(key); try { return; } finally { registry.remove(key); }",
        "registry.register(key); registry?.remove(key);",
        "registry.register(key); registry.remove(...args);",
        "return; registry.register(key);",
    ] {
        let source = format!("function clean(registry, key, replacement, args) {{ {body} }}");
        let hits = findings("JavaScript", "worker.js", &source, RECEIVER, true);
        assert!(hits.is_empty(), "{body}: {hits:#?}");
    }
}

#[test]
fn stable_local_collections_and_numeric_keys_are_supported() {
    let source = r#"function pending() {
    const active = new Registry();
    const spare = new Registry();
    active.register(4);
    spare.remove(4);
    active.remove(5);
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up active and 4 before fallthrough");
}

#[test]
fn receiver_calls_work_in_go_java_kotlin_and_rust() {
    let cases = [
        ("Go", "worker.go", "package worker\nfunc pending(active *Registry, key string, ready bool) {\nactive.register(key)\nif ready { active.remove(key) }\nreturn\n}", 5),
        ("Java", "Worker.java", "class Worker {\nvoid pending(Registry active, String key, boolean ready) {\nactive.register(key);\nif (ready) { active.remove(key); }\nreturn;\n}}", 5),
        ("Kotlin", "Worker.kt", "fun pending(active: Registry, key: String, ready: Boolean) {\nactive.register(key)\nif (ready) { active.remove(key) }\nreturn\n}", 4),
        ("Rust", "lib.rs", "fn pending(active: &mut Registry, key: String, ready: bool) {\nactive.register(key);\nif ready { active.remove(&key); }\nreturn;\n}", 4),
    ];
    for (language, path, source, line) in cases {
        let hits = findings(language, path, source, RECEIVER, true);
        assert_eq!(hits.len(), 1, "{language}: {hits:#?}");
        assert_eq!(hits[0].location.range.start_line, line, "{language}");
        assert_eq!(
            hits[0].message, "Clean up active and key before return",
            "{language}"
        );
        let complete = source
            .replace("if ready { active.remove(&key); }", "active.remove(&key);")
            .replace("if (ready) { active.remove(key); }", "active.remove(key);")
            .replace("if (ready) { active.remove(key) }", "active.remove(key)")
            .replace("if ready { active.remove(key) }", "active.remove(key)");
        assert!(
            findings(language, path, &complete, RECEIVER, true).is_empty(),
            "{language}: {complete}"
        );
    }
}

#[test]
fn rust_question_mark_checks_the_early_return() {
    let source = r#"fn pending(active: &mut Registry, key: &str) -> Result<(), Error> {
    active.register(key);
    check()?;
    active.remove(key);
    Ok(())
}"#;
    let hits = findings("Rust", "lib.rs", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 3);
}
