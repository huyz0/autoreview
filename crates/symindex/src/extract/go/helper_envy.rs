use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use super::{attach_method, collect_structs, extract_method, line_of, text};
use crate::model::{HelperAccess, HelperLocation, MethodDecl, TypeDecl};

mod body;
use body::{extract_caller, extract_helper, Call, Caller, Helper, LocalType};

#[derive(Default)]
pub(crate) struct GoPackageIndex {
    packages: HashMap<(PathBuf, String), Package>,
}

#[derive(Default)]
struct Package {
    declarations: HashMap<String, PackageDecl>,
    methods: Vec<(MethodDecl, Option<Caller>)>,
}

enum PackageDecl {
    Struct { file: PathBuf, line: u32, index: usize, generic: bool },
    Helper(Helper),
    Unresolved,
}

struct Forwarded<'a> {
    name: &'a str,
    helper: &'a Helper,
    source_slot: usize,
    slot: usize,
    argument_type: &'a LocalType,
}

impl GoPackageIndex {
    pub(crate) fn add_file(&mut self, root: Node, source: &[u8], file: &Path, types: &mut Vec<TypeDecl>) {
        let mut struct_indices = HashMap::new();
        collect_structs(root, source, file, &mut |spec, decl| {
            struct_indices.insert(spec.id(), types.len());
            types.push(decl);
        });
        let mut cursor = root.walk();
        let Some(package_name) = root.named_children(&mut cursor)
            .find(|node| node.kind() == "package_clause")
            .and_then(|node| node.named_child(0)) else { return };
        let key = (file.parent().unwrap_or(Path::new("")).to_path_buf(), text(package_name, source).to_owned());
        let package = self.packages.entry(key).or_default();

        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "type_declaration" | "const_declaration" | "var_declaration" => package.add_non_functions(node, source, file, &struct_indices),
                "function_declaration" => {
                    let Some(name) = node.child_by_field_name("name") else { continue };
                    let name = text(name, source);
                    if matches!(name, "_" | "init") {
                        continue;
                    }
                    package.add_declaration(name, || extract_helper(node, source, file).map_or(PackageDecl::Unresolved, PackageDecl::Helper));
                }
                "method_declaration" => {
                    if let Some(method) = extract_method(node, source, file) {
                        package.methods.push((method, extract_caller(node, source)));
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn attach(self, types: &mut [TypeDecl]) {
        for mut package in self.packages.into_values() {
            for (mut method, caller) in std::mem::take(&mut package.methods) {
                let Some(PackageDecl::Struct { file, index, .. }) = package.declarations.get(&method.owner_type) else { continue };
                if !visible_from(file, &method.file) {
                    continue;
                }
                if let Some(caller) = caller {
                    method.helper_accesses = package.accesses(&caller, &method);
                }
                attach_method(&mut types[*index], method);
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
            first.record_access(&self.declarations, None, method, &mut result);
            for second in self.forwarded(&first.helper.calls, &first.helper.location.file, &first.helper.params)
                .filter(|second| second.source_slot == first.slot)
            {
                second.record_access(&self.declarations, Some(&first.helper.location), method, &mut result);
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

    fn add_declaration(&mut self, name: &str, declaration: impl FnOnce() -> PackageDecl) {
        self.declarations.entry(name.to_owned()).and_modify(|entry| *entry = PackageDecl::Unresolved).or_insert_with(declaration);
    }

    fn add_non_functions(&mut self, declaration: Node, source: &[u8], file: &Path, struct_indices: &HashMap<usize, usize>) {
        let mut cursor = declaration.walk();
        for spec in declaration.named_children(&mut cursor) {
            if spec.kind() == "var_spec_list" {
                self.add_non_functions(spec, source, file, struct_indices);
                continue;
            }
            let mut names = spec.walk();
            for name in spec.children_by_field_name("name", &mut names).filter(Node::is_named) {
                self.add_declaration(text(name, source), || match struct_indices.get(&spec.id()) {
                    Some(&index) => PackageDecl::Struct {
                        file: file.to_path_buf(),
                        line: line_of(spec),
                        index,
                        generic: spec.child_by_field_name("type_parameters").is_some(),
                    },
                    None => PackageDecl::Unresolved,
                });
            }
        }
    }
}

impl<'a> Forwarded<'a> {
    fn record_access(
        &self,
        declarations: &HashMap<String, PackageDecl>,
        via: Option<&HelperLocation>,
        method: &MethodDecl,
        result: &mut BTreeMap<(&'a str, usize), HelperAccess>,
    ) {
        let name = &self.argument_type.name;
        let Some(count) = self.helper.access_count(self.slot) else { return };
        let Some(PackageDecl::Struct { file: type_file, line: type_line, generic: false, .. }) = declarations.get(name) else { return };
        if name == &method.owner_type || !visible_from(type_file, &method.file) {
            return;
        }
        // A helper parameter's accesses count once, even when multiple paths reach it.
        let evidence = result.entry((self.name, self.slot)).or_insert_with(|| HelperAccess {
            receiver_type: name.clone(),
            type_file: type_file.clone(),
            type_line: *type_line,
            locations: vec![self.helper.location.clone()],
            count,
        });
        if let Some(via) = via.filter(|via| !evidence.locations.contains(via)) {
            evidence.locations.push(via.clone());
        }
    }
}

fn is_test_file(file: &Path) -> bool {
    file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with("_test.go"))
}

fn visible_from(declaration: &Path, caller: &Path) -> bool {
    !is_test_file(declaration) || is_test_file(caller)
}
