use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::Node;

use super::super::{line_of, text};
use crate::model::{HelperLocation, TypeDecl};

pub(super) struct Helper {
    pub(super) location: HelperLocation,
    pub(super) params: Vec<Option<LocalType>>,
    accesses: Vec<MemberAccesses>,
    pub(super) calls: Vec<Call>,
}

pub(super) struct Caller {
    pub(super) type_index: usize,
    pub(super) method_index: usize,
    pub(super) params: Vec<Option<LocalType>>,
    pub(super) calls: Vec<Call>,
}

#[derive(PartialEq, Eq)]
pub(super) struct LocalType {
    pub(super) name: String,
    pointer: bool,
}

#[derive(Default)]
struct MemberAccesses {
    names: HashSet<String>,
    count: usize,
}

pub(super) struct Call {
    pub(super) name: String,
    pub(super) arguments: Vec<Option<usize>>,
}

impl Helper {
    pub(super) fn access_count(&self, slot: usize) -> Option<usize> {
        let accesses = &self.accesses[slot];
        // A wrapper around one collaborator operation is ordinary delegation.
        (accesses.names.len() >= 2).then_some(accesses.count)
    }
}

type Bindings<'a> = HashMap<&'a str, Option<usize>>;

fn function_bindings<'a>(node: Node, source: &'a [u8]) -> Option<(Vec<Option<LocalType>>, Bindings<'a>)> {
    if node.has_error() || node.child_by_field_name("type_parameters").is_some() {
        return None;
    }
    let mut params = Vec::new();
    let mut bindings = HashMap::new();
    let parameters = node.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    for param in parameters.named_children(&mut cursor).filter(|node| !node.is_extra()) {
        if param.kind() != "parameter_declaration" {
            return None;
        }
        let param_type = param.child_by_field_name("type")?;
        let mut names = param.walk();
        let mut count = 0;
        for name in param.children_by_field_name("name", &mut names) {
            let name = text(name, source);
            if name != "_" {
                bindings.insert(name, Some(params.len() + count));
            }
            count += 1;
        }
        params.extend((0..count.max(1)).map(|_| local_type(param_type, source)));
    }
    if let Some(results) = node.child_by_field_name("result").filter(|node| node.kind() == "parameter_list") {
        let mut cursor = results.walk();
        for result in results.named_children(&mut cursor) {
            let mut names = result.walk();
            for name in result.children_by_field_name("name", &mut names) {
                bindings.insert(text(name, source), None);
            }
        }
    }
    Some((params, bindings))
}

fn local_type(node: Node, source: &[u8]) -> Option<LocalType> {
    let pointer = node.kind() == "pointer_type";
    let name = if pointer { node.named_child(0)? } else { node };
    (name.kind() == "type_identifier").then(|| LocalType { name: text(name, source).to_owned(), pointer })
}

pub(super) fn extract_helper(node: Node, source: &[u8], file: &Path) -> Option<Helper> {
    let (params, bindings) = function_bindings(node, source)?;
    let mut accesses: Vec<_> = params.iter().map(|_| MemberAccesses::default()).collect();
    let mut calls = Vec::new();
    walk_straight_line(node.child_by_field_name("body")?, source, bindings, |expression, bindings| {
        if let Some((slot, member)) = parameter_member(expression, source, bindings) {
            accesses[slot].names.insert(member.to_owned());
            accesses[slot].count += 1;
        }
        calls.extend(direct_call(expression, source, bindings));
    })?;
    let location = HelperLocation { file: file.to_path_buf(), start_line: line_of(node), end_line: node.end_position().row as u32 + 1 };
    Some(Helper { location, params, accesses, calls })
}

fn parameter_member<'a>(node: Node, source: &'a [u8], bindings: &Bindings<'_>) -> Option<(usize, &'a str)> {
    if node.kind() != "selector_expression" {
        return None;
    }
    let slot = bound_param(node.child_by_field_name("operand")?, source, bindings)?;
    let member = text(node.child_by_field_name("field")?, source);
    Some((slot, member))
}

pub(super) fn extract_caller(node: Node, source: &[u8], types: &[TypeDecl], first_type: usize) -> Option<Caller> {
    let receiver = node.child_by_field_name("receiver")?.named_child(0)?;
    let owner = local_type(receiver.child_by_field_name("type")?, source)?;
    let (type_index, type_decl) = types.iter().enumerate().find(|(_, decl)| decl.name == owner.name)?;
    let name = text(node.child_by_field_name("name")?, source);
    let method_index = type_decl.methods.iter().position(|method| method.start_line == line_of(node) && method.name == name)?;
    let (params, mut bindings) = function_bindings(node, source)?;
    if let Some(receiver_name) = receiver.child_by_field_name("name") {
        bindings.insert(text(receiver_name, source), None);
    }
    let mut calls = Vec::new();
    walk_straight_line(node.child_by_field_name("body")?, source, bindings, |node, bindings| {
        calls.extend(direct_call(node, source, bindings));
    })?;
    (!calls.is_empty()).then_some(Caller { type_index: first_type + type_index, method_index, params, calls })
}

fn direct_call(node: Node, source: &[u8], bindings: &Bindings<'_>) -> Option<Call> {
    if node.kind() != "call_expression" || node.child_by_field_name("type_arguments").is_some() {
        return None;
    }
    let function = node.child_by_field_name("function").filter(|node| node.kind() == "identifier")?;
    let name = text(function, source);
    if bindings.contains_key(name) {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let arguments: Vec<_> = arguments.named_children(&mut cursor).filter(|node| !node.is_extra()).collect();
    if arguments.iter().any(|node| node.kind() == "variadic_argument") {
        return None;
    }
    let arguments: Vec<_> = arguments.into_iter().map(|node| bound_param(node, source, bindings)).collect();
    arguments.iter().any(Option::is_some).then(|| Call { name: name.to_owned(), arguments })
}

fn bound_param(node: Node, source: &[u8], bindings: &Bindings<'_>) -> Option<usize> {
    if node.kind() != "identifier" {
        return None;
    }
    bindings.get(text(node, source)).copied().flatten()
}

/// Reject bodies where control flow, reassignment, or an address escape
/// could change which parameter a name refers to.
fn walk_straight_line<'a>(body: Node, source: &'a [u8], mut bindings: Bindings<'a>, mut visit: impl FnMut(Node, &Bindings<'a>)) -> Option<()> {
    let mut block = body.walk();
    let Some(statements) = body.named_children(&mut block).find(|node| node.kind() == "statement_list") else { return Some(()) };
    let mut cursor = statements.walk();
    let mut returned = false;
    for statement in statements.named_children(&mut cursor).filter(|node| !node.is_extra()) {
        if returned {
            return None;
        }
        let binding = match statement.kind() {
            "return_statement" | "expression_statement" => None,
            "short_var_declaration" => {
                let left = single_expression(statement, "left").filter(|node| node.kind() == "identifier")?;
                let right = single_expression(statement, "right")?;
                let name = text(left, source);
                if bindings.contains_key(name) {
                    return None;
                }
                Some((name, bound_param(right, source, &bindings)))
            }
            _ => return None,
        };
        walk(statement, &mut |node| {
            if node.kind() == "func_literal" || (node.kind() == "unary_expression"
                && node.child_by_field_name("operator").is_some_and(|operator| operator.kind() == "&")) {
                return None;
            }
            visit(node, &bindings);
            Some(())
        })?;
        bindings.extend(binding);
        returned = statement.kind() == "return_statement";
    }
    Some(())
}

fn single_expression<'tree>(statement: Node<'tree>, field: &str) -> Option<Node<'tree>> {
    let list = statement.child_by_field_name(field)?;
    let mut cursor = list.walk();
    let mut expressions = list.named_children(&mut cursor).filter(|node| !node.is_extra());
    let expression = expressions.next()?;
    expressions.next().is_none().then_some(expression)
}

fn walk(node: Node, visit: &mut impl FnMut(Node) -> Option<()>) -> Option<()> {
    visit(node)?;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, visit)?;
    }
    Some(())
}
