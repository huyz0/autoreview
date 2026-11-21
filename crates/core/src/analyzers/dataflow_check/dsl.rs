use std::collections::HashMap;

use super::{
    add_java_kotlin_summaries_from_file, call_order_rules, go_imported_package_summaries,
    go_package_summaries, java_kotlin_imported_package_summaries, java_kotlin_package_summaries,
    make_finding, rule_meta, taint_pack_meta, taint_rules, AgentFinding, Cfg, Node, Path,
    ResolvedRulePack, Stmt, SummaryCaches,
};

/// Substitutes `{tainted_arg}`/`{sink_call}` in a rule's YAML `message`
/// template with the actual hit's values — the declarative-rule
/// equivalent of what the three old hand-written Rust closures each did
/// inline.
fn render_taint_message(template: &str, hit: &autoreview_dataflow::taint::TaintHit) -> String {
    template
        .replace("{tainted_arg}", &hit.tainted_arg)
        .replace("{sink_call}", &hit.sink_call)
}

/// Deliberately doesn't name the source (e.g. "an HTTP form field") the
/// way an earlier, Go-only version of this string did — now that taint
/// rules cover multiple languages and source families (HTTP form values,
/// request parameters, ...), a single hardcoded source description would
/// misdescribe whichever rules don't match it. The rule's own `message`
/// (YAML `message:` field) is where a rule-specific, accurate source
/// description belongs.
fn taint_title(hit: &autoreview_dataflow::taint::TaintHit) -> String {
    format!(
        "`{}` reaches `{}` with an unsanitized value from an untrusted source",
        hit.tainted_arg, hit.sink_call
    )
}

/// Runs every `kind: taint` rule declared in `rules-builtin/` or a
/// registered pack (loaded via `taint_rules::load_taint_rules`) whose
/// `language` matches, against one file's already-lowered functions.
/// Adding a new taint rule means adding a new YAML file — this function
/// doesn't change.
/// Interprocedural taint-source summaries for one rule's own `sources`
/// (rule engine roadmap item 6, `RULE_ENGINE_RESEARCH.md`): does a same-
/// file/same-package/imported-package helper have a path where this
/// specific rule's source reaches its own return? Closes a real gap the
/// existing intraprocedural default (taint auto-propagates through an
/// unrecognized call only from an already-tainted *argument*) structurally
/// can't see — a niladic wrapper like `func getUserID() string { return
/// req.FormValue("id") }` has no argument for that default to look at.
/// Only Go, Java, and Kotlin have the same-package/cross-package
/// resolution machinery this needs (see `go_package_summaries`/
/// `java_kotlin_package_summaries` and friends) — JavaScript/TypeScript/
/// TSX return an empty map, which `check_with_summaries` treats exactly
/// like the "no interprocedural info" case it already handled before this
/// change, so their behavior is unaffected.
fn taint_interprocedural_summaries(
    repo_root: &Path,
    path: &str,
    own_content: &str,
    language: &str,
    spec: &autoreview_dataflow::taint::TaintSpec,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let compute_summary =
        |cfg: &Cfg<Stmt>| autoreview_dataflow::taint::compute_source_summary(spec, cfg);
    match language {
        "Go" => {
            let mut summaries =
                go_package_summaries(repo_root, path, None, &compute_summary, rule_id, caches);
            summaries.extend(go_imported_package_summaries(
                repo_root,
                path,
                None,
                &compute_summary,
                rule_id,
                caches,
            ));
            summaries
        }
        "Java" | "Kotlin" => {
            let mut summaries = match autoreview_archgraph::declared_package(own_content) {
                Some(package) => java_kotlin_package_summaries(
                    repo_root,
                    path,
                    &package,
                    &compute_summary,
                    rule_id,
                    caches,
                ),
                None => {
                    let mut own_only = HashMap::new();
                    add_java_kotlin_summaries_from_file(
                        &repo_root.join(path),
                        &mut own_only,
                        &compute_summary,
                        rule_id,
                        caches,
                    );
                    own_only
                }
            };
            summaries.extend(java_kotlin_imported_package_summaries(
                repo_root,
                own_content,
                &compute_summary,
                rule_id,
                caches,
            ));
            summaries
        }
        _ => HashMap::new(),
    }
}

pub(super) fn run_loaded_taint_rules(
    repo_root: &Path,
    path: &str,
    own_content: &str,
    language: &str,
    lowered: &[(Node, Cfg<Stmt>)],
    registered_packs: &[ResolvedRulePack],
    caches: &SummaryCaches,
) -> Vec<AgentFinding> {
    let rules: Vec<_> = taint_rules::load_taint_rules(registered_packs)
        .into_iter()
        .filter(|r| r.language == language)
        .collect();
    if rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for rule in &rules {
        // Computed once per rule (not per function) — every function in
        // this file shares the same same-package/imported-package scope.
        // `rule.id` doubles as the persistent cache's rule key: it's
        // already unique per taint rule, so a niladic-source-wrapper
        // summary computed for one rule never leaks into another's
        // lookup.
        let summaries = taint_interprocedural_summaries(
            repo_root,
            path,
            own_content,
            language,
            &rule.spec,
            &rule.id,
            caches,
        );
        for (_, cfg) in lowered {
            for hit in
                autoreview_dataflow::taint::check_with_summaries(&rule.spec, cfg, Some(&summaries))
            {
                let mut finding = make_finding(
                    &rule.id,
                    &rule.category,
                    rule.severity,
                    path,
                    hit.source_line,
                    taint_title(&hit),
                    render_taint_message(&rule.message, &hit),
                );
                finding.meta = taint_pack_meta(rule);
                findings.push(finding);
            }
        }
    }
    findings
}

/// Renders a `call-sequence` rule's `{trigger_call}` placeholder — the
/// call (or, for a `checkBeforeReturn` hit, the literal text `"return"`)
/// that actually tripped the rule while its obligation was still open.
fn render_call_order_message(
    template: &str,
    hit: &autoreview_dataflow::call_order::CallOrderHit,
) -> String {
    template.replace("{trigger_call}", &hit.trigger_call)
}

/// Runs every `kind: call-sequence` rule (see
/// `autoreview_dataflow::call_order`'s own module docs for what this
/// engine models and why it's a separate primitive from taint) whose
/// `language` matches, against one file's already-lowered functions.
/// Purely intraprocedural — no summaries/caches involved, unlike
/// `run_loaded_taint_rules`, since the obligation this models is meant to
/// resolve within one function.
pub(super) fn run_call_order_rules(
    path: &str,
    language: &str,
    lowered: &[(Node, Cfg<Stmt>)],
    rules: &[call_order_rules::CallOrderRuleDef],
) -> Vec<AgentFinding> {
    let rules: Vec<_> = rules
        .iter()
        .filter(|rule| rule.language == language)
        .collect();
    if rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    let check_go_defers =
        language == "Go" && rules.iter().any(|rule| rule.spec.check_before_return);
    let functions: Vec<_> = lowered
        .iter()
        .map(|(node, cfg)| (cfg, check_go_defers && contains_go_defer(*node)))
        .collect();
    for rule in rules {
        for (cfg, deferred_cleanup) in &functions {
            let hits = call_sequence_hits(cfg, &rule.spec, *deferred_cleanup);
            findings.extend(hits.iter().map(|hit| call_order_finding(path, rule, hit)));
        }
    }
    findings
}

fn call_order_finding(
    path: &str,
    rule: &call_order_rules::CallOrderRuleDef,
    hit: &autoreview_dataflow::call_order::CallOrderHit,
) -> AgentFinding {
    let mut finding = make_finding(
        &rule.id,
        &rule.category,
        rule.severity,
        path,
        hit.source_line,
        format!(
            "`{}` reached with an unresolved `{}` obligation still open",
            hit.trigger_call, rule.id
        ),
        render_call_order_message(&rule.message, hit),
    );
    finding.meta = rule_meta(rule.pack_id.as_ref(), rule.metadata.as_ref());
    finding
}

fn call_sequence_hits(
    cfg: &Cfg<Stmt>,
    spec: &autoreview_dataflow::call_order::CallOrderSpec,
    deferred_cleanup: bool,
) -> Vec<autoreview_dataflow::call_order::CallOrderHit> {
    if !deferred_cleanup || !spec.check_before_return {
        return autoreview_dataflow::call_order::check(spec, cfg);
    }
    // Go can run cleanup after a return statement. Until deferred effects
    // are lowered, continue checking explicit trigger calls in this function.
    let mut calls_only = spec.clone();
    calls_only.check_before_return = false;
    autoreview_dataflow::call_order::check(&calls_only, cfg)
}

fn contains_go_defer(function: Node<'_>) -> bool {
    let mut nodes = vec![function];
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "defer_statement" => return true,
            "func_literal" => continue,
            _ => nodes.extend(node.named_children(&mut node.walk())),
        }
    }
    false
}
