use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::web::node_calls::{property, NodeCalls};
use super::web::{first_argument, string_flow};
use super::{children, descendants, location, text, Rule};

pub(super) const DOM_RULE: Rule = Rule {
    id: "web-url-input-to-dom-html",
    category: "security",
    subcategories: &["output.html"],
    title: "Browser parses URL-derived text as HTML",
    message: "Use textContent for text. When HTML is required, sanitize the URL-derived value with an HTML sanitizer before writing it.",
    reference: "https://developer.mozilla.org/en-US/docs/Web/API/Element/innerHTML",
};

pub(super) const REACT_RULE: Rule = Rule {
    id: "react-url-input-to-raw-html",
    title: "React renders URL input through dangerouslySetInnerHTML",
    message: "Render the URL-derived value as React text or sanitize it with an HTML sanitizer before using dangerouslySetInnerHTML.",
    reference: "https://react.dev/reference/react-dom/components/common#dangerously-setting-the-inner-html",
    ..DOM_RULE
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("location") {
        return Vec::new();
    }
    let context = Markup {
        source,
        calls: NodeCalls::new(root, source),
    };
    descendants(root)
        .filter_map(|node| {
            let (rule, value) = context.sink(node)?;
            let origin = context.origin(value, 16)?;
            let mut finding = rule.finding(
                location(path, node, source),
                vec![location(path, origin, source)],
            );
            finding
                .meta
                .get_or_insert_with(Default::default)
                .insert("cwe".into(), serde_json::json!(["CWE-79"]));
            Some(finding)
        })
        .collect()
}

struct Markup<'t, 's> {
    source: &'s str,
    calls: NodeCalls<'t, 's>,
}

impl<'t> Markup<'t, '_> {
    fn sink(&self, node: Node<'t>) -> Option<(&'static Rule, Node<'t>)> {
        match node.kind() {
            "assignment_expression" => {
                let (receiver, prop) = self.calls.member(node.child_by_field_name("left")?)?;
                (matches!(prop, "innerHTML" | "outerHTML") && self.dom_element(receiver, 8))
                    .then_some((&DOM_RULE, node.child_by_field_name("right")?))
            }
            "call_expression" => {
                let (receiver, method) =
                    self.calls.member(node.child_by_field_name("function")?)?;
                if method != "insertAdjacentHTML" || !self.dom_element(receiver, 8) {
                    return None;
                }
                let value = children(node.child_by_field_name("arguments")?)
                    .filter(|node| node.kind() != "comment")
                    .take_while(|node| node.kind() != "spread_element")
                    .nth(1)?;
                Some((&DOM_RULE, value))
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                Some((&REACT_RULE, self.react_raw_html(node)?))
            }
            _ => None,
        }
    }

    fn react_raw_html(&self, node: Node<'t>) -> Option<Node<'t>> {
        let tag = node.child_by_field_name("name")?;
        if tag.kind() != "identifier"
            || !text(tag, self.source)
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return None;
        }
        for attribute in children(node).rev() {
            if attribute.kind() == "jsx_expression" {
                return None;
            }
            if attribute.kind() != "jsx_attribute" {
                continue;
            }
            let mut parts = children(attribute).filter(|part| part.kind() != "comment");
            if text(parts.next()?, self.source) != "dangerouslySetInnerHTML" {
                continue;
            }
            let expression = parts.next()?.named_child(0)?;
            let object = self.calls.options_object(expression, attribute)?;
            return Some(property(object, "__html", self.source)?.1);
        }
        None
    }

    fn dom_element(&self, node: Node<'t>, fuel: usize) -> bool {
        if fuel == 0 {
            return false;
        }
        if let Some(value) = self.calls.const_initializer(node) {
            return self.dom_element(value, fuel - 1);
        }
        if node.kind() != "call_expression" {
            return false;
        }
        node.child_by_field_name("function")
            .and_then(|target| self.calls.member(target))
            .is_some_and(|(receiver, method)| {
                self.browser_global(receiver, "document")
                    && matches!(method, "getElementById" | "querySelector" | "createElement")
            })
    }

    fn origin(&self, expression: Node<'t>, fuel: usize) -> Option<Node<'t>> {
        string_flow::origin(
            &self.calls,
            expression,
            fuel,
            &|value, remaining| match value.kind() {
                "member_expression" => self.url_property(value).then_some(value),
                "call_expression" => self.url_parameter(value, remaining),
                _ => None,
            },
        )
    }

    fn url_parameter(&self, expression: Node<'t>, fuel: usize) -> Option<Node<'t>> {
        let target = expression.child_by_field_name("function")?;
        if matches!(
            text(target, self.source),
            "decodeURI" | "decodeURIComponent"
        ) && self.calls.unshadowed(target)
        {
            return self.origin(first_argument(expression)?, fuel);
        }
        let (receiver, "get") = self.calls.member(target)? else {
            return None;
        };
        if receiver.kind() != "new_expression"
            || !self.global(
                receiver.child_by_field_name("constructor")?,
                "URLSearchParams",
            )
        {
            return None;
        }
        self.origin(first_argument(receiver)?, fuel)
    }

    fn url_property(&self, node: Node<'t>) -> bool {
        self.calls.member(node).is_some_and(|(receiver, field)| {
            matches!(field, "search" | "hash" | "href") && self.browser_global(receiver, "location")
        })
    }

    fn browser_global(&self, node: Node<'t>, expected: &str) -> bool {
        self.global(node, expected)
            || self
                .calls
                .member(node)
                .is_some_and(|(receiver, prop)| prop == expected && self.global(receiver, "window"))
    }

    fn global(&self, node: Node<'t>, name: &str) -> bool {
        text(node, self.source) == name && self.calls.unshadowed(node)
    }
}
