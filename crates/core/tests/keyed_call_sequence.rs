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
             after: [{{call: register}}, {{callRegex: '^root[.]add$'}}]\nunless: [{{call: remove}}, {{callRegex: '^root[.]drop$'}}]\nbefore: [{{call: finish}}]\n\
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

#[test]
fn alias_chains_match_receiver_scoped_rules_and_literal_keys() {
    let source = r#"function pending(root, other, ready) {
    const shared = root;
    const alias = shared;
    const key = "pending";
    alias.add(key);
    if (ready) {
        shared.drop("pending");
    } else {
        other.remove(key);
    }
    return;
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(
        hits[0].message,
        "Clean up root and \"pending\" before return"
    );
    let clean = source.replace("other.remove(key)", "alias.drop(key)");
    assert!(findings("JavaScript", "worker.js", &clean, RECEIVER, true).is_empty());
    let rebound = source
        .replace("const alias = shared;", "let alias = shared;")
        .replace("other.remove(key);", "alias.drop(key); alias = other;");
    assert!(findings("JavaScript", "worker.js", &rebound, RECEIVER, true).is_empty());
}

#[test]
fn positional_aliases_keep_the_collection_and_key_distinct() {
    let source = r#"function pending(root, other, first, second) {
    const collection = root;
    const key = first;
    register(collection, value(), key);
    remove(other, value(), key);
    remove(collection, value(), second);
    return;
}"#;
    let hits = findings("TypeScript", "worker.ts", source, ARGUMENT, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and first before return");
    let clean = source.replace(
        "remove(collection, value(), second)",
        "remove(root, value(), first)",
    );
    assert!(findings("TypeScript", "worker.ts", &clean, ARGUMENT, true).is_empty());
}

#[test]
fn rust_reborrowing_a_collection_keeps_its_identity() {
    let source = r#"fn pending(key: &str) {
    let mut root = Registry::new();
    root.register(key);
    let alias = &mut root;
    alias.remove(&key);
}"#;
    assert!(findings("Rust", "lib.rs", source, RECEIVER, true).is_empty());
    let wrong = source.replace("alias.remove(&key)", "alias.remove(&\"other\")");
    let hits = findings("Rust", "lib.rs", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and key before return");
}

#[test]
fn finally_runs_before_return_throw_and_fallthrough() {
    let source = r#"function pending(root, key, ready, failed) {
    const alias = root;
    root.register(key);
    try {
        if (ready) return;
        if (failed) throw new Error();
    } finally {
        alias.remove(key);
    }
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replace("alias.remove(key)", "alias.remove(\"other\")");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    let mut exits: Vec<_> = hits
        .iter()
        .map(|hit| hit.location.range.start_line)
        .collect();
    exits.sort_unstable();
    assert_eq!(exits, [5, 6, 10], "{hits:#?}");
}

#[test]
fn nested_finally_preserves_the_original_exit_path() {
    let source = r#"function pending(root, key, ready) {
    root.register(key);
    try {
        try {
            if (ready) return;
        } finally {
            const note = "finished";
        }
        root.remove(key);
    } finally {
        if (!ready) root.remove("other");
    }
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 5);
}

#[test]
fn an_exit_in_finally_replaces_the_earlier_return() {
    let source = r#"function pending(root, key) {
    root.register(key);
    try {
        return;
    } finally {
        root.remove("other");
        throw new Error();
    }
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 7);
    assert_eq!(hits[0].message, "Clean up root and key before throw");
}

#[test]
fn jvm_finally_handles_aliases_and_unbraced_returns() {
    let cases = [
        ("Java", "Worker.java", "class Worker {\nvoid pending(Registry root, String key, boolean ready) {\nvar alias = root;\nroot.register(key);\ntry { if (ready) return; }\nfinally { alias.remove(key); }\n}}", "alias.remove(key)", "alias.remove(\"other\")"),
        ("Kotlin", "Worker.kt", "fun pending(root: Registry, key: String, ready: Boolean) {\nval alias = root\nroot.register(key)\ntry { if (ready) return }\nfinally { alias.remove(key) }\n}", "alias.remove(key)", "alias.remove(\"other\")"),
    ];
    for (language, file, code, cleanup, wrong) in cases {
        assert!(
            findings(language, file, code, RECEIVER, true).is_empty(),
            "{language}"
        );
        let hits = findings(
            language,
            file,
            &code.replace(cleanup, wrong),
            RECEIVER,
            true,
        );
        let mut lines: Vec<_> = hits
            .iter()
            .map(|hit| hit.location.range.start_line)
            .collect();
        lines.sort_unstable();
        let expected = if language == "Java" { [5, 7] } else { [4, 6] };
        assert_eq!(lines, expected, "{language}: {hits:#?}");
    }
}

#[test]
fn go_defer_is_branch_local_and_leaves_before_checks_active() {
    let source = r#"package worker
func pending(root *Registry, key string, ready bool) {
    alias := root
    saved := key
    root.register(key)
    if ready {
        defer alias.remove(saved)
        root.finish(key)
    } else {
        root.remove(key)
    }
    return
}"#;
    let hits = findings("Go", "worker.go", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 8);
    let source = source
        .replace("alias := root", "var alias = root")
        .replace("defer alias.remove(saved)", "{ defer alias.remove(saved) }");
    let partial = source.replace("root.remove(key)", "root.remove(\"wrong\")");
    let hits = findings("Go", "worker.go", &partial, RECEIVER, true);
    assert_eq!(hits.len(), 2, "{hits:#?}");
    assert!(hits.iter().any(|hit| hit.location.range.start_line == 12));
}

#[test]
fn go_defer_before_registration_covers_only_the_paths_where_it_was_armed() {
    let source = r#"package worker
func pending(root, other *Registry, key, wrong string, ready bool) {
    if ready {
        defer root.remove(key)
        root.register(key)
    } else {
        defer other.remove(key)
        defer root.remove(wrong)
    }
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let bad = source.replace(
        "defer root.remove(wrong)",
        "root.register(key)\n        defer root.remove(wrong)",
    );
    let hits = findings("Go", "worker.go", &bad, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and key before fallthrough");
    assert_eq!(
        hits[0].related_locations.as_ref().unwrap()[0]
            .range
            .start_line,
        8
    );
}

#[test]
fn a_straight_line_go_deferred_closure_covers_its_own_pairs() {
    let source = r#"package worker
func pending(root *Registry, key string, ready bool) {
    root.register(key)
    defer func() { root.remove(key) }()
}"#;
    let wrong = source.replace("root.remove(key)", "root.remove(\"wrong\")");
    let hits = findings("Go", "worker.go", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and key before fallthrough");
    let conditional = source.replace("root.remove(key)", "if ready { root.remove(key) }");
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let hits = findings("Go", "worker.go", &conditional, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
}

#[test]
fn a_finalizer_that_cleans_only_one_branch_leaves_the_other_pending() {
    let source = r#"function pending(root, key, clean) {
    root.register(key);
    try {
        return;
    } finally {
        if (clean) root.remove(key);
    }
}"#;
    let hits = findings("TypeScript", "worker.tsx", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 4);
}

#[test]
fn reassignment_tracks_the_collection_and_key_together() {
    let source = r#"function pending(root, other, first, second, flip) {
    let collection = root;
    let key = first;
    if (flip) {
        collection = other;
        key = second;
        other.register(second);
    } else {
        root.register(first);
    }
    collection.remove(key);
    return;
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replace("collection = other;", "collection = root;");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up other and second before return");
}

#[test]
fn reassigning_a_binding_preserves_the_identity_copied_before_it() {
    let source = r#"function pending(root, other, key) {
    const saved = root;
    const original = key;
    root.register(key);
    root = other;
    key = "different";
    saved.remove(original);
    return;
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replace("saved.remove(original)", "root.remove(key)");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and key before return");
}

#[test]
fn a_new_collection_does_not_share_the_previous_values_identity() {
    let source = r#"function pending(root, key) {
    const saved = root;
    root.register(key);
    root = new Map();
    root.remove(key);
    return;
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert!(
        hits.iter().any(|hit| hit.location.range.start_line == 6),
        "{hits:#?}"
    );
    let clean = source.replace(
        "    root = new Map();",
        "    saved.remove(key);\n    root = new Map();",
    );
    assert!(findings("JavaScript", "worker.js", &clean, RECEIVER, true).is_empty());
}

#[test]
fn rust_reassignment_of_a_key_keeps_the_old_obligation() {
    let source = r#"fn pending(root: &mut Registry, mut key: &str) {
    let saved = key;
    root.register(key);
    key = "different";
    root.remove(&key);
}"#;
    let hits = findings("Rust", "lib.rs", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].message, "Clean up root and key before return");
    let clean = source.replace("remove(&key)", "remove(&saved)");
    assert!(findings("Rust", "lib.rs", &clean, RECEIVER, true).is_empty());
}

#[test]
fn opaque_calls_can_leave_through_catch_before_normal_cleanup() {
    let source = r#"async function pending(root, key) {
    root.register(key);
    try {
        await work();
    } catch (failure) {
        root.remove(key);
        return;
    }
    await root.remove(key);
}"#;
    assert!(findings("TypeScript", "worker.ts", source, RECEIVER, true).is_empty());
    let wrong = source.replace(
        "        root.remove(key);",
        "        root.remove(\"other\");",
    );
    let hits = findings("TypeScript", "worker.ts", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].location.range.start_line, 7);
}

#[test]
fn finally_cleans_an_implicit_exception_without_hiding_wrong_pairs() {
    let source = r#"function pending(root, key) {
    root.register(key);
    try {
        work();
    } catch (failure) {
        throw failure;
    } finally {
        root.remove(key);
    }
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let missing = source.replace("root.remove(key);", "const ignored = true;");
    let hits = findings("JavaScript", "worker.js", &missing, RECEIVER, true);
    let mut lines: Vec<_> = hits
        .iter()
        .map(|hit| hit.location.range.start_line)
        .collect();
    lines.sort_unstable();
    assert_eq!(lines, [6, 10], "{hits:#?}");
}

#[test]
fn implicit_throw_does_not_run_later_normal_cleanup() {
    let source = r#"function pending(root, key) {
    root.register(key);
    work();
    root.remove(key);
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(
        hits[0].message,
        "Clean up root and key before implicit throw"
    );
    assert_eq!(hits[0].location.range.start_line, 3);
}

#[test]
fn typed_catches_route_known_exceptions_and_run_finally_after_handlers() {
    let cases = [
        ("Java", "Worker.java", "class Worker { void pending(Registry root, String key) {\nroot.register(key);\ntry { throw new BadException(); }\ncatch (BadException e) { root.remove(key); }\nfinally { root.finish(key); }\n}}"),
        ("Kotlin", "Worker.kt", "fun pending(root: Registry, key: String) {\nroot.register(key)\ntry { throw BadException() } catch (e: BadException) { root.remove(key) } finally { root.finish(key) }\n}"),
    ];
    for (language, path, code) in cases {
        assert!(
            findings(language, path, code, RECEIVER, true).is_empty(),
            "{language}"
        );
        let wrong = code.replace("remove(key)", "remove(\"other\")");
        let hits = findings(language, path, &wrong, RECEIVER, true);
        assert_eq!(hits.len(), 2, "{language}: {hits:#?}");
        assert!(
            hits.iter().any(|hit| hit.message.ends_with("root.finish")),
            "{language}: {hits:#?}"
        );
    }
}

#[test]
fn go_direct_defer_copies_arguments_and_closures_read_them_at_exit() {
    let source = r#"package worker
func pending(root *Registry, first, second string) {
    key := first
    root.register(first)
    defer root.remove(key)
    key = second
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let closure = source.replace(
        "defer root.remove(key)",
        "defer func() { root.remove(key) }()",
    );
    let hits = findings("Go", "worker.go", &closure, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(
        hits[0].message,
        "Clean up root and first before fallthrough"
    );
}

#[test]
fn go_conditional_defer_respects_branch_guards_and_lifo_order() {
    let source = r#"package worker
func pending(root *Registry, first, second string, ready bool) {
    if !ready { return }
    root.register(first)
    key := second
    defer func() {
        if ready {
            root.remove(key)
        }
    }()
    defer func() { key = first }()
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let wrong = source.replace("key = first", "key = second");
    let hits = findings("Go", "worker.go", &wrong, RECEIVER, true);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(
        hits[0].message,
        "Clean up root and first before fallthrough"
    );
}
