use std::collections::HashSet;

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::{children, descendants, location, text, Rule};
use super::flow::{Check, Helpers};
use super::package::{local_names, File};
use super::two_result_call;

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-otel-span-not-ended",
    category: "correctness",
    subcategory: "observability.span-lifecycle",
    title: "OpenTelemetry span can leave the function without ending",
    message: "At least one path leaves this span open. Defer span.End after starting the span, or explicitly transfer the span to the caller.",
    reference: "https://opentelemetry.io/docs/languages/go/instrumentation/#creating-spans",
};

pub(in crate::analyzers::framework_checks) const TRACE_PARENT_RULE: Rule = Rule {
    id: "go-otel-span-discards-parent-context",
    category: "correctness",
    subcategory: "observability.trace-propagation",
    title: "OpenTelemetry span starts from a fresh context",
    message: "This span starts from a fresh context even though the function has a context parameter. Use the existing or derived context when this span belongs to the same trace.",
    reference: "https://opentelemetry.io/docs/languages/go/instrumentation/#creating-spans",
};

pub(super) fn findings(file: &File) -> Vec<AgentFinding> {
    let trace = aliases(file, "go.opentelemetry.io/otel/trace");
    let otel = aliases(file, "go.opentelemetry.io/otel");
    let contexts = aliases(file, "context");
    if trace.is_empty() && otel.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for function in file.functions() {
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let locals = local_names(function, &file.source);
        let scope = Scope {
            file,
            function,
            trace: &trace,
            otel: &otel,
            contexts: &contexts,
            locals: &locals,
        };
        results.extend(scope.findings(body));
    }
    results
}

struct Scope<'a, 'tree> {
    file: &'a File,
    function: Node<'tree>,
    trace: &'a HashSet<&'a str>,
    otel: &'a HashSet<&'a str>,
    contexts: &'a HashSet<&'a str>,
    locals: &'a HashSet<&'a str>,
}

impl Scope<'_, '_> {
    fn findings(&self, body: Node<'_>) -> Vec<AgentFinding> {
        let helpers = Helpers::new();
        let mut findings = Vec::new();
        for assignment in descendants(body).filter(|node| node.kind() == "short_var_declaration") {
            let Some((binding, call)) = self.binding(assignment) else {
                continue;
            };
            let source = &self.file.source;
            if let Some((fresh, parent)) = self.discarded_parent(call, body) {
                findings.push(TRACE_PARENT_RULE.finding(
                    location(&self.file.path, fresh, source),
                    vec![location(&self.file.path, parent, source)],
                ));
            }
            if text(binding, source) == "_" {
                findings.push(RULE.finding(location(&self.file.path, binding, source), Vec::new()));
                continue;
            }
            let mut check = Check::new(
                source,
                text(binding, source),
                Some(assignment),
                &helpers,
                self.locals,
            )
            .for_span();
            if check.check(body) != Some(false) {
                continue;
            }
            if let Some(exit) = check.exit {
                findings.push(RULE.finding(
                    location(&self.file.path, binding, source),
                    vec![location(&self.file.path, exit, source)],
                ));
            }
        }
        findings
    }

    fn binding<'tree>(&self, assignment: Node<'tree>) -> Option<(Node<'tree>, Node<'tree>)> {
        let (binding, call) = two_result_call(assignment)?;
        let (object, method) = selector(call)?;
        (self.text(method) == "Start" && self.tracer(object, assignment)).then_some((binding, call))
    }

    fn discarded_parent<'tree>(&self, call: Node<'tree>, body: Node<'tree>) -> Option<(Node<'tree>, Node<'_>)> {
        let fresh = call.child_by_field_name("arguments")?.named_child(0)?;
        let (package, method) = selector(fresh)?;
        if !self.contexts.contains(self.text(package)) || self.locals.contains(self.text(package))
            || !matches!(self.text(method), "Background" | "TODO")
            || std::iter::successors(call.parent(), |node| node.parent())
                .take_while(|node| *node != body).any(|node| node.kind() == "func_literal")
        {
            return None;
        }
        let body_locals = local_names(body, &self.file.source);
        let mut parameters = self.function.child_by_field_name("parameters").into_iter()
            .flat_map(children)
            .filter(|param| {
                self.field(*param, "type").and_then(|ty| ty.rsplit_once('.'))
                    .is_some_and(|(pkg, ty)| ty == "Context" && self.contexts.contains(pkg))
            })
            .flat_map(children)
            .filter(|name| name.kind() == "identifier" && !body_locals.contains(self.text(*name)));
        let parent = parameters.next()?;
        parameters.next().is_none().then_some((fresh, parent))
    }

    fn tracer(&self, object: Node<'_>, at: Node<'_>) -> bool {
        if object.kind() == "call_expression" {
            return self.otel_tracer_call(object);
        }
        if object.kind() != "identifier" {
            return false;
        }
        let name = self.text(object);
        self.local_tracer(name, at) || self.unshadowed_parameter(name)
    }

    fn unshadowed_parameter(&self, name: &str) -> bool {
        self.typed_parameter(name)
            && self
                .function
                .child_by_field_name("body")
                .is_some_and(|body| !local_names(body, &self.file.source).contains(name))
    }

    fn typed_parameter(&self, name: &str) -> bool {
        self.function
            .child_by_field_name("parameters")
            .into_iter()
            .flat_map(children)
            .any(|param| {
                self.field(param, "type")
                    .and_then(|ty| ty.rsplit_once('.'))
                    .is_some_and(|(pkg, ty)| ty == "Tracer" && self.trace.contains(pkg))
                    && children(param)
                        .any(|node| node.kind() == "identifier" && self.text(node) == name)
            })
    }

    fn local_tracer(&self, name: &str, at: Node<'_>) -> bool {
        let Some(previous) = at
            .prev_named_sibling()
            .filter(|node| node.kind() == "short_var_declaration")
        else {
            return false;
        };
        let Some(left) = previous.child_by_field_name("left") else {
            return false;
        };
        let Some(right) = previous.child_by_field_name("right") else {
            return false;
        };
        left.named_child_count() == 1
            && right.named_child_count() == 1
            && left
                .named_child(0)
                .is_some_and(|part| self.text(part) == name)
            && right
                .named_child(0)
                .is_some_and(|part| self.otel_tracer_call(part))
    }

    fn otel_tracer_call(&self, call: Node<'_>) -> bool {
        selector(call).is_some_and(|(package, method)| {
            package.kind() == "identifier"
                && self.otel.contains(self.text(package))
                && !self.locals.contains(self.text(package))
                && self.text(method) == "Tracer"
        })
    }

    fn text(&self, node: Node<'_>) -> &str {
        text(node, &self.file.source)
    }

    fn field(&self, node: Node<'_>, field: &str) -> Option<&str> {
        node.child_by_field_name(field).map(|part| self.text(part))
    }
}

fn selector(call: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let callee = call.child_by_field_name("function")?;
    Some((
        callee.child_by_field_name("operand")?,
        callee.child_by_field_name("field")?,
    ))
}

pub(super) fn aliases<'a>(file: &'a File, package: &'static str) -> HashSet<&'a str> {
    descendants(file.root())
        .filter(|node| node.kind() == "import_spec")
        .filter(|node| {
            node.child_by_field_name("path")
                .is_some_and(|path| text(path, &file.source).trim_matches(['"', '`']) == package)
        })
        .map(|node| {
            node.child_by_field_name("name").map_or_else(
                || package.rsplit('/').next().unwrap_or(""),
                |alias| text(alias, &file.source),
            )
        })
        .collect()
}
