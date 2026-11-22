mod returns;

use returns::{ChangedReturns, changed_returns};

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use autoreview_dataflow::{
    lower::java::lower_function, rules::java_kotlin_npe_risk::check_call_source,
};
use autoreview_langsupport::{Language, parser_for};
use autoreview_schema::{AgentFinding, Location};
use tree_sitter::Node;

use super::{
    Java, children, class_members, descendants,
    hierarchy::{Hierarchy, Lookup, Type},
    parse_source, source_files,
};
use crate::analyzers::framework_checks::Rule;

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-new-nullable-return-breaks-caller",
    category: "correctness",
    subcategories: &["correctness.nullability"],
    title: "New nullable return can break an unchanged Java caller",
    message: "A changed helper can now return null on a path to an unchecked dereference.",
    reference: "https://docs.oracle.com/javase/specs/jls/se21/html/jls-15.html#jls-15.12.4.4",
};

/// Compares the snapshots before finding unchanged callers of changed helpers.
pub fn run(base: &Path, head: &Path, changed: &[String]) -> Vec<AgentFinding> {
    let Some(mut parser) = parser_for(Language::Java) else {
        return Vec::new();
    };
    let returns = changed_returns(base, head, changed, &mut parser);
    if returns.is_empty() {
        return Vec::new();
    }
    let hierarchy = Hierarchy::load(head);
    let changed: HashSet<_> = changed.iter().map(String::as_str).collect();
    let mut findings = Vec::new();
    for file in source_files(head) {
        let path = file
            .strip_prefix(head)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if changed.contains(path.as_str()) {
            continue;
        }
        let Some((source, tree)) = parse_source(&mut parser, &file) else {
            continue;
        };
        let java = Java::new(&path, &source, &tree);
        findings.extend(inspect_file(&java, &hierarchy, &returns));
    }
    findings
}

fn classes(root: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    descendants(root).filter(|node| is_class(*node))
}

fn is_class(node: Node<'_>) -> bool {
    matches!(node.kind(), "class_declaration" | "interface_declaration")
}

fn inspect_file(
    java: &Java<'_>,
    hierarchy: &Hierarchy,
    returns: &ChangedReturns,
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for class in children(java.root).filter(|node| is_class(*node)) {
        let Some(owner) = hierarchy.owner(java, class) else {
            continue;
        };
        let fields: HashSet<_> = class_members(class)
            .filter(|node| node.kind() == "field_declaration")
            .flat_map(children)
            .filter(|node| node.kind() == "variable_declarator")
            .filter_map(|node| java.field(node, "name"))
            .collect();
        for callable in class_members(class).filter(|node| {
            matches!(
                node.kind(),
                "method_declaration" | "constructor_declaration"
            )
        }) {
            let Some(body) = callable.child_by_field_name("body") else {
                continue;
            };
            if supported_caller(java, body) {
                let context = Caller {
                    java,
                    hierarchy,
                    owner,
                    fields: &fields,
                };
                findings.extend(context.inspect(callable, returns));
            }
        }
    }
    findings
}

struct Caller<'a, 's> {
    java: &'a Java<'s>,
    hierarchy: &'a Hierarchy,
    owner: &'a Type,
    fields: &'a HashSet<&'s str>,
}

impl Caller<'_, '_> {
    fn inspect(&self, callable: Node<'_>, returns: &ChangedReturns) -> Vec<AgentFinding> {
        let bindings: HashSet<_> = descendants(callable)
            .filter(|node| {
                matches!(
                    node.kind(),
                    "formal_parameter"
                        | "variable_declarator"
                        | "spread_parameter"
                        | "type_parameter"
                )
            })
            .filter_map(|node| self.java.field(node, "name"))
            .collect();
        let calls: HashMap<_, _> = descendants(callable)
            .filter(|node| node.kind() == "method_invocation")
            .map(|node| ((node.start_byte(), node.end_byte()), node))
            .collect();
        let sources: Vec<_> = calls
            .iter()
            .filter_map(|(&span, &node)| {
                self.resolve(node, &bindings, returns)
                    .map(|source| (span, node, source))
            })
            .collect();
        if sources.is_empty() {
            return Vec::new();
        }
        let cfg = lower_function(self.java.source.as_bytes(), callable);
        let mut findings = Vec::new();
        let mut reported = HashSet::new();
        let mut sources = sources;
        sources.sort_unstable_by_key(|(span, _, _)| *span);
        for (span, source, resolved) in sources {
            for hit in check_call_source(&cfg, span) {
                let Some(&use_site) = hit.use_span.and_then(|span| calls.get(&span)) else {
                    continue;
                };
                let Some(name) = self.java.field(use_site, "name") else {
                    continue;
                };
                if resolved
                    .return_type
                    .is_instance_method(self.hierarchy, name)
                    && reported.insert(hit.use_span)
                {
                    findings.push(make_finding(self.java, use_site, source, &resolved));
                }
            }
        }
        findings
    }

    fn resolve<'a>(
        &'a self,
        call: Node<'_>,
        bindings: &HashSet<&str>,
        returns: &'a ChangedReturns,
    ) -> Option<ResolvedCall<'a>> {
        let class = call.child_by_field_name("object")?;
        if !matches!(class.kind(), "identifier" | "field_access") {
            return None;
        }
        let class = self.java.text(class);
        let qualifier = class.split('.').next()?;
        // Without a classpath, inherited fields can shadow a type qualifier.
        if self.owner.superclass.is_some()
            || !self.owner.interfaces.is_empty()
            || bindings.contains(qualifier)
            || self.fields.contains(qualifier)
        {
            return None;
        }
        let name = self.java.field(call, "name")?;
        let arguments: Vec<_> = children(call.child_by_field_name("arguments")?)
            .filter(|node| !comment(*node))
            .collect();
        let target =
            Lookup::resolve_static(self.hierarchy, self.owner, class, name, arguments.len())?;
        let evidence = returns
            .get(&target.method.location.path)?
            .get(&target.method.start)?;
        let declared = target.method.return_type.as_deref()?;
        let return_type = if self.hierarchy.canonical(target.owner, declared).as_deref()
            == Some("java.lang.String")
        {
            ReturnType::String
        } else {
            ReturnType::Local(self.hierarchy.named(target.owner, declared)?)
        };
        Some(ResolvedCall {
            return_type,
            nullable_return: evidence.nullable_at_call(&arguments)?,
            declaration: &evidence.declaration,
        })
    }
}

struct ResolvedCall<'a> {
    return_type: ReturnType<'a>,
    nullable_return: &'a Location,
    declaration: &'a Location,
}

enum ReturnType<'a> {
    String,
    Local(&'a Type),
}

impl ReturnType<'_> {
    fn is_instance_method(&self, hierarchy: &Hierarchy, name: &str) -> bool {
        let instance = match self {
            Self::String => matches!(
                name,
                "length"
                    | "isEmpty"
                    | "isBlank"
                    | "charAt"
                    | "substring"
                    | "subSequence"
                    | "trim"
                    | "strip"
                    | "stripLeading"
                    | "stripTrailing"
                    | "startsWith"
                    | "endsWith"
                    | "contains"
                    | "indexOf"
                    | "lastIndexOf"
                    | "split"
                    | "replace"
                    | "replaceAll"
                    | "replaceFirst"
                    | "matches"
                    | "toLowerCase"
                    | "toUpperCase"
                    | "equalsIgnoreCase"
                    | "compareTo"
                    | "concat"
                    | "getBytes"
                    | "toCharArray"
                    | "repeat"
            ),
            Self::Local(owner) => Lookup::instance_method_name(hierarchy, owner, name),
        };
        instance
            || matches!(
                name,
                "getClass" | "toString" | "hashCode" | "equals" | "notify" | "notifyAll" | "wait"
            )
    }
}

fn make_finding(
    java: &Java<'_>,
    use_site: Node<'_>,
    source: Node<'_>,
    resolved: &ResolvedCall<'_>,
) -> AgentFinding {
    let related = vec![
        java.location(source),
        resolved.nullable_return.clone(),
        resolved.declaration.clone(),
    ];
    let mut finding = RULE.finding(java.location(use_site), related);
    finding.message = format!(
        "`{}` can now return null. The unchanged caller invokes `{}` on that result along a path without a null check.",
        java.text(source),
        java.field(use_site, "name").unwrap_or("the method"),
    );
    finding
}

fn supported_caller(java: &Java<'_>, body: Node<'_>) -> bool {
    descendants(body).all(|node| match node.kind() {
        "try_statement"
        | "try_with_resources_statement"
        | "switch_expression"
        | "lambda_expression"
        | "class_body"
        | "for_statement"
        | "enhanced_for_statement"
        | "while_statement"
        | "do_statement"
        | "ternary_expression" => false,
        "method_invocation" => !matches!(
            java.field(node, "name"),
            Some("requireNonNull" | "nonNull" | "isNull")
        ),
        "if_statement" => node
            .child_by_field_name("condition")
            .is_some_and(|condition| {
                let raw = java.text(condition);
                !raw.contains("null") || (!raw.contains("&&") && !raw.contains("||"))
            }),
        _ => true,
    })
}

fn comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}
