use std::collections::BTreeSet;

use autoreview_schema::{AgentFinding, Location, LocationRange, Side};
use tree_sitter::Node;

use crate::analyzers::rust_review::syntax::{children, is_comment, is_test_item};

use super::super::MakeFinding;

const MIN_CODE_LINES: usize = 300;

pub(super) fn finding(
    path: &str,
    content: &str,
    module: Node<'_>,
    limit: usize,
    make_finding: &MakeFinding<'_>,
) -> Option<AgentFinding> {
    let scope = match module.kind() {
        "source_file" => module,
        "mod_item" => module.child_by_field_name("body")?,
        _ => return None,
    };
    if module.has_error()
        || path
            .split('/')
            .any(|part| matches!(part, "tests" | "benches" | "fixtures"))
    {
        return None;
    }
    let items = production_items(scope, content);
    if items.len() <= limit {
        return None;
    }
    let lines = code_lines(&items, content);
    if lines.len() < MIN_CODE_LINES {
        return None;
    }
    let first = *lines.first()? + 1;
    let mut finding = make_finding(
        "rust-god-module", path, first as u32, first as u32,
        format!("Large Rust module ({} declarations)", items.len()),
        format!("This module has {} direct declarations and {} code lines. The check's limits are more than {limit} declarations and at least {MIN_CODE_LINES} code lines. Review which declarations belong together.", items.len(), lines.len()),
    );
    finding.related_locations = Some(
        items
            .into_iter()
            .map(|item| declaration_location(path, item))
            .collect(),
    );
    Some(finding)
}

fn declaration_location(path: &str, item: Node<'_>) -> Location {
    Location {
        path: path.to_owned(),
        range: LocationRange {
            start_line: (item.start_position().row + 1) as u32,
            end_line: Some((item.end_position().row + 1) as u32),
            ..LocationRange::default()
        },
        snippet: String::new(),
        side: Side::New,
    }
}

fn production_items<'tree>(scope: Node<'tree>, content: &str) -> Vec<Node<'tree>> {
    let mut items = Vec::new();
    for item in children(scope).filter(|item| !is_test_item(*item, content)) {
        match item.kind() {
            "impl_item" => {
                let methods = item
                    .child_by_field_name("body")
                    .into_iter()
                    .flat_map(children)
                    .filter(|member| {
                        member.kind() == "function_item" && !is_test_item(*member, content)
                    });
                items.extend(methods);
            }
            "function_item" | "struct_item" | "enum_item" | "union_item" | "type_item"
            | "trait_item" | "const_item" | "static_item" => items.push(item),
            _ => {}
        }
    }
    items
}

fn code_lines(items: &[Node<'_>], content: &str) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    let mut pending = items.to_vec();
    while let Some(node) = pending.pop() {
        if is_comment(node)
            || matches!(node.kind(), "attribute_item" | "mod_item")
            || node.kind().ends_with("_item") && is_test_item(node, content)
        {
            continue;
        }
        if node.child_count() == 0 {
            lines.insert(node.start_position().row);
        } else {
            pending.extend(node.children(&mut node.walk()));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_langsupport::{parser_for, Language};

    #[test]
    fn counts_production_items_per_module_with_a_code_line_floor() {
        let statements = "let _value = true;\n".repeat(MIN_CODE_LINES);
        let functions = format!("fn work() {{\n{statements}\n}}\nfn stop() {{}}\n");
        let source = format!(
            "mod wide {{\n{functions}}}\n#[cfg(test)]\nmod tests {{\n{functions}}}\nmod small {{\nfn a() {{}} fn b() {{}}\n}}"
        );
        let findings = crate::analyzers::complexity::detect_complexity_in_file_with_thresholds(
            "src/lib.rs",
            &source,
            crate::analyzers::complexity::ComplexityLanguage::Rust,
            &super::super::ComplexityThresholds {
                rust_god_module: 1,
                ..Default::default()
            },
            &Default::default(),
        );
        let modules: Vec<_> = findings
            .iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some("rust-god-module"))
            .collect();
        assert_eq!(modules.len(), 1, "{modules:?}");
        assert_eq!(modules[0].location.range.start_line, 2);
    }

    #[test]
    fn ignores_comments_and_counts_methods_in_direct_impl_blocks() {
        let comments = "// Long doc.\n".repeat(MIN_CODE_LINES);
        let source = format!("struct Type;\nimpl Type {{ fn a() {{\n{comments}\n}} fn b() {{}} }}");
        let tree = parser_for(Language::Rust)
            .unwrap()
            .parse(&source, None)
            .unwrap();
        let items = production_items(tree.root_node(), &source);
        assert_eq!(items.len(), 3);
        assert!(code_lines(&items, &source).len() < MIN_CODE_LINES);
    }
}
