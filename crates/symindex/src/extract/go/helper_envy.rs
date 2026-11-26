use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use super::{line_of, text};
use crate::model::{HelperAccess, HelperLocation, MethodDecl, TypeDecl};

#[derive(Default)]
pub(crate) struct GoHelperEnvy {
    packages: HashMap<(PathBuf, String), Package>,
}

#[derive(Default)]
struct Package {
    declarations: HashMap<String, PackageDecl>,
    callers: Vec<Caller>,
}

enum PackageDecl {
    Struct { file: PathBuf, line: u32 },
    Helper(Helper),
    Unresolved,
}

struct Helper {
    location: HelperLocation,
    params: Vec<Option<LocalType>>,
    accesses: Vec<MemberAccesses>,
    calls: Vec<Call>,
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

struct Forwarded<'a> {
    name: &'a str,
    helper: &'a Helper,
    source_slot: usize,
    slot: usize,
    argument_type: &'a LocalType,
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
                "type_declaration" | "const_declaration" | "var_declaration" => package.add_non_functions(node, source, file),
                "function_declaration" => {
                    let Some(name) = node.child_by_field_name("name") else { continue };
                    let name = text(name, source);
                    if matches!(name, "_" | "init") {
                        continue;
                    }
                    package.add_declaration(name, || extract_helper(node, source, file).map_or(PackageDecl::Unresolved, PackageDecl::Helper));
                }
                "method_declaration" => {
                    package.callers.extend(extract_caller(node, source, types, first_type));
                }
                _ => {}
            }
        }
    }

    pub(crate) fn attach(self, types: &mut [TypeDecl]) {
        for package in self.packages.into_values() {
            for caller in &package.callers {
                let method = &mut types[caller.type_index].methods[caller.method_index];
                method.helper_accesses = package.accesses(caller, method);
            }
        }
    }
}

impl Package {
    fn accesses(&self, caller: &Caller, method: &MethodDecl) -> Vec<HelperAccess> {
        let mut result = BTreeMap::new();
        let mut seen = HashSet::new();
        for first in self.forwarded(&caller.calls, &method.file, &caller.params) {
            if !seen.insert((first.name, first.slot)) {
                continue;
            }
            self.record_access(&first, None, method, &mut result);
            for second in self.forwarded(&first.helper.calls, &first.helper.location.file, &first.helper.params)
                .filter(|second| second.source_slot == first.slot)
            {
                self.record_access(&second, Some(&first.helper.location), method, &mut result);
            }
        }
        result.into_values().collect()
    }

    fn forwarded<'a>(&'a self, calls: &'a [Call], from: &'a Path, params: &'a [Option<LocalType>]) -> impl Iterator<Item = Forwarded<'a>> + 'a {
        calls.iter().filter_map(move |call| Some((call, self.resolve_helper(call, from)?)))
            .flat_map(move |(call, helper)| {
                call.arguments.iter().enumerate().filter_map(move |(slot, argument)| {
                    let source_slot = (*argument)?;
                    let argument_type = params[source_slot].as_ref()?;
                    (helper.params[slot].as_ref() == Some(argument_type)).then_some(Forwarded {
                        name: &call.name,
                        helper,
                        source_slot,
                        slot,
                        argument_type,
                    })
                })
            })
    }

    fn resolve_helper(&self, call: &Call, caller_file: &Path) -> Option<&Helper> {
        let PackageDecl::Helper(helper) = self.declarations.get(&call.name)? else { return None };
        let file = &helper.location.file;
        (file != caller_file && helper.params.len() == call.arguments.len() && visible_from(file, caller_file))
            .then_some(helper)
    }

    fn record_access<'a>(
        &self,
        target: &Forwarded<'a>,
        via: Option<&HelperLocation>,
        method: &MethodDecl,
        result: &mut BTreeMap<(&'a str, usize), HelperAccess>,
    ) {
        let name = &target.argument_type.name;
        let accesses = &target.helper.accesses[target.slot];
        // A wrapper around one collaborator operation is ordinary delegation.
        if name == &method.owner_type || accesses.names.len() < 2 {
            return;
        }
        let Some(PackageDecl::Struct { file: type_file, line: type_line }) = self.declarations.get(name) else { return };
        if !visible_from(type_file, &method.file) {
            return;
        }
        // A helper parameter's accesses count once, even when multiple paths reach it.
        let evidence = result.entry((target.name, target.slot)).or_insert_with(|| HelperAccess {
            receiver_type: name.clone(),
            type_file: type_file.clone(),
            type_line: *type_line,
            locations: vec![target.helper.location.clone()],
            count: accesses.count,
        });
        if let Some(via) = via.filter(|via| !evidence.locations.contains(via)) {
            evidence.locations.push(via.clone());
        }
    }

    fn add_declaration(&mut self, name: &str, declaration: impl FnOnce() -> PackageDecl) {
        self.declarations.entry(name.to_owned()).and_modify(|entry| *entry = PackageDecl::Unresolved).or_insert_with(declaration);
    }

    fn add_non_functions(&mut self, declaration: Node, source: &[u8], file: &Path) {
        let mut cursor = declaration.walk();
        for spec in declaration.named_children(&mut cursor) {
            if spec.kind() == "var_spec_list" {
                self.add_non_functions(spec, source, file);
                continue;
            }
            let is_struct = spec.kind() == "type_spec"
                && spec.child_by_field_name("type").is_some_and(|node| node.kind() == "struct_type")
                && spec.child_by_field_name("type_parameters").is_none();
            let mut names = spec.walk();
            for name in spec.children_by_field_name("name", &mut names).filter(Node::is_named) {
                self.add_declaration(text(name, source), || {
                    if is_struct { PackageDecl::Struct { file: file.to_path_buf(), line: line_of(spec) } } else { PackageDecl::Unresolved }
                });
            }
        }
    }
}

fn is_test_file(file: &Path) -> bool {
    file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with("_test.go"))
}

fn visible_from(declaration: &Path, caller: &Path) -> bool {
    !is_test_file(declaration) || is_test_file(caller)
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

fn extract_caller(node: Node, source: &[u8], types: &[TypeDecl], first_type: usize) -> Option<Caller> {
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
