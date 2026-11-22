mod bindings;

use std::path::Path;

use autoreview_langsupport::Language::{self, Go, Java, JavaScript, Kotlin, Rust, Tsx, TypeScript};
use tree_sitter::Node;

use super::{children, descendants, location, text, web::field_text, AgentFinding, Rule};

const MESSAGE: &str = "This string transformation returns a value that is discarded. Assign, return, or otherwise use the transformed string.";
const BASE: Rule = Rule {
    id: "",
    category: "correctness",
    subcategories: &["correctness.discarded-results"],
    title: "String transformation result is discarded",
    message: MESSAGE,
    reference: "",
};

pub(super) const RULES: &[Rule] = &[
    Rule { id: "go-discarded-string-transformation", reference: "https://pkg.go.dev/strings", ..BASE },
    Rule { id: "java-discarded-string-transformation", reference: "https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/String.html", ..BASE },
    Rule { id: "kotlin-discarded-string-transformation", reference: "https://kotlinlang.org/docs/strings.html#work-with-string-content", ..BASE },
    Rule { id: "javascript-discarded-string-transformation", reference: "https://tc39.es/ecma262/multipage/text-processing.html#sec-properties-of-the-string-prototype-object", ..BASE },
    Rule { id: "typescript-discarded-string-transformation", reference: "https://tc39.es/ecma262/multipage/text-processing.html#sec-properties-of-the-string-prototype-object", ..BASE },
    Rule { id: "rust-discarded-string-transformation", reference: "https://doc.rust-lang.org/std/primitive.str.html", ..BASE },
];

pub(super) fn run(root: &Path, paths: &[String]) -> Vec<AgentFinding> {
    super::run_sources(root, paths, |_| true, inspect)
}

fn inspect(path: &str, source: &str, root: Node<'_>, language: Language) -> Vec<AgentFinding> {
    let analyzer = bindings::Strings::new(language, root, source);
    let rule = &RULES[match language {
        Go => 0,
        Java => 1,
        Kotlin => 2,
        JavaScript => 3,
        TypeScript | Tsx => 4,
        Rust => 5,
    }];
    descendants(root)
        .filter(|node| discarded(*node, language))
        .filter(|node| analyzer.is_transformation(*node))
        .map(|node| rule.finding(location(path, node, source), Vec::new()))
        .collect()
}

fn discarded(call: Node<'_>, language: Language) -> bool {
    if !matches!(call.kind(), "call_expression" | "method_invocation") {
        return false;
    }
    if language == Kotlin {
        return call
            .parent()
            .is_some_and(|parent| kotlin_discarded(call, parent));
    }
    let parent = std::iter::successors(call.parent(), |node| node.parent())
        .find(|node| node.kind() != "parenthesized_expression");
    parent.is_some_and(|node| match language {
        Rust => {
            node.kind() == "expression_statement"
                && node
                    .child(node.child_count().saturating_sub(1) as u32)
                    .is_some_and(|end| end.kind() == ";")
        }
        _ => node.kind() == "expression_statement",
    })
}

fn kotlin_discarded<'t>(mut value: Node<'t>, mut parent: Node<'t>) -> bool {
    loop {
        match parent.kind() {
            "block" | "lambda_literal" if next_statement(value) => return true,
            "block" | "parenthesized_expression" => {}
            "if_expression" if parent.child_by_field_name("condition") != Some(value) => {}
            "function_body" if value.kind() == "block" => return true,
            "for_statement" | "while_statement" | "do_while_statement" => {
                return value.kind() == "block"
            }
            _ => return false,
        }
        value = parent;
        let Some(outer) = parent.parent() else {
            return false;
        };
        parent = outer;
    }
}

fn next_statement(node: Node<'_>) -> bool {
    std::iter::successors(node.next_named_sibling(), |sibling| {
        sibling.next_named_sibling()
    })
    .any(|sibling| !sibling.kind().contains("comment"))
}

fn member<'t, 's>(
    call: Node<'t>,
    source: &'s str,
    language: Language,
) -> Option<(Node<'t>, &'s str)> {
    let (receiver, name) = match language {
        Java => (
            call.child_by_field_name("object")?,
            call.child_by_field_name("name")?,
        ),
        Kotlin => {
            let navigation = call
                .named_child(0)
                .filter(|node| node.kind() == "navigation_expression")?;
            (navigation.named_child(0)?, navigation.named_child(1)?)
        }
        _ => {
            let member = call.child_by_field_name("function")?;
            (
                member
                    .child_by_field_name("object")
                    .or_else(|| member.child_by_field_name("value"))?,
                member
                    .child_by_field_name("property")
                    .or_else(|| member.child_by_field_name("field"))?,
            )
        }
    };
    Some((receiver, text(name, source)))
}

fn transformation(language: Language, name: &str) -> bool {
    let supported = match language {
        Java => "trim strip stripLeading stripTrailing stripIndent replace replaceAll replaceFirst toLowerCase toUpperCase substring concat repeat indent translateEscapes",
        Kotlin => "trim trimStart trimEnd trimMargin trimIndent replace replaceFirst replaceBefore replaceAfter lowercase uppercase removePrefix removeSuffix removeSurrounding substring drop take padStart padEnd repeat",
        Rust => "trim trim_start trim_end trim_matches trim_start_matches trim_end_matches replace replacen to_lowercase to_uppercase to_ascii_lowercase to_ascii_uppercase strip_prefix strip_suffix repeat",
        _ => "trim trimStart trimEnd trimLeft trimRight replace replaceAll toLowerCase toUpperCase toLocaleLowerCase toLocaleUpperCase normalize substring slice concat repeat padStart padEnd",
    };
    supported
        .split_ascii_whitespace()
        .any(|method| name == method)
}

fn unwrap(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "non_null_expression"
    ) {
        let Some(value) = children(node).find(|child| !child.kind().contains("comment")) else {
            break;
        };
        node = value;
    }
    node
}
