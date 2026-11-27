use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::{AgentFinding, Severity};
use tree_sitter::Node;

use super::super::web::node_calls::{property, NodeCalls};
use super::super::{children, descendants, location, Rule};

const MODULES: &[&str] = &["https", "node:https", "tls", "node:tls"];

pub(in crate::analyzers::framework_checks) const JAVASCRIPT_RULE: Rule = Rule {
    id: "javascript-tls-verification-disabled",
    category: "security",
    subcategories: &["transport.tls"],
    title: "Node.js client accepts an unverified TLS peer",
    message: "This Node.js client disables certificate verification. Keep verification enabled and configure a trusted CA for private services.",
    reference: "https://nodejs.org/api/tls.html#tlsconnectoptions-callback",
};

pub(in crate::analyzers::framework_checks) const TYPESCRIPT_RULE: Rule = Rule {
    id: "typescript-tls-verification-disabled",
    ..JAVASCRIPT_RULE
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("rejectUnauthorized") {
        return Vec::new();
    }
    let rule = match language_for_path(path.as_ref()) {
        Some(Language::TypeScript | Language::Tsx) => &TYPESCRIPT_RULE,
        _ => &JAVASCRIPT_RULE,
    };
    let calls = NodeCalls::new(root, source);
    descendants(root)
        .filter_map(|call| {
            let (module, member) = calls.resolve(call, MODULES)?;
            let module = module.trim_start_matches("node:");
            let mut args = children(call.child_by_field_name("arguments")?)
                .filter(|argument| argument.kind() != "comment");
            let first = args.next()?;
            let options = match (module, member, call.kind()) {
                ("https", "Agent", "new_expression") => first,
                ("tls", "connect", "call_expression") => match first.kind() {
                    "number" => {
                        let next = args.next()?;
                        if next.kind() == "string" {
                            args.next()?
                        } else {
                            next
                        }
                    }
                    "string" => args.next()?,
                    _ => first,
                },
                ("https", "get" | "request", "call_expression") => {
                    if first.kind() == "string" {
                        args.next()?
                    } else {
                        first
                    }
                }
                _ => return None,
            };
            let object = calls.options_object(options, call)?;
            let (setting, value) = property(object, "rejectUnauthorized", source)?;
            (value.kind() == "false").then_some(())?;
            let related = (options != object)
                .then(|| location(path, call, source))
                .into_iter()
                .collect();
            let mut finding = rule.finding(location(path, setting, source), related);
            finding.severity = Severity::High;
            if let Some(metadata) = &mut finding.meta {
                metadata.insert("cwe".into(), serde_json::json!(["CWE-295"]));
            }
            Some(finding)
        })
        .collect()
}
