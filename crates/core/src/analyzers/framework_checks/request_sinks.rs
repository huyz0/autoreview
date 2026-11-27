use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::web::node_calls::{string_literal, NodeCalls};
use super::web::{first_argument, is_function, string_flow};
use super::{descendants, location, text, Rule};

const SQL: Rule = Rule {
    id: "express-request-to-pg-query-text",
    category: "security",
    subcategories: &["injection.sql"],
    title: "Request data enters node-postgres query text",
    message: "Keep query text fixed and pass request data through node-postgres bind parameters.",
    reference: "https://node-postgres.com/features/queries",
};
const SHELL: Rule = Rule {
    id: "express-request-to-node-exec",
    subcategories: &["injection.command"],
    title: "Node passes request input to a shell command",
    message:
        "Use a fixed executable and pass validated arguments separately through a shell-free API.",
    reference:
        "https://nodejs.org/api/child_process.html#child_processexeccommand-options-callback",
    ..SQL
};
const URL: Rule = Rule {
    id: "express-request-to-axios-destination",
    subcategories: &["network.outbound-destination"],
    title: "Express request selects an Axios destination",
    message: "Use a fixed destination or validate the fully parsed destination against a closed allowlist.",
    reference: "https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html",
    ..SQL
};
pub(super) const RULES: &[&Rule] = &[&SQL, &SHELL, &URL];

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("express")
        || !["pg", "axios", "child_process"]
            .iter()
            .any(|name| source.contains(name))
    {
        return Vec::new();
    }
    let review = Review {
        path,
        source,
        calls: NodeCalls::new(root, source),
    };
    descendants(root)
        .filter_map(|call| review.handler(call))
        .flat_map(|handler| review.inspect_handler(handler))
        .collect()
}

struct Review<'t, 's> {
    source: &'s str,
    path: &'s str,
    calls: NodeCalls<'t, 's>,
}

impl<'t> Review<'t, '_> {
    fn handler(&self, route: Node<'t>) -> Option<Node<'t>> {
        let (receiver, method) = self.calls.member(route.child_by_field_name("function")?)?;
        if route.kind() != "call_expression"
            || !matches!(method, "get" | "post" | "put" | "patch" | "delete" | "all")
        {
            return None;
        }
        let app = self.calls.const_initializer(receiver)?;
        if self.calls.resolve(app, &["express"]) != Some(("express", "default"))
            || app.kind() != "call_expression"
        {
            return None;
        }
        let mut args = string_flow::meaningful_children(route.child_by_field_name("arguments")?);
        string_literal(args.next()?, self.source)?;
        let callback = args.next()?;
        (args.next().is_none()
            && matches!(callback.kind(), "arrow_function" | "function_expression"))
        .then_some(callback)
    }

    fn inspect_handler(&self, handler: Node<'t>) -> Vec<AgentFinding> {
        let Some((request, body)) =
            request_parameter(handler).zip(handler.child_by_field_name("body"))
        else {
            return Vec::new();
        };
        let name = text(request, self.source);
        if descendants(body).any(|node| self.reassigns_request(node, name)) {
            return Vec::new();
        }
        descendants(body)
            .filter_map(|sink| self.inspect_sink(handler, sink, name))
            .collect()
    }

    fn inspect_sink(
        &self,
        handler: Node<'t>,
        sink: Node<'t>,
        request: &str,
    ) -> Option<AgentFinding> {
        if sink.kind() != "call_expression" || enclosing_function(sink) != Some(handler) {
            return None;
        }
        let (rule, cwe) = self.sink(sink)?;
        let argument = first_argument(sink)?;
        if rule.id == URL.id && self.has_fixed_url_guard(sink, argument) {
            return None;
        }
        let origin = string_flow::origin(&self.calls, argument, 16, &|value, _| {
            self.request_source(value, request, handler)
        })?;
        let mut finding = rule.finding(
            location(self.path, sink, self.source),
            vec![location(self.path, origin, self.source)],
        );
        finding
            .meta
            .get_or_insert_with(Default::default)
            .insert("cwe".into(), serde_json::json!([cwe]));
        Some(finding)
    }

    fn sink(&self, call: Node<'t>) -> Option<(&'static Rule, &'static str)> {
        const MODULES: &[&str] = &["node:child_process", "child_process", "axios"];
        if let Some(api) = self.calls.resolve(call, MODULES) {
            return match api {
                ("node:child_process" | "child_process", "exec") => Some((&SHELL, "CWE-78")),
                ("axios", "get" | "post") => Some((&URL, "CWE-918")),
                _ => None,
            };
        }
        let (receiver, "query") = self.calls.member(call.child_by_field_name("function")?)? else {
            return None;
        };
        let constructor = self.calls.const_initializer(receiver).unwrap_or(receiver);
        if constructor.kind() != "new_expression" {
            return None;
        }
        matches!(
            self.calls.resolve(constructor, &["pg"]),
            Some(("pg", "Client" | "Pool"))
        )
        .then_some((&SQL, "CWE-89"))
    }

    fn request_source(&self, node: Node<'t>, request: &str, handler: Node<'t>) -> Option<Node<'t>> {
        let (collection, _) = self.calls.member(node)?;
        let (receiver, name) = self.calls.member(collection)?;
        if matches!(name, "query" | "params" | "body")
            && receiver.kind() == "identifier"
            && text(receiver, self.source) == request
            && self.calls.binding_scope(receiver) == Some(handler)
            && enclosing_function(node) == Some(handler)
        {
            Some(node)
        } else {
            None
        }
    }

    fn reassigns_request(&self, node: Node<'t>, request: &str) -> bool {
        if !matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression"
        ) {
            return false;
        }
        let Some(mut target) = node.child_by_field_name("left") else {
            return false;
        };
        while let Some((receiver, _)) = self.calls.member(target) {
            target = receiver;
        }
        target.kind() == "identifier" && text(target, self.source) == request
    }

    fn has_fixed_url_guard(&self, sink: Node<'t>, argument: Node<'t>) -> bool {
        if self.calls.const_initializer(argument).is_none() {
            return false;
        }
        let Some(previous) = preceding_statement(sink) else {
            return false;
        };
        let Some((condition, reject)) = rejection_guard(previous) else {
            return false;
        };
        let same_value = condition.child_by_field_name("left").is_some_and(|name| {
            name.kind() == "identifier"
                && text(name, self.source) == text(argument, self.source)
                && self.calls.const_initializer(name) == self.calls.const_initializer(argument)
        });
        let fixed_https = condition
            .child_by_field_name("right")
            .and_then(|literal| string_literal(literal, self.source))
            .is_some_and(|url| url.starts_with("https://") && url.len() > "https://".len());
        same_value && fixed_https && reject.kind() == "return_statement"
    }
}

fn request_parameter(handler: Node<'_>) -> Option<Node<'_>> {
    let param = handler.child_by_field_name("parameter").or_else(|| {
        string_flow::meaningful_children(handler.child_by_field_name("parameters")?).next()
    })?;
    let param = if param.kind() == "required_parameter" {
        param.child_by_field_name("pattern")?
    } else {
        param
    };
    (param.kind() == "identifier").then_some(param)
}

fn enclosing_function(node: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(node.parent(), |part| part.parent()).find(|part| {
        is_function(*part)
            || matches!(
                part.kind(),
                "generator_function" | "generator_function_declaration"
            )
    })
}

fn preceding_statement(sink: Node<'_>) -> Option<Node<'_>> {
    let stmt = std::iter::successors(sink.parent(), |part| part.parent())
        .take_while(|part| !is_function(*part))
        .find(|part| {
            part.parent()
                .is_some_and(|parent| parent.kind() == "statement_block")
        })?;
    std::iter::successors(stmt.prev_named_sibling(), |part| part.prev_named_sibling())
        .find(|part| part.kind() != "comment")
}

fn rejection_guard(statement: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if statement.kind() != "if_statement" || statement.child_by_field_name("alternative").is_some()
    {
        return None;
    }
    let condition = statement.child_by_field_name("condition")?;
    let comparison = string_flow::meaningful_children(condition).next()?;
    if comparison.kind() != "binary_expression"
        || comparison.child_by_field_name("operator")?.kind() != "!=="
    {
        return None;
    }
    let mut reject = statement.child_by_field_name("consequence")?;
    if reject.kind() == "statement_block" {
        let mut children = string_flow::meaningful_children(reject);
        reject = children.next()?;
        (children.next().is_none()).then_some(())?;
    }
    Some((comparison, reject))
}
