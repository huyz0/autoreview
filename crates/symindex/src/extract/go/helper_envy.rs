use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use super::{line_of, text};
use crate::model::{HelperAccess, TypeDecl};

#[derive(Default)]
pub(crate) struct GoHelperEnvy {
    packages: HashMap<(PathBuf, String), Package>,
}

#[derive(Default)]
struct Package {
    structs: HashMap<String, Option<(PathBuf, u32)>>,
    helpers: HashMap<String, Option<Helper>>,
    callers: Vec<Caller>,
}

struct Helper {
    file: PathBuf,
    line: u32,
    params: Vec<Option<LocalType>>,
    accesses: Vec<MemberAccesses>,
}

struct Caller {
    type_index: usize,
    method_index: usize,
    params: Vec<Option<LocalType>>,
    calls: Vec<Call>,
}

#[derive(PartialEq, Eq)]
struct LocalType {
    name: String,
    pointer: bool,
}

#[derive(Default)]
struct MemberAccesses {
    names: HashSet<String>,
    count: usize,
}

struct Call {
    name: String,
    arguments: Vec<Option<usize>>,
}

impl GoHelperEnvy {
    pub(crate) fn add_file(&mut self, root: Node, source: &[u8], file: &Path, types: &[TypeDecl], first_type: usize) {
        let mut cursor = root.walk();
        let Some(package_name) = root.named_children(&mut cursor)
            .find(|node| node.kind() == "package_clause")
            .and_then(|node| node.named_child(0)) else { return };
        let key = (file.parent().unwrap_or(Path::new("")).to_path_buf(), text(package_name, source).to_owned());
        let package = self.packages.entry(key).or_default();

        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "type_declaration" => package.add_structs(node, source, file),
                "function_declaration" => {
                    let Some(name) = node.child_by_field_name("name") else { continue };
                    let name = text(name, source);
                    if matches!(name, "_" | "init") {
                        continue;
                    }
                    package.helpers.entry(name.to_owned())
                        .and_modify(|entry| *entry = None)
                        .or_insert_with(|| extract_helper(node, source, file));
                }
                "method_declaration" => {
                    let Some(name) = node.child_by_field_name("name") else { continue };
                    let Some(receiver) = node.child_by_field_name("receiver").and_then(|list| list.named_child(0)) else { continue };
                    let Some(owner) = receiver.child_by_field_name("type").and_then(|node| local_type(node, source)) else { continue };
                    let Some((type_index, type_decl)) = types.iter().enumerate().find(|(_, decl)| decl.name == owner.name) else { continue };
                    let Some(method_index) = type_decl.methods.iter().position(|method| method.start_line == line_of(node) && method.name == text(name, source)) else { continue };
                    let Some((params, mut bindings)) = function_bindings(node, source) else { continue };
                    if let Some(receiver_name) = receiver.child_by_field_name("name") {
                        bindings.insert(text(receiver_name, source), None);
                    }
                    let Some(body) = node.child_by_field_name("body") else { continue };
                    let mut calls = Vec::new();
                    if walk_straight_line(body, source, bindings, |node, bindings| {
                        if let Some(call) = direct_call(node, source, bindings) {
                            calls.push(call);
                        }
                    }).is_some() && !calls.is_empty() {
                        package.callers.push(Caller { type_index: first_type + type_index, method_index, params, calls });
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn attach(self, types: &mut [TypeDecl]) {
        for package in self.packages.into_values() {
            for caller in package.callers {
                let method = &mut types[caller.type_index].methods[caller.method_index];
                let mut seen = HashSet::new();
                for call in &caller.calls {
                    let Some(Some(helper)) = package.helpers.get(&call.name) else { continue };
                    if helper.file == method.file || helper.params.len() != call.arguments.len()
                        || (is_test_file(&helper.file) && !is_test_file(&method.file)) {
                        continue;
                    }
                    for (slot, argument) in call.arguments.iter().enumerate() {
                        let Some(argument_type) = argument.and_then(|i| caller.params[i].as_ref()) else { continue };
                        let accesses = &helper.accesses[slot];
                        // A wrapper around one collaborator operation is ordinary delegation.
                        if argument_type.name == method.owner_type || Some(argument_type) != helper.params[slot].as_ref() || accesses.names.len() < 2 {
                            continue;
                        }
                        let Some(Some((type_file, type_line))) = package.structs.get(&argument_type.name) else { continue };
                        if is_test_file(type_file) && !is_test_file(&method.file) {
                            continue;
                        }
                        if seen.insert((call.name.as_str(), slot)) {
                            method.helper_accesses.push(HelperAccess {
                                receiver_type: argument_type.name.clone(),
                                type_file: type_file.clone(),
                                type_line: *type_line,
                                helper_file: helper.file.clone(),
                                helper_line: helper.line,
                                count: accesses.count,
                            });
                        }
                    }
                }
            }
        }
    }
}

impl Package {
    fn add_structs(&mut self, declaration: Node, source: &[u8], file: &Path) {
        let mut cursor = declaration.walk();
        for spec in declaration.named_children(&mut cursor) {
            let Some(name) = spec.child_by_field_name("name") else { continue };
            let is_struct = spec.kind() == "type_spec"
                && spec.child_by_field_name("type").is_some_and(|node| node.kind() == "struct_type")
                && spec.child_by_field_name("type_parameters").is_none();
            self.structs.entry(text(name, source).to_owned())
                .and_modify(|entry| *entry = None)
                .or_insert_with(|| is_struct.then(|| (file.to_path_buf(), line_of(spec))));
        }
    }
}

fn is_test_file(file: &Path) -> bool {
    file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with("_test.go"))
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

fn extract_helper(node: Node, source: &[u8], file: &Path) -> Option<Helper> {
    let (params, bindings) = function_bindings(node, source)?;
    let mut accesses: Vec<_> = params.iter().map(|_| MemberAccesses::default()).collect();
    walk_straight_line(node.child_by_field_name("body")?, source, bindings, |node, bindings| {
        if node.kind() == "selector_expression" {
            let Some(slot) = node.child_by_field_name("operand").and_then(|operand| bound_param(operand, source, bindings)) else { return };
            let Some(field) = node.child_by_field_name("field") else { return };
            accesses[slot].names.insert(text(field, source).to_owned());
            accesses[slot].count += 1;
        }
    })?;
    Some(Helper { file: file.to_path_buf(), line: line_of(node), params, accesses })
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
        let mut unsafe_binding = false;
        walk(statement, &mut |node| {
            unsafe_binding |= node.kind() == "func_literal" || (node.kind() == "unary_expression"
                && node.child_by_field_name("operator").is_some_and(|operator| operator.kind() == "&"));
        });
        if unsafe_binding {
            return None;
        }
        walk(statement, &mut |node| visit(node, &bindings));
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

fn walk(node: Node, visit: &mut impl FnMut(Node)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, visit);
    }
}
