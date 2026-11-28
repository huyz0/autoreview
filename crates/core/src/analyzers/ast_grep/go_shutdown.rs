use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::{Node, Point};

use crate::analyzers::rust_review::syntax::descendants;

mod contract;

const RULE: &str = "go-checked-error-returned-nil";
const RATE: &str = "\"github.com/cilium/cilium/pkg/rate\"";

pub(super) fn filter(root: &Path, findings: &mut Vec<AgentFinding>) {
    let mut contract = None;
    findings.retain(|finding| {
        !is_candidate(finding)
            || !context_shutdown(root, finding)
            || !*contract.get_or_insert_with(|| contract::audited_limiter(root))
    });
}

fn is_candidate(finding: &AgentFinding) -> bool {
    finding.source.rule_id.as_deref() == Some(RULE)
        && !finding
            .meta
            .as_ref()
            .is_some_and(|meta| meta.contains_key("rulePackId"))
}

fn context_shutdown(root: &Path, finding: &AgentFinding) -> bool {
    let range = &finding.location.range;
    let location = Point::new(
        range.start_line.saturating_sub(1) as usize,
        range.start_col.unwrap_or(1).saturating_sub(1) as usize,
    );
    std::fs::read_to_string(root.join(&finding.location.path))
        .is_ok_and(|source| proves_shutdown(&source, location).is_some())
}

fn proves_shutdown(source: &str, location: Point) -> Option<()> {
    let tree = parser_for(Language::Go)?.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let start = root.descendant_for_point_range(location, location)?;
    let branch = ancestors(start)
        .find(|node| node.kind() == "if_statement" && node.start_position() == location)?;
    let rate = import_name(root, source, RATE, "rate")?;
    let context = import_name(root, source, "\"context\"", "context")?;
    matches_shutdown(branch, source, rate, context)
}

fn matches_shutdown(branch: Node<'_>, source: &str, rate: &str, context: &str) -> Option<()> {
    let (_, call) = short_declaration(branch.child_by_field_name("initializer")?)?;
    let (receiver, arguments) = selector_call(call, "Wait", source)?;
    let [ctx] = arguments.as_slice() else {
        return None;
    };
    if receiver.kind() != "identifier" || ctx.kind() != "identifier" {
        return None;
    }
    let function = ancestors(branch).skip(1).find(|node| {
        matches!(
            node.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        )
    })?;
    let formal = context_parameter(function, text(*ctx, source), context, source)?;
    let shutdown = previous_statement(branch)?;
    if !returns_on_done(shutdown, text(*ctx, source), source) {
        return None;
    }
    let constructor = limiter_constructor(function, branch, text(receiver, source), rate, source)?;
    stable_bindings(
        function,
        source,
        [constructor, receiver],
        [formal, *ctx],
        rate,
    )
    .then_some(())
}

fn context_parameter<'a>(
    function: Node<'a>,
    name: &str,
    context: &str,
    source: &str,
) -> Option<Node<'a>> {
    let parameters = function.child_by_field_name("parameters")?;
    children(parameters).into_iter().find_map(|parameter| {
        let ty = parameter.child_by_field_name("type")?;
        if ty.kind() != "qualified_type" || text(ty, source) != format!("{context}.Context") {
            return None;
        }
        children(parameter)
            .into_iter()
            .find(|node| node.kind() == "identifier" && text(*node, source) == name)
    })
}

fn returns_on_done(select: Node<'_>, ctx: &str, source: &str) -> bool {
    select.kind() == "select_statement"
        && children(select).into_iter().any(|case| {
            let Some(receive) = case.child_by_field_name("communication") else {
                return false;
            };
            let Some(channel) = receive.child_by_field_name("right") else {
                return false;
            };
            let Some(body) = children(case)
                .into_iter()
                .find(|node| node.kind() == "statement_list")
            else {
                return false;
            };
            receive.kind() == "receive_statement"
                && receive.child_by_field_name("left").is_none()
                && text(channel, source).split_whitespace().collect::<String>()
                    == format!("<-{ctx}.Done()")
                && sole_child(body).is_some_and(|stmt| {
                    stmt.kind() == "return_statement"
                        && text(stmt, source).split_whitespace().eq(["return", "nil"])
                })
        })
}

fn limiter_constructor<'a>(
    function: Node<'a>,
    branch: Node<'a>,
    name: &str,
    rate: &str,
    source: &str,
) -> Option<Node<'a>> {
    let body = sole_child(function.child_by_field_name("body")?)?;
    children(body).into_iter().take_while(|node| node.start_byte() < branch.start_byte()).find_map(|node| {
        let (binding, call) = short_declaration(node)?;
        let (package, args) = selector_call(call, "NewLimiter", source)?;
        (binding.kind() == "identifier" && text(binding, source) == name &&
            package.kind() == "identifier" && text(package, source) == rate &&
            matches!(args.as_slice(), [_, burst] if burst.kind() == "int_literal" && text(*burst, source) == "1"))
            .then_some(binding)
    })
}

fn stable_bindings(
    function: Node<'_>,
    source: &str,
    limiter: [Node<'_>; 2],
    context: [Node<'_>; 2],
    rate: &str,
) -> bool {
    // Reject every unmodeled use of these names. This also rejects shadowing
    // and receiver escapes without having to resolve arbitrary aliases.
    let mut identifiers = descendants(function).filter(|node| {
        matches!(
            node.kind(),
            "identifier" | "type_identifier" | "package_identifier"
        )
    });
    let mut rate_uses = 0;
    let safe = identifiers.all(|node| {
        let name = text(node, source);
        if name == rate {
            rate_uses += 1;
        }
        if name == text(limiter[0], source) {
            return limiter.contains(&node) || deferred_stop(node, source);
        }
        if name == text(context[0], source) {
            return context.contains(&node) || selector_use(node, "Done", source).is_some();
        }
        true
    });
    safe && rate_uses == 1
}

fn deferred_stop(node: Node<'_>, source: &str) -> bool {
    selector_use(node, "Stop", source)
        .and_then(|call| call.parent())
        .is_some_and(|parent| parent.kind() == "defer_statement")
}

fn selector_use<'a>(node: Node<'a>, method: &str, source: &str) -> Option<Node<'a>> {
    let selector = node.parent()?;
    let call = selector.parent()?;
    let (receiver, args) = selector_call(call, method, source)?;
    (receiver == node && args.is_empty()).then_some(call)
}

fn selector_call<'a>(
    call: Node<'a>,
    method: &str,
    source: &str,
) -> Option<(Node<'a>, Vec<Node<'a>>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    if callee.kind() != "selector_expression"
        || text(callee.child_by_field_name("field")?, source) != method
    {
        return None;
    }
    Some((
        callee.child_by_field_name("operand")?,
        children(call.child_by_field_name("arguments")?),
    ))
}

fn short_declaration(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if node.kind() != "short_var_declaration" {
        return None;
    }
    Some((
        sole_child(node.child_by_field_name("left")?)?,
        sole_child(node.child_by_field_name("right")?)?,
    ))
}

fn import_name<'a>(
    root: Node<'_>,
    source: &'a str,
    path: &str,
    default: &'a str,
) -> Option<&'a str> {
    let mut imports = children(root)
        .into_iter()
        .filter(|node| node.kind() == "import_declaration")
        .flat_map(children)
        .flat_map(|item| {
            if item.kind() == "import_spec_list" {
                children(item)
            } else {
                vec![item]
            }
        });
    let spec = imports.find(|node| {
        node.child_by_field_name("path")
            .is_some_and(|node| text(node, source) == path)
    })?;
    match spec.child_by_field_name("name") {
        None => Some(default),
        Some(name) if name.kind() == "package_identifier" => Some(text(name, source)),
        _ => None,
    }
}

fn ancestors(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(Some(node), Node::parent)
}

fn previous_statement(node: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(node.prev_named_sibling(), Node::prev_named_sibling)
        .find(|sibling| sibling.kind() != "comment")
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk())
        .filter(|child| child.kind() != "comment")
        .collect()
}

fn sole_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut items = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment");
    let child = items.next()?;
    items.next().is_none().then_some(child)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHUTDOWN: &str = r#"package worker
import (
    "context"
    budget "github.com/cilium/cilium/pkg/rate"
)
func run(ctx context.Context, updates <-chan int) error {
    limiter := budget.NewLimiter(1000, 1)
    defer limiter.Stop()
    for {
        select { case <-ctx.Done(): return nil; case <-updates: }
        if err := limiter.Wait(ctx); err != nil { return nil }
    }
}
"#;

    fn recognizes(source: &str) -> bool {
        let (prefix, _) = source.split_once("if err :=").unwrap();
        let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let column = prefix.rsplit('\n').next().unwrap().len();
        proves_shutdown(source, Point::new(row, column)).is_some()
    }

    #[test]
    fn recognizes_a_local_one_token_limiter_at_context_shutdown() {
        assert!(recognizes(SHUTDOWN));
    }

    #[test]
    fn retains_findings_for_unknown_errors_or_bindings() {
        for (original, changed) in [
            (
                "github.com/cilium/cilium/pkg/rate",
                "example.com/worker/rate",
            ),
            ("1000, 1", "1000, 0"),
            ("limiter.Wait(ctx)", "limiter.WaitN(ctx, 2)"),
            ("<-ctx.Done()", "<-other.Done()"),
            ("<-ctx.Done(): return nil", "<-ctx.Done(): return ctx.Err()"),
            ("defer limiter.Stop()", "limiter = anotherLimiter()"),
            ("defer limiter.Stop()", "alias := limiter; use(alias)"),
            ("defer limiter.Stop()", "ctx = anotherContext()"),
            ("for {", "for {\n        limiter := anotherLimiter()"),
            (
                "limiter := budget",
                "budget := replacement; limiter := budget",
            ),
            (
                "limiter := budget",
                "type budget = replacement; limiter := budget",
            ),
        ] {
            assert!(
                !recognizes(&SHUTDOWN.replace(original, changed)),
                "{changed}"
            );
        }
    }
}
