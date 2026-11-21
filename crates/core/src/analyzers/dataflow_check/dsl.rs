use std::collections::HashMap;

use super::taint_rules::TaintRuleDef;
use super::{
    call_order_rules, go_summaries, java_kotlin_summaries, make_finding, rule_meta, AgentFinding,
    Cfg, Node, Path, Stmt, SummaryCaches,
};
use autoreview_dataflow::taint;

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

pub(super) struct TaintFile<'a> {
    pub root: &'a Path,
    pub path: &'a str,
    pub content: &'a str,
    pub language: &'a str,
    pub caches: &'a SummaryCaches<'a>,
}

impl TaintFile<'_> {
    /// Go and JVM helpers can carry a source back through a return value.
    fn summaries(&self, rule: &TaintRuleDef) -> HashMap<String, bool> {
        let compute = |cfg: &Cfg<Stmt>| taint::compute_source_summary(&rule.spec, cfg);
        match self.language {
            "Go" => go_summaries(self.root, self.path, None, &compute, &rule.id, self.caches),
            "Java" | "Kotlin" => java_kotlin_summaries(
                self.root,
                self.path,
                self.content,
                &compute,
                &rule.id,
                self.caches,
            ),
            _ => HashMap::new(),
        }
    }

    pub(super) fn findings(
        &self,
        functions: &[(Node, Cfg<Stmt>)],
        rules: &[TaintRuleDef],
    ) -> Vec<AgentFinding> {
        let mut findings = Vec::new();
        for rule in rules.iter().filter(|rule| rule.language == self.language) {
            let summaries = self.summaries(rule);
            for (_, cfg) in functions {
                let hits = taint::check_with_summaries(&rule.spec, cfg, Some(&summaries));
                findings.extend(hits.iter().map(|hit| taint_finding(self.path, rule, hit)));
            }
        }
        findings
    }
}

fn taint_finding(path: &str, rule: &TaintRuleDef, hit: &taint::TaintHit) -> AgentFinding {
    let mut finding = make_finding(
        &rule.id,
        &rule.category,
        rule.severity,
        path,
        hit.source_line,
        taint_title(hit),
        render_taint_message(&rule.message, hit),
    );
    finding.meta = rule_meta(rule.pack_id.as_ref(), rule.metadata.as_ref());
    finding
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
