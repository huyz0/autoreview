use autoreview_core::{run_dataflow_check, ResolvedRulePack};
use autoreview_schema::{AgentFinding, RulePackTrust};

fn findings(
    language: &str,
    path: &str,
    source: &str,
    identity: &str,
    check_returns: bool,
) -> Vec<AgentFinding> {
    workspace_findings(
        language,
        &[(path, source)],
        &[path],
        identity,
        check_returns,
    )
}

fn workspace_findings(
    language: &str,
    files: &[(&str, &str)],
    entries: &[&str],
    identity: &str,
    check_returns: bool,
) -> Vec<AgentFinding> {
    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    for &(path, source) in files {
        let file = repo.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
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
        &entries
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>(),
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

#[track_caller]
fn one(hits: &[AgentFinding]) -> &AgentFinding {
    let [hit] = hits else {
        panic!("expected one finding, got {hits:#?}")
    };
    hit
}

fn finding_lines(hits: &[AgentFinding]) -> Vec<u32> {
    let mut lines: Vec<_> = hits
        .iter()
        .map(|hit| hit.location.range.start_line)
        .collect();
    lines.sort_unstable();
    lines
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
    register(active, 0, "first");
    remove(spare, 0, "first");
    remove(active, 0, "second");
}"#;
    let hits = findings("TypeScript", "worker.tsx", source, ARGUMENT, true);
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 5);
    assert_eq!(
        hit.message,
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 5);
    assert_eq!(
        hit.message,
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up active and 4 before fallthrough");
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 3);
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up root and \"pending\" before return");
    let clean = source.replace("other.remove(key)", "alias.drop(key)");
    assert_clean_js(&clean);
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
    register(collection, 0, key);
    remove(other, 0, key);
    remove(collection, 0, second);
    return;
}"#;
    let hits = findings("TypeScript", "worker.ts", source, ARGUMENT, true);
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up root and first before return");
    let clean = source.replace(
        "remove(collection, 0, second)",
        "remove(root, 0, first)",
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
    assert_key_pending_on_return(&hits);
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
    let exits = finding_lines(&hits);
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 5);
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 7);
    assert_eq!(hit.message, "Clean up root and key before throw");
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
        let lines = finding_lines(&hits);
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 8);
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up root and key before fallthrough");
    assert_eq!(
        hit.related_locations.as_ref().unwrap()[0].range.start_line,
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up root and key before fallthrough");
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 4);
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up other and second before return");
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
    assert_key_pending_on_return(&hits);
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
    assert_clean_js(&clean);
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
    assert_key_pending_on_return(&hits);
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
    let hit = one(&hits);
    assert_eq!(hit.location.range.start_line, 7);
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
    let lines = finding_lines(&hits);
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
    let hit = one(&hits);
    assert_eq!(hit.message, "Clean up root and key before implicit throw");
    assert_eq!(hit.location.range.start_line, 3);
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
    assert_go_first_missing(&hits);
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
    assert_go_first_missing(&hits);
}

#[track_caller]
fn assert_go_first_missing(hits: &[AgentFinding]) {
    assert_eq!(
        one(hits).message,
        "Clean up root and first before fallthrough"
    );
}

#[test]
fn jvm_reassignments_follow_mutable_local_aliases() {
    let cases = [
        ("Java", "Worker.java", "class Worker { void pending(Registry root, String key) { var selected = key; root.register(selected); selected = \"changed\"; root.remove(selected); }}"),
        ("Kotlin", "Worker.kt", "fun pending(root: Registry, key: String) { var selected = key; root.register(selected); selected = \"changed\"; root.remove(selected) }"),
    ];
    for (language, path, code) in cases {
        let hits = findings(language, path, code, RECEIVER, true);
        assert_eq!(
            one(&hits).message,
            "Clean up root and key before fallthrough",
            "{language}"
        );
        let clean = code.replace("remove(selected)", "remove(key)");
        assert!(
            findings(language, path, &clean, RECEIVER, true).is_empty(),
            "{language}"
        );
    }
}

#[track_caller]
fn assert_key_pending_on_return(hits: &[AgentFinding]) {
    assert_eq!(one(hits).message, "Clean up root and key before return");
}

#[test]
fn helper_chains_transfer_registration_and_remove_the_callers_pair() {
    let source = r#"function begin(active, key) {
    active.register(key);
}
function removeEntry(collection, key) {
    collection.remove(key);
}
function finishEntry(active, key) {
    removeEntry(active, key);
}
function pending(root, spare, first, second, ready) {
    begin(root, first);
    if (ready) {
        finishEntry(root, first);
    } else {
        finishEntry(root, first);
    }
    return;
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replacen("finishEntry(root, first)", "finishEntry(spare, second)", 1);
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_eq!(one(&hits).message, "Clean up root and first before return");
    assert_eq!(
        one(&hits).related_locations.as_ref().unwrap()[0]
            .range
            .start_line,
        2
    );
}

#[test]
fn a_throwing_helper_carries_its_registration_into_the_callers_catch() {
    let source = r#"function begin(active, key) {
    active.register(key);
    throw new Error();
}
function pending(root, key) {
    try {
        begin(root, key);
    } catch (failure) {
        root.remove(key);
    }
}"#;
    assert!(findings("TypeScript", "worker.ts", source, RECEIVER, true).is_empty());
    let wrong = source.replace("root.remove(key)", "root.remove(\"wrong\")");
    let hits = findings("TypeScript", "worker.ts", &wrong, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
}

#[test]
fn a_caller_can_defer_a_local_helper_with_captured_arguments() {
    let source = r#"package worker
func cleanup(active *Registry, key string) {
    active.remove(key)
}
func pending(root, spare *Registry, key string) {
    root.register(key)
    defer cleanup(root, key)
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let wrong = source.replace("defer cleanup(root, key)", "defer cleanup(spare, key)");
    let hits = findings("Go", "worker.go", &wrong, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
}

#[test]
fn java_and_rust_resolve_direct_helpers_by_scope_and_arity() {
    let cases = [
        (
            "Java",
            "Worker.java",
            r#"class Other {
    void cleanup(Registry root, String key) { root.remove("wrong"); }
}
class Worker {
    void cleanup(Registry active) { active.remove("unused"); }
    void cleanup(Registry active, String key) { active.remove(key); }
    void pending(Registry root, String key) {
        root.register(key);
        cleanup(root, key);
    }
}"#,
        ),
        (
            "Rust",
            "lib.rs",
            r#"fn cleanup(active: &mut Registry, key: &str) { active.remove(&key); }
fn pending(root: &mut Registry, key: &str) {
    root.register(key);
    cleanup(root, key);
}"#,
        ),
    ];
    for (language, path, source) in cases {
        assert!(
            findings(language, path, source, RECEIVER, true).is_empty(),
            "{language}"
        );
        let wrong = source
            .replace("active.remove(key)", "active.remove(\"wrong\")")
            .replace("active.remove(&key)", "active.remove(\"wrong\")");
        let hits = findings(language, path, &wrong, RECEIVER, true);
        assert_eq!(
            one(&hits).related_locations.as_ref().unwrap()[0].snippet,
            "root.register(key);",
            "{language}"
        );
    }
}

#[test]
fn cleanup_in_a_later_iteration_does_not_clear_an_earlier_key() {
    let source = r#"function pending(root, keys, skip) {
    for (const key of keys) {
        root.register(key);
        if (skip) continue;
        root.remove(key);
    }
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
    let clean = source.replace(
        "if (skip) continue;",
        "if (skip) { root.remove(key); continue; }",
    );
    assert_clean_js(&clean);
}

#[test]
fn loop_back_edges_keep_the_previous_key_distinct_from_the_current_key() {
    let source = r#"function pending(root, keys, initial) {
    let previous = initial;
    for (const key of keys) {
        root.register(key);
        root.remove(previous);
        previous = key;
    }
}"#;
    let hits = findings("TypeScript", "worker.ts", source, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
    let clean = source.replace(
        "previous = key;\n    }",
        "previous = key;\n    }\n    root.remove(previous);",
    );
    assert!(findings("TypeScript", "worker.ts", &clean, RECEIVER, true).is_empty());
}

#[test]
fn break_and_do_while_keep_reachable_exits_only() {
    let source = r#"function pending(root, key) {
    do {
        root.register(key);
        break;
        root.remove(key);
    } while (false);
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
    let clean = source.replace(
        "break;\n        root.remove(key);",
        "root.remove(key);\n        break;",
    );
    assert_clean_js(&clean);
}

#[test]
fn iterator_keys_work_with_helpers_across_supported_languages() {
    let cases = [
        ("Go", "worker.go", "package worker\nfunc cleanup(active *Registry, key string) { active.remove(key) }\nfunc pending(root *Registry, keys []string) {\nfor _, key := range keys { root.register(key); cleanup(root, key) }\n}"),
        ("Java", "Worker.java", "class Worker {\nvoid cleanup(Registry active, String key) { active.remove(key); }\nvoid pending(Registry root, String[] keys) {\nfor (String key : keys) { root.register(key); cleanup(root, key); }\n}}"),
        ("Kotlin", "Worker.kt", "fun cleanup(active: Registry, key: String) { active.remove(key) }\nfun pending(root: Registry, keys: List<String>) {\nfor (key in keys) { root.register(key); cleanup(root, key) }\n}"),
        ("Rust", "lib.rs", "fn cleanup(active: &mut Registry, key: &str) { active.remove(key); }\nfn pending(root: &mut Registry, keys: Vec<&str>) {\nfor key in keys { root.register(key); cleanup(root, key); }\n}"),
    ];
    for (language, path, source) in cases {
        let hits = findings(language, path, source, RECEIVER, true);
        assert!(hits.is_empty(), "{language}: {hits:#?}");
        let wrong = source.replace("active.remove(key)", "active.remove(\"different\")");
        let hits = findings(language, path, &wrong, RECEIVER, true);
        assert!(
            one(&hits).message.starts_with("Clean up root and key"),
            "{language}"
        );
    }
}

#[test]
fn loop_jumps_run_finally_before_leaving_or_repeating_the_iteration() {
    let source = r#"function pending(root, keys, stop) {
    for (const key of keys) {
        root.register(key);
        try {
            if (stop) break;
            continue;
        } finally {
            root.remove(key);
        }
    }
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replace("root.remove(key)", "root.remove(\"wrong\")");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_key_pending_on_fallthrough(&hits);
}

#[test]
fn a_go_helper_used_as_a_return_value_runs_before_the_exit_check() {
    let source = r#"package worker
func cleanup(active *Registry, key string) error {
    active.remove(key)
    return nil
}
func pending(root *Registry, key string) error {
    root.register(key)
    return cleanup(root, key)
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let wrong = source.replace(
        "return cleanup(root, key)",
        "return cleanup(root, \"wrong\")",
    );
    assert_key_pending_on_return(&findings("Go", "worker.go", &wrong, RECEIVER, true));
}

#[test]
fn ordinary_for_updates_keep_current_and_previous_counters_distinct() {
    let source = r#"function pending(root, limit) {
    for (let i = 0; i < limit; i++) {
        root.register(i);
        root.remove(i);
    }
}"#;
    assert!(findings("JavaScript", "worker.js", source, RECEIVER, true).is_empty());
    let wrong = source.replace("root.remove(i)", "root.remove(0)");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_eq!(one(&hits).message, "Clean up root and i before fallthrough");
}

#[track_caller]
fn assert_key_pending_on_fallthrough(hits: &[AgentFinding]) {
    assert_eq!(
        one(hits).message,
        "Clean up root and key before fallthrough"
    );
}

#[track_caller]
fn assert_clean_js(source: &str) {
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert!(hits.is_empty(), "{hits:#?}");
}

#[test]
fn named_and_namespace_imports_keep_cross_file_registration_and_exit_locations() {
    let helpers = r#"export function begin(active, key) {
    active.register(key);
}
export function removeFrom(active, key) {
    active.remove(key);
}"#;
    let caller = r#"import { begin as start } from "./helpers";
import * as support from "./helpers";
function pending(root, other, key) {
    start(root, key);
    support.removeFrom(other, key);
}"#;
    let files = [("app.ts", caller), ("helpers.ts", helpers)];
    let hits = workspace_findings(
        "TypeScript",
        &files,
        &["app.ts", "helpers.ts"],
        RECEIVER,
        true,
    );
    let found = one(&hits);
    assert_eq!(found.location.path, "app.ts");
    let registration = &found.related_locations.as_ref().unwrap()[0];
    assert_eq!(registration.path, "helpers.ts");
    assert_eq!(registration.range.start_line, 2);
    assert_eq!(registration.snippet, "active.register(key);");
    let clean = caller.replace("removeFrom(other, key)", "removeFrom(root, key)");
    let files = [("app.ts", clean.as_str()), ("helpers.ts", helpers)];
    assert!(workspace_findings("TypeScript", &files, &["app.ts"], RECEIVER, true).is_empty());
}

#[test]
fn imported_return_values_flow_through_forwarders_and_keep_their_pairs() {
    let first = r#"import { choose as selectedKey } from "./keys.js";
export function forwardKey(key) { return selectedKey(key); }
export function collection(active) { return active; }"#;
    let keys = r#"export function choose(key) { return key; }"#;
    let caller = r#"import { collection, forwardKey } from "./forward";
function pending(root, spare, key) {
    root.register(key);
    const alias = collection(root);
    const same = forwardKey(key);
    alias.remove(same);
}"#;
    let files = [
        ("worker.ts", caller),
        ("forward.ts", first),
        ("keys.ts", keys),
    ];
    assert!(workspace_findings("TypeScript", &files, &["worker.ts"], RECEIVER, true).is_empty());
    let wrong = caller.replace("collection(root)", "collection(spare)");
    let files = [
        ("worker.ts", wrong.as_str()),
        ("forward.ts", first),
        ("keys.ts", keys),
    ];
    assert_key_pending_on_fallthrough(&workspace_findings(
        "TypeScript",
        &files,
        &["worker.ts"],
        RECEIVER,
        true,
    ));
}

#[test]
fn branch_dependent_helper_returns_preserve_the_callers_obligation() {
    let source = r#"function select(first, second, useFirst) {
    if (useFirst) return first;
    return second;
}
function pending(root, original, alternative, flag) {
    root.register(original);
    const chosen = select(original, alternative, flag);
    root.remove(chosen);
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert_eq!(
        one(&hits).message,
        "Clean up root and original before fallthrough"
    );
    let clean = source.replace("return second", "return first");
    assert_clean_js(&clean);
}

#[test]
fn returned_identity_is_evaluated_before_finally_rebinds_the_parameter() {
    let source = r#"function pick(first, second) {
    try {
        return first;
    } finally {
        first = second;
    }
}
function pending(root, key, other) {
    root.register(key);
    const alias = pick(key, other);
    root.remove(alias);
}"#;
    assert_clean_js(source);
    let wrong = source.replace("first = second;", "return second;");
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", &wrong, RECEIVER, true));
}

#[test]
fn go_import_aliases_and_same_package_helpers_return_identities() {
    let library = "package helpers\nfunc Choose(key string) string { return key }";
    let sibling =
        "package app\nfunc selectCollection(active *Registry) *Registry { return active }";
    let caller = r#"package app
import tools "example.test/worker/lookup"
func pending(root *Registry, key string) {
    root.register(key)
    alias := selectCollection(root)
    chosen := tools.Choose(key)
    alias.remove(chosen)
}"#;
    let module = "module example.test/worker\n\ngo 1.22";
    let files = [
        ("go.mod", module),
        ("lookup/lookup.go", library),
        ("collector.go", sibling),
        ("worker.go", caller),
    ];
    assert!(workspace_findings("Go", &files, &["worker.go"], RECEIVER, true).is_empty());
    let wrong = caller.replace("tools.Choose(key)", "tools.Choose(\"other\")");
    let files = [
        ("go.mod", module),
        ("lookup/lookup.go", library),
        ("collector.go", sibling),
        ("worker.go", &wrong),
    ];
    assert_key_pending_on_fallthrough(&workspace_findings(
        "Go",
        &files,
        &["worker.go"],
        RECEIVER,
        true,
    ));
}

#[test]
fn go_helper_discovery_skips_unrelated_files_but_follows_transitive_calls() {
    let mut owned = vec![
        ("go.mod".to_owned(), "module example.test/worker\n\ngo 1.22".to_owned()),
        ("worker.go".to_owned(), r#"package app
import tools "example.test/worker/lookup"
func pending(root *Registry, key string) {
    root.register(key)
    chosen := tools.Choose(key)
    root.remove(chosen)
}"#.to_owned()),
        ("lookup/z_choose.go".to_owned(), "package lookup\nfunc Choose(key string) string { return forward(key) }".to_owned()),
        ("lookup/z_forward.go".to_owned(), "package lookup\nfunc forward(key string) string { return key }".to_owned()),
    ];
    for index in 0..80 {
        owned.push((format!("lookup/a_unused_{index:03}.go"), format!("package lookup\nfunc Unused{index:03}() {{}}")));
    }
    let files: Vec<_> = owned.iter().map(|(path, source)| (path.as_str(), source.as_str())).collect();
    assert!(workspace_findings("Go", &files, &["worker.go"], RECEIVER, true).is_empty());
    owned[3].1 = "package lookup\nfunc forward(key string) string { return \"other\" }".to_owned();
    let files: Vec<_> = owned.iter().map(|(path, source)| (path.as_str(), source.as_str())).collect();
    assert_key_pending_on_fallthrough(&workspace_findings("Go", &files, &["worker.go"], RECEIVER, true));
}

#[test]
fn java_static_imports_and_kotlin_aliases_follow_returned_keys() {
    let cases = [
        ("Java", "app/Worker.java", "support/Picker.java",
         "package support;\npublic class Picker { public static String pick(String key) { return key; } }",
         "package app;\nimport static support.Picker.pick;\nclass Worker { void pending(Registry root, String key) { root.register(key); var chosen = pick(key); root.remove(chosen); }}"),
        ("Kotlin", "app/Worker.kt", "support/Picker.kt",
         "package support\nfun pick(key: String): String = key",
         "package app\nimport support.pick as choose\nfun pending(root: Registry, key: String) { root.register(key); val chosen = choose(key); root.remove(chosen) }"),
    ];
    for (language, path, helper, library, caller) in cases {
        assert!(
            workspace_findings(
                language,
                &[(path, caller), (helper, library)],
                &[path],
                RECEIVER,
                true
            )
            .is_empty(),
            "{language}"
        );
        let wrong = caller
            .replace("pick(key)", "pick(\"other\")")
            .replace("choose(key)", "choose(\"other\")");
        assert_key_pending_on_fallthrough(&workspace_findings(
            language,
            &[(path, &wrong), (helper, library)],
            &[path],
            RECEIVER,
            true,
        ));
    }
}

#[test]
fn rust_module_paths_and_use_aliases_follow_tail_returns() {
    let library = r#"pub(crate) fn key(value: &str) -> &str { value }
pub(crate) fn collection(value: &mut Registry) -> &mut Registry { value }"#;
    let caller = r#"mod support;
use crate::support::key as select;
fn pending(root: &mut Registry, key: &str) {
    root.register(key);
    let alias = support::collection(root);
    let chosen = select(key);
    alias.remove(&chosen);
}"#;
    let files = [("src/lib.rs", caller), ("src/support.rs", library)];
    assert!(workspace_findings("Rust", &files, &["src/lib.rs"], RECEIVER, true).is_empty());
    let wrong = caller.replace("select(key)", "select(\"other\")");
    assert_key_pending_on_return(&workspace_findings(
        "Rust",
        &[("src/lib.rs", &wrong), ("src/support.rs", library)],
        &["src/lib.rs"],
        RECEIVER,
        true,
    ));
}

#[test]
fn repeated_imported_allocations_do_not_alias_each_other() {
    let library = "export function create() { return new Map(); }";
    let caller = r#"import { create } from "./maps";
function pending(key) {
    const first = create();
    const second = create();
    first.register(key);
    second.remove(key);
}"#;
    let files = [("app.ts", caller), ("maps.ts", library)];
    let hits = workspace_findings("TypeScript", &files, &["app.ts"], RECEIVER, true);
    assert_eq!(
        one(&hits).message,
        "Clean up returned collection and key before fallthrough"
    );
    let clean = caller.replace("second.remove(key)", "first.remove(key)");
    assert!(workspace_findings(
        "TypeScript",
        &[("app.ts", &clean), ("maps.ts", library)],
        &["app.ts"],
        RECEIVER,
        true
    )
    .is_empty());
}

#[test]
fn extensionless_jsx_imports_follow_helper_results() {
    let caller = "import { choose } from './picker';\nfunction pending(root, key) { root.register(key); const chosen = choose(key); root.remove(chosen); }";
    let clean = "export function choose(key) { return key; }";
    let wrong = "export function choose(key) { return 'other'; }";
    assert!(workspace_findings("JavaScript", &[("app.js", caller), ("picker.jsx", clean)], &["app.js"], RECEIVER, true).is_empty());
    assert_key_pending_on_fallthrough(&workspace_findings("JavaScript", &[("app.js", caller), ("picker.jsx", wrong)], &["app.js"], RECEIVER, true));
}

#[test]
fn nested_helper_results_preserve_cleanup_keys_across_languages() {
    let cases = [
        ("JavaScript", "worker.js", "function choose(key) { return key; }\nfunction pending(root, key) { root.register(key); root.remove(choose(key)); }", "return key", "return 'other'"),
        ("TypeScript", "worker.ts", "function choose(key: string) { return key; }\nfunction pending(root: Registry, key: string) { root.register(key); root.remove(choose(key)); }", "return key", "return 'other'"),
        ("TypeScript", "worker.tsx", "function choose(key: string) { return key; }\nfunction pending(root: Registry, key: string) { root.register(key); root.remove(choose(key)); }", "return key", "return 'other'"),
        ("Go", "worker.go", "package worker\nfunc choose(key string) string { return key }\nfunc pending(root *Registry, key string) { root.register(key); root.remove(choose(key)) }", "return key", "return \"other\""),
        ("Java", "Worker.java", "class Worker { String choose(String key) { return key; } void pending(Registry root, String key) { root.register(key); root.remove(choose(key)); } }", "return key", "return \"other\""),
        ("Kotlin", "Worker.kt", "fun choose(key: String): String { return key }\nfun pending(root: Registry, key: String) { root.register(key); root.remove(choose(key)) }", "return key", "return \"other\""),
        ("Rust", "worker.rs", "fn choose(key: &str) -> &str { key }\nfn pending(root: &mut Registry, key: &str) { root.register(key); root.remove(choose(key)); }", "{ key }", "{ \"other\" }"),
        ("Rust", "borrowed.rs", "fn choose(key: &str) -> &str { key }\nfn pending(root: &mut Registry, key: &str) { root.register(key); root.remove(&choose(key)); }", "{ key }", "{ \"other\" }"),
    ];
    for (language, path, clean, from, to) in cases {
        assert!(findings(language, path, clean, RECEIVER, true).is_empty(), "clean {language} {path}");
        let wrong = clean.replace(from, to);
        let hits = findings(language, path, &wrong, RECEIVER, true);
        assert!(!hits.is_empty(), "wrong key in {language} {path}: {hits:?}");
    }
}

#[test]
fn multiple_nested_helper_arguments_preserve_collection_and_key_identity() {
    let source = r#"function collection(root) { return root; }
function keyFor(key) { return key; }
function cleanup(root, key) { root.remove(key); }
function pending(root, spare, key) {
    root.register(key);
    cleanup(collection(root), keyFor(key));
}"#;
    assert_clean_js(source);
    let wrong_key = source.replace("keyFor(key));", "keyFor('other'));");
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", &wrong_key, RECEIVER, true));
    let wrong_collection = source.replace("collection(root),", "collection(spare),");
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", &wrong_collection, RECEIVER, true));
}

#[test]
fn nested_collection_and_key_results_work_with_positional_selectors() {
    let source = r#"function collection(root) { return root; }
function keyFor(key) { return key; }
function pending(root, spare, key) {
    register(root, 0, key);
    remove(collection(root), 0, keyFor(key));
}"#;
    assert!(findings("JavaScript", "worker.js", source, ARGUMENT, true).is_empty());
    let wrong = source.replace("collection(root), 0", "collection(spare), 0");
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", &wrong, ARGUMENT, true));
}

#[test]
fn nested_helper_branches_do_not_borrow_each_others_results() {
    let source = r#"function choose(key, wrong) { if (wrong) return 'other'; return key; }
function pending(root, key, wrong) {
    root.register(key);
    root.remove(choose(key, wrong));
}"#;
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", source, RECEIVER, true));
    let clean = source.replace("if (wrong) return 'other';", "if (wrong) return key;");
    assert_clean_js(&clean);
}

#[test]
fn nested_helper_chains_and_loops_keep_results_per_call_site() {
    let source = r#"function choose(key) { return key; }
function pending(root, keys) {
    for (const key of keys) {
        root.register(key);
        root.remove(choose(choose(key)));
    }
}"#;
    assert_clean_js(source);
    let wrong = source.replace("choose(choose(key))", "choose(choose('other'))");
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", &wrong, RECEIVER, true));
}

#[test]
fn nested_helper_returns_are_followed_through_expression_bodies() {
    let cases = [
        ("JavaScript", "worker.js", "function choose(key) { return key; }\nfunction wrap(key) { return choose(choose(key)); }\nfunction pending(root, key) { root.register(key); root.remove(wrap(key)); }", "return key", "return 'other'"),
        ("Kotlin", "worker.kt", "fun choose(key: String): String { return key }\nfun wrap(key: String): String = choose(choose(key))\nfun pending(root: Registry, key: String) { root.register(key); root.remove(wrap(key)) }", "return key", "return \"other\""),
    ];
    for (language, path, source, from, to) in cases {
        assert!(findings(language, path, source, RECEIVER, true).is_empty(), "{language} clean");
        let hits = findings(language, path, &source.replace(from, to), RECEIVER, true);
        assert!(!hits.is_empty(), "{language} wrong return: {hits:?}");
    }
}

#[test]
fn nested_helper_results_follow_imports_between_files() {
    let web = "import { choose } from './helpers';\nfunction pending(root, key) { root.register(key); root.remove(choose(key)); }";
    let clean = "export function choose(key) { return key; }";
    let wrong = "export function choose(key) { return 'other'; }";
    assert!(workspace_findings("JavaScript", &[("worker.js", web), ("helpers.js", clean)], &["worker.js"], RECEIVER, true).is_empty());
    assert_key_pending_on_fallthrough(&workspace_findings("JavaScript", &[("worker.js", web), ("helpers.js", wrong)], &["worker.js"], RECEIVER, true));

    let go = "package app\nimport \"example.test/app/helpers\"\nfunc pending(root *Registry, key string) { root.register(key); root.remove(helpers.Choose(key)) }";
    let module = "module example.test/app\n\ngo 1.22";
    let clean = "package helpers\nfunc Choose(key string) string { return key }";
    let wrong = "package helpers\nfunc Choose(key string) string { return \"other\" }";
    assert!(workspace_findings("Go", &[("go.mod", module), ("worker.go", go), ("helpers/helpers.go", clean)], &["worker.go"], RECEIVER, true).is_empty());
    assert_key_pending_on_fallthrough(&workspace_findings("Go", &[("go.mod", module), ("worker.go", go), ("helpers/helpers.go", wrong)], &["worker.go"], RECEIVER, true));
}

#[test]
fn web_barrel_reexports_follow_helper_results_across_files() {
    let leaf = "export function choose(key) { return key; }";
    let wrong_leaf = "export function choose(key) { return 'other'; }";
    for (language, caller_path, barrel_path, leaf_path) in [
        ("JavaScript", "worker.js", "index.js", "helpers.js"),
        ("TypeScript", "worker.ts", "index.ts", "helpers.ts"),
        ("TypeScript", "worker.tsx", "index.ts", "helpers.ts"),
    ] {
        for (barrel, import, call) in [
            ("export * from './helpers';", "import { choose as pick } from './index';", "pick"),
            ("export { choose as pick } from './helpers';", "import { pick } from './index';", "pick"),
            ("import { choose } from './helpers'; export { choose as pick };", "import { pick } from './index';", "pick"),
            ("export * from './helpers';", "import * as support from './index';", "support.choose"),
        ] {
            let caller = format!("{import}\nfunction pending(root, key) {{ root.register(key); root.remove({call}(key)); }}");
            let clean = [(caller_path, caller.as_str()), (barrel_path, barrel), (leaf_path, leaf)];
            assert!(workspace_findings(language, &clean, &[caller_path], RECEIVER, true).is_empty(), "clean {language} {barrel}");
            let wrong = [(caller_path, caller.as_str()), (barrel_path, barrel), (leaf_path, wrong_leaf)];
            let hits = workspace_findings(language, &wrong, &[caller_path], RECEIVER, true);
            assert!(!hits.is_empty(), "wrong return through {language} {barrel}: {hits:?}");
        }
    }
}

#[test]
fn web_reexports_support_cycles_and_default_aliases() {
    let caller = "import { pick } from './index';\nfunction pending(root, key) { root.register(key); root.remove(pick(key)); }";
    let cycle = "export * from './index';";
    for (index, helper) in [
        ("export * from './cycle'; export { default as pick } from './helper';", "export default function choose(key) { return key; }"),
        ("export * from './cycle'; export { default as pick } from './helper';", "export default (key) => { return key; };"),
        ("export * from './cycle'; export * from './helper';", "export function pick(key) { return key; }"),
    ] {
        let clean = [("worker.js", caller), ("index.js", index), ("cycle.js", cycle), ("helper.js", helper)];
        assert!(workspace_findings("JavaScript", &clean, &["worker.js"], RECEIVER, true).is_empty());
        let wrong = helper.replace("return key", "return 'other'");
        let files = [("worker.js", caller), ("index.js", index), ("cycle.js", cycle), ("helper.js", wrong.as_str())];
        assert_key_pending_on_fallthrough(&workspace_findings("JavaScript", &files, &["worker.js"], RECEIVER, true));
    }
}

#[test]
fn exported_web_function_values_preserve_key_identities() {
    for (language, caller_path, index_path, helper_path) in [
        ("JavaScript", "worker.js", "index.js", "helper.js"),
        ("TypeScript", "worker.ts", "index.ts", "helper.ts"),
        ("TypeScript", "worker.tsx", "index.ts", "helper.ts"),
    ] {
        let caller = "import { choose } from './index';\nfunction pending(root, key) { root.register(key); root.remove(choose(key)); }";
        let index = "export * from './helper';";
        for (clean, wrong) in [
            ("export const choose = key => key;", "export const choose = key => 'other';"),
            ("export const choose = (key) => { return key; };", "export const choose = (key) => { return 'other'; };"),
            ("export const choose = function(key) { return key; };", "export const choose = function(key) { return 'other'; };"),
            ("const choose = key => key; export { choose };", "const choose = key => 'other'; export { choose };"),
        ] {
            let files = [(caller_path, caller), (index_path, index), (helper_path, clean)];
            assert!(workspace_findings(language, &files, &[caller_path], RECEIVER, true).is_empty(), "{language}: {clean}");
            let files = [(caller_path, caller), (index_path, index), (helper_path, wrong)];
            let hits = workspace_findings(language, &files, &[caller_path], RECEIVER, true);
            assert!(!hits.is_empty(), "{language}: {wrong}: {hits:?}");
        }
        if language == "TypeScript" {
            let clean = "export const choose = (key: string): string => key;";
            let wrong = "export const choose = (key: string): string => 'other';";
            assert!(workspace_findings(language, &[(caller_path, caller), (index_path, index), (helper_path, clean)], &[caller_path], RECEIVER, true).is_empty());
            assert_key_pending_on_fallthrough(&workspace_findings(language, &[(caller_path, caller), (index_path, index), (helper_path, wrong)], &[caller_path], RECEIVER, true));
        }
    }
}

#[test]
fn default_exported_arrow_helpers_follow_returns() {
    let caller = "import pick from './helper';\nfunction pending(root, key) { root.register(key); root.remove(pick(key)); }";
    for (clean, wrong) in [
        ("export default key => key;", "export default key => 'other';"),
        ("export default function(key) { return key; }", "export default function(key) { return 'other'; }"),
    ] {
        assert!(workspace_findings("JavaScript", &[("worker.js", caller), ("helper.js", clean)], &["worker.js"], RECEIVER, true).is_empty());
        assert_key_pending_on_fallthrough(&workspace_findings("JavaScript", &[("worker.js", caller), ("helper.js", wrong)], &["worker.js"], RECEIVER, true));
    }
}

#[test]
fn top_level_arrow_functions_are_checked_as_entry_points() {
    let source = "const pending = (root, key) => { root.register(key); };";
    assert_key_pending_on_fallthrough(&findings("JavaScript", "worker.js", source, RECEIVER, true));
    let clean = source.replace("root.register(key);", "root.register(key); root.remove(key);");
    assert_clean_js(&clean);
}

#[test]
fn nested_go_defer_arguments_are_captured_before_reassignment() {
    let source = r#"package worker
func choose(key string) string { return key }
func pending(root *Registry, key string) {
    root.register(key)
    defer root.remove(choose(key))
    key = "other"
}"#;
    assert!(findings("Go", "worker.go", source, RECEIVER, true).is_empty());
    let wrong = source.replace("choose(key))", "choose(\"other\"))");
    assert_key_pending_on_fallthrough(&findings("Go", "worker.go", &wrong, RECEIVER, true));
}

#[test]
fn nested_throwing_helper_reaches_finally_before_exit() {
    let source = r#"function choose(key, bad) { if (bad) throw new Error(); return key; }
function pending(root, key, bad) {
    root.register(key);
    try { root.remove(choose(key, bad)); }
    finally { root.remove(key); }
}"#;
    assert_clean_js(source);
    let wrong = source.replace("finally { root.remove(key); }", "finally { root.remove('other'); }");
    let hits = findings("JavaScript", "worker.js", &wrong, RECEIVER, true);
    assert_eq!(one(&hits).message, "Clean up root and key before implicit throw");
}

#[test]
fn optional_calls_do_not_unconditionally_execute_nested_cleanup() {
    let source = r#"function cleanup(root, key) { root.remove(key); }
function pending(root, key, maybe) {
    root.register(key);
    maybe?.(cleanup(root, key));
}"#;
    let hits = findings("JavaScript", "worker.js", source, RECEIVER, true);
    assert!(hits.iter().any(|hit| hit.message == "Clean up root and key before fallthrough"), "{hits:?}");
}
