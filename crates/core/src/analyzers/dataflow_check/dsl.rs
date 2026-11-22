use std::collections::HashMap;

use super::taint_rules::TaintRuleDef;
use super::{
    call_order_rules, make_finding, rule_meta, AgentFinding, Cfg, Node, Severity, Stmt,
    SummarySource,
};
use autoreview_dataflow::taint;
use autoreview_langsupport::Language;
use autoreview_schema::{Location, LocationRange, Side};

/// The reporting and dispatch fields shared by the CFG rule families.
pub struct DataflowRuleDef<Spec> {
    pub id: String,
    pub language: String,
    pub category: String,
    pub severity: Severity,
    pub message: String,
    pub spec: Spec,
    pub pack_id: Option<String>,
    pub metadata: Option<super::super::ast_grep::RuleMetadataBlock>,
}

impl<Spec> DataflowRuleDef<Spec> {
    fn finding(&self, path: &str, line: u32, title: String, message: String) -> AgentFinding {
        let mut finding = make_finding(
            &self.id,
            &self.category,
            self.severity,
            path,
            line,
            title,
            message,
        );
        finding.meta = rule_meta(self.pack_id.as_ref(), self.metadata.as_ref());
        finding
    }
}

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

pub(super) fn run_taint_rules(
    file: &SummarySource,
    language: Language,
    functions: &[(Node, Cfg<Stmt>)],
    rules: &[TaintRuleDef],
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for rule in rules.iter().filter(|rule| {
        Language::from_rule_name(&rule.language)
            .is_some_and(|declared| language.matches_rule_language(declared))
    }) {
        let summaries = taint_summaries(file, language, rule);
        for (_, cfg) in functions {
            let hits = taint::check_with_summaries(&rule.spec, cfg, Some(&summaries));
            findings.extend(hits.into_iter().map(|mut hit| {
                if language == Language::Rust {
                    hit.tainted_arg
                        .truncate(hit.tainted_arg.rfind('#').unwrap_or(hit.tainted_arg.len()));
                }
                taint_finding(file.path, rule, &hit)
            }));
        }
    }
    findings
}

fn taint_summaries(
    file: &SummarySource,
    language: Language,
    rule: &TaintRuleDef,
) -> HashMap<String, bool> {
    let compute = |cfg: &Cfg<Stmt>| taint::compute_source_summary(&rule.spec, cfg);
    match language {
        Language::Go => file.go_summaries(None, &compute, &rule.id),
        Language::Java | Language::Kotlin => file.java_kotlin_summaries(&compute, &rule.id),
        _ => HashMap::new(),
    }
}

fn taint_finding(path: &str, rule: &TaintRuleDef, hit: &taint::TaintHit) -> AgentFinding {
    rule.finding(
        path,
        hit.source_line,
        taint_title(hit),
        render_taint_message(&rule.message, hit),
    )
}

/// Renders a `call-sequence` rule's `{trigger_call}` placeholder — the
/// call (or, for a `checkBeforeReturn` hit, the literal text `"return"`)
/// that actually tripped the rule while its obligation was still open.
fn render_call_order_message(
    template: &str,
    hit: &autoreview_dataflow::call_order::CallOrderHit,
) -> String {
    let message = template.replace("{trigger_call}", &hit.trigger_call);
    match &hit.entry {
        Some(entry) => message.replace("{collection}", &entry.collection).replace("{key}", &entry.key).replace("{handle}", &entry.collection),
        None => message,
    }
}

/// Runs the function-wide call-sequence rules. Keyed rules are dispatched
/// separately so their repository-local call graph can span source files.
pub(super) fn run_call_order_rules(
    file: &SummarySource,
    language: Language,
    lowered: &[(Node, Cfg<Stmt>)],
    rules: &[call_order_rules::CallOrderRuleDef],
) -> Vec<AgentFinding> {
    let rules = rules.iter().filter(|rule| !rule.spec.uses_path_engine() &&
        Language::from_rule_name(&rule.language)
            .is_some_and(|declared| language.matches_rule_language(declared)));
    let mut findings = Vec::new();
    for rule in rules {
        for (node, cfg) in lowered {
            let deferred = language == Language::Go && rule.spec.check_before_return && contains_go_defer(*node);
            let hits = call_sequence_hits(cfg, &rule.spec, deferred);
            findings.extend(hits.iter().map(|hit| call_order_finding(file.path, Some(file.content), rule, hit)));
        }
    }
    findings
}

pub(super) fn call_order_finding(
    path: &str,
    registration_source: Option<&str>,
    rule: &call_order_rules::CallOrderRuleDef,
    hit: &autoreview_dataflow::call_order::CallOrderHit,
) -> AgentFinding {
    let title = match &hit.entry {
        Some(entry) if rule.spec.contract.is_some() => format!(
            "`{}` leaves the `{}` handle incomplete", hit.trigger_call, entry.collection
        ),
        None if rule.spec.contract.is_some() => format!("Unused result of `{}`", hit.trigger_call),
        Some(entry) => format!(
            "`{}` reached with key `{}` still pending in `{}` (registered at line {})",
            hit.trigger_call, entry.key, entry.collection, entry.registration_line
        ),
        None => format!(
            "`{}` reached with an unresolved `{}` obligation still open",
            hit.trigger_call, rule.id
        ),
    };
    let mut finding = rule.finding(
        hit.path.as_deref().unwrap_or(path),
        hit.source_line,
        title,
        render_call_order_message(&rule.message, hit),
    );
    if let Some(entry) = &hit.entry {
        finding.related_locations = Some(vec![Location {
            path: entry.registration_path.as_deref().unwrap_or(path).to_owned(),
            range: LocationRange { start_line: entry.registration_line, ..Default::default() },
            snippet: registration_source.and_then(|source| source.lines().nth(entry.registration_line.saturating_sub(1) as usize)).unwrap_or_default().trim().to_owned(),
            side: Side::New,
        }]);
    }
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
